// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask vectors-check`: every JSON file under `protocol/test-vectors/` must parse.
//!
//! The vector files are shared by the Rust host and the Kotlin client, so a syntax slip (or a
//! UTF-8 byte-order mark added by a Windows editor) breaks both suites. Where a file follows the
//! `{ "vectors": [ { "id": ... } ] }` shape, ids must also be unique strings.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use crate::proc::{display_path, workspace_root};

/// Entry point for `cargo xtask vectors-check [dir]`.
pub fn run(args: &[String]) -> Result<ExitCode> {
    let dir = match args {
        [] => workspace_root().join("protocol").join("test-vectors"),
        [dir] => PathBuf::from(dir),
        _ => bail!("usage: cargo xtask vectors-check [dir]"),
    };
    let mut files = Vec::new();
    collect_json(&dir, &mut files)?;
    if files.is_empty() {
        eprintln!("no JSON files found under {}", display_path(&dir));
        return Ok(ExitCode::from(1));
    }
    let mut failures = 0;
    for file in &files {
        match check_file(file) {
            Ok(summary) => println!("[OK  ] {} : {summary}", display_path(file)),
            Err(e) => {
                failures += 1;
                println!("[FAIL] {} : {e:#}", display_path(file));
            }
        }
    }
    if failures > 0 {
        eprintln!("{failures} of {} vector file(s) invalid", files.len());
        return Ok(ExitCode::from(1));
    }
    println!("all {} vector file(s) valid", files.len());
    Ok(ExitCode::SUCCESS)
}

fn collect_json(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_json(&path, out)?;
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            out.push(path);
        }
    }
    out.sort();
    Ok(())
}

fn check_file(path: &Path) -> Result<String> {
    let text = std::fs::read_to_string(path).context("not valid UTF-8")?;
    check_text(&text)
}

fn check_text(text: &str) -> Result<String> {
    if text.starts_with('\u{feff}') {
        bail!("starts with a UTF-8 byte-order mark; save as UTF-8 without BOM");
    }
    let value: serde_json::Value = serde_json::from_str(text).context("invalid JSON")?;
    let Some(vectors) = value.get("vectors").and_then(serde_json::Value::as_array) else {
        return Ok("valid JSON".to_string());
    };
    let mut seen = HashSet::new();
    for (index, vector) in vectors.iter().enumerate() {
        if let Some(id) = vector.get("id") {
            let Some(id) = id.as_str() else {
                bail!("vectors[{index}].id is not a string");
            };
            if !seen.insert(id.to_string()) {
                bail!("duplicate vector id `{id}`");
            }
        }
    }
    Ok(format!("valid JSON, {} vectors", vectors.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_json_is_accepted() {
        assert_eq!(check_text(r#"{"a": 1}"#).unwrap(), "valid JSON");
    }

    #[test]
    fn vectors_are_counted() {
        let summary = check_text(r#"{"vectors":[{"id":"a"},{"id":"b"}]}"#).unwrap();
        assert!(summary.contains("2 vectors"));
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let err = check_text(r#"{"vectors":[{"id":"a"},{"id":"a"}]}"#).unwrap_err();
        assert!(format!("{err}").contains("duplicate vector id"));
    }

    #[test]
    fn non_string_id_is_rejected() {
        assert!(check_text(r#"{"vectors":[{"id":3}]}"#).is_err());
    }

    #[test]
    fn syntax_errors_and_bom_are_rejected() {
        assert!(check_text("{ not json").is_err());
        assert!(check_text("\u{feff}{}").is_err());
    }

    #[test]
    fn the_repository_vectors_are_valid() {
        let dir = workspace_root().join("protocol").join("test-vectors");
        let mut files = Vec::new();
        collect_json(&dir, &mut files).unwrap();
        assert!(!files.is_empty(), "expected at least paths.json");
        for file in files {
            check_file(&file).unwrap_or_else(|e| panic!("{}: {e:#}", file.display()));
        }
    }
}
