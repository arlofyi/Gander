// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask check-elf-alignment <apk|aab|dir|.so>...`: the Android 16 KB page-size check.
//! A directory is scanned recursively for `.so`, `.apk` and `.aab` files.
//!
//! Android 15+ devices may use 16 KB memory pages, and Google Play requires apps that ship
//! native code for 64-bit ABIs to support them. Two things must hold:
//!
//! 1. every `PT_LOAD` segment of every 64-bit shared library has `p_align >= 16384`;
//! 2. inside an APK, a library stored uncompressed starts on a 16 KB boundary (`zipalign -P 16`).
//!    A deflated library is extracted at install time and needs no zip alignment.
//!
//! 32-bit ABIs are checked and reported but never fail the run (the requirement does not
//! apply to them). The checker is plain Rust so it works on Windows and Linux CI alike.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use crate::proc::display_path;

const PAGE_16K: u64 = 16 * 1024;
const PT_LOAD: u32 = 1;

/// Result of parsing one ELF image.
#[derive(Debug, PartialEq, Eq)]
pub struct ElfInfo {
    pub is_64bit: bool,
    /// `p_align` of every `PT_LOAD` segment.
    pub load_aligns: Vec<u64>,
}

impl ElfInfo {
    pub fn min_load_align(&self) -> Option<u64> {
        self.load_aligns.iter().copied().min()
    }

    pub fn is_16k_aligned(&self) -> bool {
        !self.load_aligns.is_empty() && self.load_aligns.iter().all(|a| *a >= PAGE_16K)
    }
}

fn u16_at(data: &[u8], offset: usize, le: bool) -> Option<u16> {
    let bytes: [u8; 2] = data.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(if le {
        u16::from_le_bytes(bytes)
    } else {
        u16::from_be_bytes(bytes)
    })
}

fn u32_at(data: &[u8], offset: usize, le: bool) -> Option<u32> {
    let bytes: [u8; 4] = data.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(if le {
        u32::from_le_bytes(bytes)
    } else {
        u32::from_be_bytes(bytes)
    })
}

fn u64_at(data: &[u8], offset: usize, le: bool) -> Option<u64> {
    let bytes: [u8; 8] = data.get(offset..offset.checked_add(8)?)?.try_into().ok()?;
    Some(if le {
        u64::from_le_bytes(bytes)
    } else {
        u64::from_be_bytes(bytes)
    })
}

/// Parse the program headers of an ELF image held in memory.
pub fn parse_elf(data: &[u8]) -> Result<ElfInfo> {
    if data.get(..4) != Some(b"\x7fELF") {
        bail!("not an ELF file (bad magic)");
    }
    let is_64bit = match data.get(4) {
        Some(1) => false,
        Some(2) => true,
        other => bail!("unsupported ELF class {other:?}"),
    };
    let le = match data.get(5) {
        Some(1) => true,
        Some(2) => false,
        other => bail!("unsupported ELF data encoding {other:?}"),
    };
    let truncated = || anyhow::anyhow!("truncated ELF header");
    let (phoff, phentsize, phnum) = if is_64bit {
        (
            usize::try_from(u64_at(data, 0x20, le).ok_or_else(truncated)?)?,
            usize::from(u16_at(data, 0x36, le).ok_or_else(truncated)?),
            usize::from(u16_at(data, 0x38, le).ok_or_else(truncated)?),
        )
    } else {
        (
            usize::try_from(u32_at(data, 0x1C, le).ok_or_else(truncated)?)?,
            usize::from(u16_at(data, 0x2A, le).ok_or_else(truncated)?),
            usize::from(u16_at(data, 0x2C, le).ok_or_else(truncated)?),
        )
    };
    let min_entry = if is_64bit { 56 } else { 32 };
    if phnum > 0 && phentsize < min_entry {
        bail!("program header entry size {phentsize} is too small");
    }
    let mut load_aligns = Vec::new();
    for index in 0..phnum {
        let base = phoff
            .checked_add(index.checked_mul(phentsize).ok_or_else(truncated)?)
            .ok_or_else(truncated)?;
        let p_type = u32_at(data, base, le).ok_or_else(truncated)?;
        if p_type != PT_LOAD {
            continue;
        }
        let align = if is_64bit {
            u64_at(data, base + 48, le).ok_or_else(truncated)?
        } else {
            u64::from(u32_at(data, base + 28, le).ok_or_else(truncated)?)
        };
        load_aligns.push(align);
    }
    Ok(ElfInfo {
        is_64bit,
        load_aligns,
    })
}

/// One library that was examined.
#[derive(Debug)]
struct Finding {
    name: String,
    ok: bool,
    required: bool,
    note: String,
}

fn judge_elf(name: &str, data: &[u8]) -> Finding {
    match parse_elf(data) {
        Ok(info) => {
            let aligned = info.is_16k_aligned();
            let min = info
                .min_load_align()
                .map_or_else(|| "no PT_LOAD".to_string(), |a| a.to_string());
            let note = if aligned {
                format!("ALIGNED (min LOAD p_align {min}, {})", bits(info.is_64bit))
            } else {
                format!(
                    "UNALIGNED (min LOAD p_align {min}, {})",
                    bits(info.is_64bit)
                )
            };
            Finding {
                name: name.to_string(),
                ok: aligned,
                required: info.is_64bit,
                note,
            }
        }
        Err(e) => Finding {
            name: name.to_string(),
            ok: false,
            required: true,
            note: format!("UNREADABLE ({e:#})"),
        },
    }
}

fn bits(is_64bit: bool) -> &'static str {
    if is_64bit {
        "64-bit"
    } else {
        "32-bit, not required"
    }
}

fn is_shared_library(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("so"))
}

/// Examine every `lib/**/*.so` entry of an APK/AAB/zip.
fn check_zip(path: &Path) -> Result<Vec<Finding>> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("{} is not a readable zip/apk", path.display()))?;
    let mut findings = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = entry
            .name()
            .context("zip entry has an invalid name")?
            .into_owned();
        if !is_shared_library(&name) || !name.contains("lib/") {
            continue;
        }
        let stored = entry.compression() == zip::CompressionMethod::Stored;
        // Without the data offset a misalignment would pass silently, so treat it as an error.
        let data_start = entry
            .data_start()
            .with_context(|| format!("no data offset available for {name}"))?;
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .with_context(|| format!("reading {name}"))?;
        let mut finding = judge_elf(&name, &data);
        if stored && data_start % PAGE_16K != 0 {
            let is_64 = parse_elf(&data).map(|i| i.is_64bit).unwrap_or(true);
            if is_64 {
                finding.ok = false;
                finding.required = true;
            }
            finding.note = format!(
                "{}; ZIP-MISALIGNED (stored at offset {data_start}, run zipalign -P 16)",
                finding.note
            );
        } else if stored {
            finding.note = format!("{}; stored, zip offset 16 KB aligned", finding.note);
        } else {
            finding.note = format!("{}; compressed in zip (extracted at install)", finding.note);
        }
        findings.push(finding);
    }
    Ok(findings)
}

fn is_package(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("apk") || e.eq_ignore_ascii_case("aab"))
}

/// Collect every loose `.so` and every `.apk`/`.aab` below `dir`, sorted.
fn collect_checkable_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_checkable_files(&path, out)?;
        } else if path
            .to_str()
            .is_some_and(|p| is_shared_library(p) || is_package(p))
        {
            out.push(path);
        }
    }
    out.sort();
    Ok(())
}

fn check_path(path: &Path) -> Result<Vec<Finding>> {
    if path.is_dir() {
        // A directory is scanned for loose libraries and for packages (so the build output
        // directory `app/build/outputs/apk/debug/` works as an argument and is never a vacuous
        // pass over an APK).
        let mut files = Vec::new();
        collect_checkable_files(path, &mut files)?;
        let mut findings = Vec::new();
        for file in &files {
            if file.to_str().is_some_and(is_package) {
                let label = display_path(file);
                for mut finding in check_zip(file)? {
                    finding.name = format!("{label}!{}", finding.name);
                    findings.push(finding);
                }
            } else {
                let data =
                    std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
                findings.push(judge_elf(&display_path(file), &data));
            }
        }
        return Ok(findings);
    }
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if data.starts_with(b"\x7fELF") {
        return Ok(vec![judge_elf(&display_path(path), &data)]);
    }
    check_zip(path)
}

/// Entry point for `cargo xtask check-elf-alignment <path>...`.
pub fn run(args: &[String]) -> Result<ExitCode> {
    if args.is_empty() {
        bail!("usage: cargo xtask check-elf-alignment <apk|aab|dir|.so>...");
    }
    let mut all = Vec::new();
    for arg in args {
        all.extend(check_path(Path::new(arg))?);
    }
    if all.is_empty() {
        println!("no native libraries found; nothing to check");
        return Ok(ExitCode::SUCCESS);
    }
    let mut failed = 0;
    for finding in &all {
        let label = if finding.ok {
            "OK  "
        } else if finding.required {
            failed += 1;
            "FAIL"
        } else {
            "INFO"
        };
        println!("[{label}] {} : {}", finding.name, finding.note);
    }
    if failed > 0 {
        eprintln!(
            "{failed} native librar{} not 16 KB compatible",
            if failed == 1 { "y is" } else { "ies are" }
        );
        return Ok(ExitCode::from(1));
    }
    println!(
        "{} native librar{} checked, none fails the 16 KB requirement",
        all.len(),
        if all.len() == 1 { "y" } else { "ies" }
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// Build a minimal ELF with the given `(p_type, p_align)` program headers.
    fn build_elf(is_64bit: bool, le: bool, headers: &[(u32, u64)]) -> Vec<u8> {
        let put16 = |v: u16| {
            if le {
                v.to_le_bytes().to_vec()
            } else {
                v.to_be_bytes().to_vec()
            }
        };
        let put32 = |v: u32| {
            if le {
                v.to_le_bytes().to_vec()
            } else {
                v.to_be_bytes().to_vec()
            }
        };
        let put64 = |v: u64| {
            if le {
                v.to_le_bytes().to_vec()
            } else {
                v.to_be_bytes().to_vec()
            }
        };
        let header_len = if is_64bit { 64 } else { 52 };
        let entry_len: usize = if is_64bit { 56 } else { 32 };
        let mut data = vec![0u8; header_len];
        data[..4].copy_from_slice(b"\x7fELF");
        data[4] = if is_64bit { 2 } else { 1 };
        data[5] = if le { 1 } else { 2 };
        let count = u16::try_from(headers.len()).unwrap();
        if is_64bit {
            data[0x20..0x28].copy_from_slice(&put64(64));
            data[0x36..0x38].copy_from_slice(&put16(56));
            data[0x38..0x3A].copy_from_slice(&put16(count));
        } else {
            data[0x1C..0x20].copy_from_slice(&put32(52));
            data[0x2A..0x2C].copy_from_slice(&put16(32));
            data[0x2C..0x2E].copy_from_slice(&put16(count));
        }
        for (p_type, align) in headers {
            let mut entry = vec![0u8; entry_len];
            entry[..4].copy_from_slice(&put32(*p_type));
            if is_64bit {
                entry[48..56].copy_from_slice(&put64(*align));
            } else {
                entry[28..32].copy_from_slice(&put32(u32::try_from(*align).unwrap()));
            }
            data.extend_from_slice(&entry);
        }
        data
    }

    #[test]
    fn aligned_64bit_elf_passes() {
        let elf = build_elf(true, true, &[(PT_LOAD, 0x4000), (2, 8), (PT_LOAD, 0x10000)]);
        let info = parse_elf(&elf).unwrap();
        assert!(info.is_64bit);
        assert_eq!(info.load_aligns, vec![0x4000, 0x10000]);
        assert!(info.is_16k_aligned());
        assert_eq!(info.min_load_align(), Some(0x4000));
    }

    #[test]
    fn four_kib_64bit_elf_fails() {
        let info = parse_elf(&build_elf(
            true,
            true,
            &[(PT_LOAD, 0x1000), (PT_LOAD, 0x4000)],
        ))
        .unwrap();
        assert!(!info.is_16k_aligned());
        assert_eq!(info.min_load_align(), Some(0x1000));
    }

    #[test]
    fn elf_without_load_segments_is_not_aligned() {
        let info = parse_elf(&build_elf(true, true, &[(2, 8)])).unwrap();
        assert!(!info.is_16k_aligned());
    }

    #[test]
    fn thirty_two_bit_and_big_endian_parse() {
        let info = parse_elf(&build_elf(false, true, &[(PT_LOAD, 0x1000)])).unwrap();
        assert!(!info.is_64bit);
        assert_eq!(info.load_aligns, vec![0x1000]);
        let be = parse_elf(&build_elf(true, false, &[(PT_LOAD, 0x4000)])).unwrap();
        assert_eq!(be.load_aligns, vec![0x4000]);
    }

    #[test]
    fn garbage_and_truncated_input_is_rejected() {
        assert!(parse_elf(b"MZ not an elf").is_err());
        assert!(parse_elf(b"\x7fELF").is_err());
        let mut elf = build_elf(true, true, &[(PT_LOAD, 0x4000)]);
        elf.truncate(70);
        assert!(parse_elf(&elf).is_err());
    }

    #[test]
    fn absurd_header_offsets_do_not_panic() {
        let mut elf = build_elf(true, true, &[(PT_LOAD, 0x4000)]);
        elf[0x20..0x28].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(parse_elf(&elf).is_err());
    }

    #[test]
    fn judge_marks_32bit_unaligned_as_not_required() {
        let finding = judge_elf(
            "lib/armeabi-v7a/libx.so",
            &build_elf(false, true, &[(PT_LOAD, 0x1000)]),
        );
        assert!(!finding.ok);
        assert!(!finding.required);
        let finding = judge_elf(
            "lib/arm64-v8a/libx.so",
            &build_elf(true, true, &[(PT_LOAD, 0x1000)]),
        );
        assert!(!finding.ok);
        assert!(finding.required);
    }

    fn write_zip(stored_alignment: Option<u16>, elf: &[u8]) -> Vec<u8> {
        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            let mut options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            if let Some(alignment) = stored_alignment {
                options = options.with_alignment(alignment);
            }
            // A short entry first so the next one is not accidentally aligned.
            writer
                .start_file("assets/pad.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"x").unwrap();
            writer
                .start_file("lib/arm64-v8a/libgander.so", options)
                .unwrap();
            writer.write_all(elf).unwrap();
            writer.finish().unwrap();
        }
        buffer.into_inner()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("gander-xtask-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn apk_with_aligned_stored_library_passes() {
        let elf = build_elf(true, true, &[(PT_LOAD, 0x4000)]);
        let path = scratch("aligned.apk");
        std::fs::write(&path, write_zip(Some(16384), &elf)).unwrap();
        let findings = check_zip(&path).unwrap();
        assert_eq!(findings.len(), 1);
        assert!(findings[0].ok, "{}", findings[0].note);
    }

    #[test]
    fn apk_with_misaligned_stored_library_fails() {
        let elf = build_elf(true, true, &[(PT_LOAD, 0x4000)]);
        let path = scratch("misaligned.apk");
        std::fs::write(&path, write_zip(None, &elf)).unwrap();
        let findings = check_zip(&path).unwrap();
        assert_eq!(findings.len(), 1);
        assert!(!findings[0].ok);
        assert!(findings[0].required);
        assert!(findings[0].note.contains("ZIP-MISALIGNED"));
    }

    #[test]
    fn directory_scan_checks_apks_inside() {
        let dir = scratch("apk-tree");
        let _ = std::fs::remove_dir_all(&dir);
        let nested = dir.join("debug");
        std::fs::create_dir_all(&nested).unwrap();
        let elf = build_elf(true, true, &[(PT_LOAD, 0x1000)]);
        std::fs::write(nested.join("app-debug.apk"), write_zip(Some(16384), &elf)).unwrap();
        let findings = check_path(&dir).unwrap();
        assert_eq!(findings.len(), 1);
        assert!(
            findings[0]
                .name
                .contains("app-debug.apk!lib/arm64-v8a/libgander.so")
        );
        assert!(
            !findings[0].ok,
            "4 KiB LOAD alignment must fail: {}",
            findings[0].note
        );
        assert!(findings[0].required);
    }

    #[test]
    fn directory_scan_finds_nested_libraries() {
        let dir = scratch("tree");
        let abi = dir.join("lib").join("arm64-v8a");
        std::fs::create_dir_all(&abi).unwrap();
        std::fs::write(
            abi.join("liba.so"),
            build_elf(true, true, &[(PT_LOAD, 0x4000)]),
        )
        .unwrap();
        std::fs::write(abi.join("notes.txt"), b"ignore me").unwrap();
        let findings = check_path(&dir).unwrap();
        assert_eq!(findings.len(), 1);
        assert!(findings[0].ok);
    }
}
