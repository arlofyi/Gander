// SPDX-License-Identifier: Apache-2.0
//! Windows handler resolution and signature reading for the Stream launch policy
//! (`docs/specs/2026-11-01-stream-launch-policy.md`, step 2). Nothing here executes or opens a
//! file for execution:
//!
//! - the handler comes from `AssocQueryStringW` (`ASSOCSTR_EXECUTABLE`, `ASSOCSTR_COMMAND`,
//!   `ASSOCSTR_APPID`) for the `open` verb;
//! - the executable path is canonicalised with `std::fs::canonicalize`, which opens a handle
//!   without read or execute access and calls `GetFinalPathNameByHandleW`;
//! - the Authenticode signature is read with `WinVerifyTrust` (no UI, no revocation check, no
//!   network retrieval), first as an embedded signature, then through the system catalogues
//!   (`CryptCATAdmin*`) for catalogue-signed Windows binaries.

use std::ffi::{OsStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::ptr;

use gander_core::policy::{
    AssocError, AssociationResolver, HandlerInfo, PolicyEnvironment, Signature,
};
use windows_sys::Win32::Foundation::{
    HANDLE, HWND, INVALID_HANDLE_VALUE, TRUST_E_NOSIGNATURE, TRUST_E_PROVIDER_UNKNOWN,
    TRUST_E_SUBJECT_FORM_UNKNOWN,
};
use windows_sys::Win32::Security::Cryptography::Catalog::{
    CATALOG_INFO, CryptCATAdminAcquireContext2, CryptCATAdminCalcHashFromFileHandle2,
    CryptCATAdminEnumCatalogFromHash, CryptCATAdminReleaseCatalogContext,
    CryptCATAdminReleaseContext, CryptCATCatalogInfoFromContext,
};
use windows_sys::Win32::Security::Cryptography::{
    CERT_NAME_SIMPLE_DISPLAY_TYPE, CertGetNameStringW,
};
use windows_sys::Win32::Security::WinTrust::{
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_CATALOG_INFO, WINTRUST_DATA, WINTRUST_FILE_INFO,
    WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_CATALOG, WTD_CHOICE_FILE, WTD_DISABLE_MD2_MD4,
    WTD_REVOCATION_CHECK_NONE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
    WTD_UI_NONE, WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData, WinVerifyTrust,
};
use windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
use windows_sys::Win32::UI::Shell::{
    ASSOCF_INIT_IGNOREUNKNOWN, ASSOCF_NOTRUNCATE, ASSOCSTR, ASSOCSTR_APPID, ASSOCSTR_COMMAND,
    ASSOCSTR_EXECUTABLE, AssocQueryStringW,
};

/// `HRESULT_FROM_WIN32(ERROR_NO_ASSOCIATION)`: nothing is registered for the type.
const HR_NO_ASSOCIATION: i32 = 0x8007_0483_u32 as i32;
/// `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)`: the requested string is not registered.
const HR_NOT_FOUND: i32 = 0x8007_0490_u32 as i32;
/// `S_FALSE`: returned with the required size when the output buffer is null.
const S_FALSE: i32 = 1;

/// The production [`AssociationResolver`]: the current user's file associations.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsAssociations;

impl AssociationResolver for WindowsAssociations {
    fn resolve(&self, ext: &str) -> Result<Option<HandlerInfo>, AssocError> {
        let ext = ext.strip_prefix('.').unwrap_or(ext);
        if ext.is_empty() || ext.contains(['\\', '/', ':', '\0']) {
            return Ok(None);
        }
        let assoc = format!(".{ext}");
        let app_user_model_id = query_assoc(&assoc, ASSOCSTR_APPID).ok().flatten();
        let Some(exe) = query_assoc(&assoc, ASSOCSTR_EXECUTABLE)? else {
            // Packaged apps activated through COM (Photos for .jpg on Windows 11, measured
            // 2026-11-01) register an AppUserModelID but no executable.
            return Ok(app_user_model_id.and_then(packaged_handler));
        };
        // `"%1" %*` (batch files, executables) resolves to the file itself: no handler app.
        if exe.contains('%') {
            return Ok(None);
        }
        let command = query_assoc(&assoc, ASSOCSTR_COMMAND)?;
        let mut info = inspect_exe(Path::new(&exe))?;
        info.command = command;
        info.package_family_name = app_user_model_id
            .as_deref()
            .and_then(package_family_from_aumid);
        info.app_user_model_id = app_user_model_id;
        Ok(Some(info))
    }

    fn inspect(&self, exe: &Path) -> Result<HandlerInfo, AssocError> {
        if let Some(aumid) = exe
            .to_str()
            .and_then(|s| s.strip_prefix("shell:AppsFolder\\"))
        {
            return packaged_handler(aumid.to_owned())
                .ok_or_else(|| AssocError("not a packaged app id".to_owned()));
        }
        inspect_exe(exe)
    }
}

/// A packaged handler known only by its `AppUserModelID`.
fn packaged_handler(aumid: String) -> Option<HandlerInfo> {
    let family = package_family_from_aumid(&aumid)?;
    Some(HandlerInfo {
        exe: PathBuf::from(format!("shell:AppsFolder\\{aumid}")),
        command: None,
        signature: Signature::Package,
        app_user_model_id: Some(aumid),
        package_family_name: Some(family),
    })
}

/// Canonicalises `exe` and reads its signature.
fn inspect_exe(exe: &Path) -> Result<HandlerInfo, AssocError> {
    if !exe.is_absolute() {
        return Err(AssocError(
            "handler executable is not an absolute path".to_owned(),
        ));
    }
    let canonical = std::fs::canonicalize(exe)
        .map_err(|e| AssocError(format!("cannot canonicalise handler: {e}")))?;
    if !canonical.is_file() {
        return Err(AssocError("handler is not a file".to_owned()));
    }
    let signature = read_signature(&canonical);
    Ok(HandlerInfo::new(canonical, signature))
}

/// The package family name of an `AppUserModelID` (`Family_publisherid!App`). Plain desktop
/// apps report an AUMID without `!`; they have no package.
fn package_family_from_aumid(aumid: &str) -> Option<String> {
    let (family, app) = aumid.split_once('!')?;
    (!family.is_empty() && !app.is_empty() && family.contains('_')).then(|| family.to_owned())
}

/// Host facts for the policy: `%SystemRoot%` from `GetSystemWindowsDirectoryW` (not from the
/// environment, which a parent process controls).
pub fn policy_environment() -> PolicyEnvironment {
    let mut buf = [0u16; 512];
    // SAFETY: the buffer and its length in u16 units are valid for the call.
    let len = unsafe { GetSystemWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if len == 0 || len >= buf.len() {
        return PolicyEnvironment::default();
    }
    PolicyEnvironment {
        system_root: PathBuf::from(String::from_utf16_lossy(&buf[..len])),
    }
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// `AssocQueryStringW` for the `open` verb. `Ok(None)` when nothing is registered.
fn query_assoc(assoc: &str, what: ASSOCSTR) -> Result<Option<String>, AssocError> {
    // The shell resolves packaged handlers through COM/WinRT: the thread must be in an apartment
    // and the MTA must outlive the call (see `crate::com`).
    let _com = crate::com::ComScope::enter();
    let assoc_w = wide(OsStr::new(assoc));
    let verb_w = wide(OsStr::new("open"));
    let flags = ASSOCF_NOTRUNCATE | ASSOCF_INIT_IGNOREUNKNOWN;
    let mut len: u32 = 0;
    // SAFETY: valid NUL-terminated strings; a null output buffer asks for the required length.
    let hr = unsafe {
        AssocQueryStringW(
            flags,
            what,
            assoc_w.as_ptr(),
            verb_w.as_ptr(),
            ptr::null_mut(),
            &mut len,
        )
    };
    if hr == HR_NO_ASSOCIATION || hr == HR_NOT_FOUND {
        return Ok(None);
    }
    if hr != S_FALSE && hr != 0 {
        return Err(AssocError(format!(
            "AssocQueryStringW size query failed: 0x{:08X}",
            hr as u32
        )));
    }
    if len == 0 {
        return Ok(None);
    }
    let mut buf = vec![0u16; len as usize];
    // SAFETY: `buf` holds `len` u16 units, which the call reported as sufficient.
    let hr = unsafe {
        AssocQueryStringW(
            flags,
            what,
            assoc_w.as_ptr(),
            verb_w.as_ptr(),
            buf.as_mut_ptr(),
            &mut len,
        )
    };
    if hr == HR_NO_ASSOCIATION || hr == HR_NOT_FOUND {
        return Ok(None);
    }
    if hr != 0 {
        return Err(AssocError(format!(
            "AssocQueryStringW failed: 0x{:08X}",
            hr as u32
        )));
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let value = String::from_utf16_lossy(&buf[..end]);
    Ok((!value.is_empty()).then_some(value))
}

/// Reads the Authenticode signature of `path` without executing it: embedded signature first,
/// then the system catalogues.
pub fn read_signature(path: &Path) -> Signature {
    let path_w = wide(path.as_os_str());
    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: path_w.as_ptr(),
        hFile: ptr::null_mut(),
        pgKnownSubject: ptr::null_mut(),
    };
    let mut data = new_trust_data(WTD_CHOICE_FILE);
    data.Anonymous.pFile = &mut file_info;
    // SAFETY: `data` points at `file_info`, which outlives the call.
    match unsafe { verify(&mut data) } {
        Verified::Signed(publisher) => Signature::Signed { publisher },
        Verified::Untrusted => Signature::Untrusted,
        Verified::NoSignature => read_catalog_signature(path, &path_w),
    }
}

enum Verified {
    Signed(String),
    NoSignature,
    Untrusted,
}

fn new_trust_data(choice: u32) -> WINTRUST_DATA {
    WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: choice,
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL | WTD_REVOCATION_CHECK_NONE | WTD_DISABLE_MD2_MD4,
        ..Default::default()
    }
}

/// Runs `WinVerifyTrust` (verify, read the signer, close).
///
/// # Safety
///
/// `data` must be filled by [`new_trust_data`] and its union pointer must stay valid for the call.
unsafe fn verify(data: &mut WINTRUST_DATA) -> Verified {
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let no_ui = INVALID_HANDLE_VALUE as HWND;
    // SAFETY: the caller guarantees `data` is valid; `action` is a valid GUID.
    let status = unsafe {
        WinVerifyTrust(
            no_ui,
            &mut action,
            (data as *mut WINTRUST_DATA).cast::<c_void>(),
        )
    };
    let result = if status == 0 {
        // SAFETY: state data is valid after a successful verify until the close call below.
        match unsafe { signer_name(data.hWVTStateData) } {
            Some(name) => Verified::Signed(name),
            None => Verified::Untrusted,
        }
    } else if status == TRUST_E_NOSIGNATURE
        || status == TRUST_E_SUBJECT_FORM_UNKNOWN
        || status == TRUST_E_PROVIDER_UNKNOWN
    {
        Verified::NoSignature
    } else {
        Verified::Untrusted
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: releases the state allocated by the verify call; same arguments.
    unsafe {
        WinVerifyTrust(
            no_ui,
            &mut action,
            (data as *mut WINTRUST_DATA).cast::<c_void>(),
        )
    };
    result
}

/// The simple display name of the leaf certificate of the first signer.
///
/// # Safety
///
/// `state` must be the `hWVTStateData` of a successful, not yet closed `WinVerifyTrust` call.
unsafe fn signer_name(state: HANDLE) -> Option<String> {
    // SAFETY: per the function contract.
    let prov = unsafe { WTHelperProvDataFromStateData(state) };
    if prov.is_null() {
        return None;
    }
    // SAFETY: `prov` is valid provider data.
    let signer = unsafe { WTHelperGetProvSignerFromChain(prov, 0, 0, 0) };
    if signer.is_null() {
        return None;
    }
    // SAFETY: `signer` is valid while the state is open.
    let signer = unsafe { &*signer };
    if signer.csCertChain == 0 || signer.pasCertChain.is_null() {
        return None;
    }
    // SAFETY: the chain has at least one element.
    let cert = unsafe { (*signer.pasCertChain).pCert };
    if cert.is_null() {
        return None;
    }
    // SAFETY: valid certificate context; a null buffer asks for the length.
    let len = unsafe {
        CertGetNameStringW(
            cert,
            CERT_NAME_SIMPLE_DISPLAY_TYPE,
            0,
            ptr::null(),
            ptr::null_mut(),
            0,
        )
    };
    if len <= 1 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    // SAFETY: `buf` has the reported length.
    let written = unsafe {
        CertGetNameStringW(
            cert,
            CERT_NAME_SIMPLE_DISPLAY_TYPE,
            0,
            ptr::null(),
            buf.as_mut_ptr(),
            len,
        )
    };
    let end = (written as usize).saturating_sub(1).min(buf.len());
    let name = String::from_utf16_lossy(&buf[..end]);
    (!name.is_empty()).then_some(name)
}

/// Looks `path` up in the system catalogues (SHA-256, then SHA-1 for older catalogues) and
/// verifies the catalogue signature.
fn read_catalog_signature(path: &Path, path_w: &[u16]) -> Signature {
    let Ok(file) = std::fs::File::open(path) else {
        return Signature::Unsigned;
    };
    let handle = file.as_raw_handle() as HANDLE;
    let sha256 = wide(OsStr::new("SHA256"));
    for algorithm in [sha256.as_ptr(), ptr::null()] {
        // SAFETY: `handle` is an open file handle for the duration; `path_w` is NUL-terminated.
        if let Some(sig) = unsafe { catalog_lookup(handle, path_w, algorithm) } {
            return sig;
        }
    }
    Signature::Unsigned
}

/// One catalogue lookup with the given hash algorithm. `None` when no catalogue lists the file.
///
/// # Safety
///
/// `file` must be an open readable file handle and `path_w` NUL-terminated; `algorithm` is null
/// or a NUL-terminated algorithm name.
unsafe fn catalog_lookup(file: HANDLE, path_w: &[u16], algorithm: *const u16) -> Option<Signature> {
    let mut admin: isize = 0;
    // SAFETY: out pointer is valid; null subsystem and policy select the defaults.
    if unsafe { CryptCATAdminAcquireContext2(&mut admin, ptr::null(), algorithm, ptr::null(), 0) }
        == 0
    {
        return None;
    }
    let result = (|| {
        let mut cb: u32 = 0;
        // SAFETY: size query with a null buffer.
        unsafe { CryptCATAdminCalcHashFromFileHandle2(admin, file, &mut cb, ptr::null_mut(), 0) };
        if cb == 0 || cb > 256 {
            return None;
        }
        let mut hash = vec![0u8; cb as usize];
        // SAFETY: `hash` holds `cb` bytes.
        if unsafe {
            CryptCATAdminCalcHashFromFileHandle2(admin, file, &mut cb, hash.as_mut_ptr(), 0)
        } == 0
        {
            return None;
        }
        // SAFETY: valid admin context and hash.
        let catalog = unsafe {
            CryptCATAdminEnumCatalogFromHash(admin, hash.as_ptr(), cb, 0, ptr::null_mut())
        };
        if catalog == 0 {
            return None;
        }
        let mut info = CATALOG_INFO {
            cbStruct: size_of::<CATALOG_INFO>() as u32,
            wszCatalogFile: [0; 260],
        };
        // SAFETY: valid catalogue context and output struct.
        let got_info = unsafe { CryptCATCatalogInfoFromContext(catalog, &mut info, 0) } != 0;
        let signature = if got_info {
            let tag: String = hash.iter().map(|b| format!("{b:02X}")).collect();
            let tag_w = wide(OsStr::new(&tag));
            let mut cat_info = WINTRUST_CATALOG_INFO {
                cbStruct: size_of::<WINTRUST_CATALOG_INFO>() as u32,
                dwCatalogVersion: 0,
                pcwszCatalogFilePath: info.wszCatalogFile.as_ptr(),
                pcwszMemberTag: tag_w.as_ptr(),
                pcwszMemberFilePath: path_w.as_ptr(),
                hMemberFile: file,
                pbCalculatedFileHash: hash.as_mut_ptr(),
                cbCalculatedFileHash: cb,
                pcCatalogContext: ptr::null_mut(),
                hCatAdmin: admin,
            };
            let mut data = new_trust_data(WTD_CHOICE_CATALOG);
            data.Anonymous.pCatalog = &mut cat_info;
            // SAFETY: every pointer in `cat_info` outlives the call.
            Some(match unsafe { verify(&mut data) } {
                Verified::Signed(publisher) => Signature::Signed { publisher },
                Verified::Untrusted | Verified::NoSignature => Signature::Untrusted,
            })
        } else {
            None
        };
        // SAFETY: releases the catalogue context obtained above.
        unsafe { CryptCATAdminReleaseCatalogContext(admin, catalog, 0) };
        signature
    })();
    // SAFETY: releases the admin context acquired above.
    unsafe { CryptCATAdminReleaseContext(admin, 0) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use gander_core::policy::rules;

    #[test]
    fn unregistered_extension_has_no_handler() {
        let r = WindowsAssociations.resolve("ganderzzqq9");
        assert_eq!(r, Ok(None));
    }

    #[test]
    fn hostile_extension_strings_are_not_queried() {
        assert_eq!(WindowsAssociations.resolve(""), Ok(None));
        assert_eq!(WindowsAssociations.resolve("..\\x"), Ok(None));
    }

    #[test]
    fn txt_resolves_to_a_canonical_existing_handler() {
        // Every Windows install associates .txt (Notepad, classic or packaged). The test reads
        // the association only.
        let Some(h) = WindowsAssociations.resolve("txt").expect("lookup") else {
            panic!("no .txt handler on this machine");
        };
        assert!(h.exe.is_absolute(), "{}", h.exe.display());
        assert!(h.exe.is_file());
        assert!(
            h.exe_name().to_ascii_lowercase().ends_with(".exe"),
            "{}",
            h.exe_name()
        );
    }

    #[test]
    fn system_root_comes_from_the_os() {
        let env = policy_environment();
        assert!(env.system_root.is_absolute());
        assert!(env.system_root.join("System32").is_dir());
    }

    #[test]
    fn signature_of_a_windows_binary_is_microsoft() {
        // cmd.exe is catalogue- or embedded-signed by Microsoft. Only its signature is read.
        let cmd = policy_environment()
            .system_root
            .join("System32")
            .join("cmd.exe");
        match read_signature(&cmd) {
            Signature::Signed { publisher } => {
                assert!(publisher.starts_with("Microsoft"), "{publisher}");
            }
            other => panic!("unexpected {other:?}"),
        }
        // And the policy refuses it whatever it opens.
        let info = WindowsAssociations.inspect(&cmd).unwrap();
        assert!(rules::is_never_listed_name(&info.exe_name()));
    }

    #[test]
    fn unsigned_file_reads_as_unsigned() {
        let dir = std::env::temp_dir().join(format!("gander-sig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("plain.exe");
        // Not a PE image and never executed: only its signature is read.
        std::fs::write(&file, b"not a program").unwrap();
        let sig = read_signature(&file);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(sig, Signature::Unsigned | Signature::Untrusted),
            "{sig:?}"
        );
        assert_eq!(sig.publisher(), None);
    }

    #[test]
    fn inspect_canonicalises_case_and_separators() {
        let env = policy_environment();
        let spelled = PathBuf::from(
            env.system_root
                .join("system32")
                .join("CMD.EXE")
                .to_string_lossy()
                .replace('\\', "/"),
        );
        let info = WindowsAssociations.inspect(&spelled).unwrap();
        assert!(rules::same_windows_path(
            &info.exe,
            &env.system_root.join("System32").join("cmd.exe")
        ));
    }

    #[test]
    fn relative_handler_is_refused() {
        assert!(WindowsAssociations.inspect(Path::new("cmd.exe")).is_err());
    }

    #[test]
    fn real_associations_never_allow_without_a_catalogue() {
        use gander_core::config::AppCatalogue;
        use gander_core::policy::{PolicyInput, StreamPolicy};
        use gander_core::protocol::files::StreamDecision;

        let catalogue = AppCatalogue::default();
        let env = policy_environment();
        let policy = StreamPolicy::new(&catalogue, &WindowsAssociations, &env);
        for name in [
            "notes.txt",
            "Report.pdf",
            "Deck.pptx",
            "page.html",
            "photo.jpg",
            "run.bat",
            "script.py",
            "setup.exe",
            "link.lnk",
            "app.js",
        ] {
            let d = policy.evaluate(&PolicyInput {
                file_name: name,
                stream_permitted: true,
                share_default: StreamDecision::Ask,
                requested_app: None,
            });
            // Visible with --nocapture: what this machine's associations resolve to.
            println!(
                "{name}: {:?} {} handler={:?} signer={:?} browser={:?}",
                d.outcome,
                d.reason.as_str(),
                d.handler.as_ref().map(|h| h.exe_name()),
                d.handler.as_ref().map(|h| h.signature.display().to_owned()),
                d.browser.map(|b| b.browser),
            );
            assert_ne!(d.outcome, StreamDecision::Allowed, "{name}");
        }
    }

    #[test]
    fn packaged_handler_shape() {
        let h = packaged_handler("Microsoft.Windows.Photos_8wekyb3d8bbwe!App".to_owned()).unwrap();
        assert_eq!(h.signature, Signature::Package);
        assert_eq!(
            h.package_family_name.as_deref(),
            Some("Microsoft.Windows.Photos_8wekyb3d8bbwe")
        );
        let again = WindowsAssociations.inspect(&h.exe).unwrap();
        assert_eq!(again, h);
        assert!(packaged_handler("NoFamily".to_owned()).is_none());
    }

    #[test]
    fn aumid_family() {
        assert_eq!(
            package_family_from_aumid("Microsoft.WindowsNotepad_8wekyb3d8bbwe!App").as_deref(),
            Some("Microsoft.WindowsNotepad_8wekyb3d8bbwe")
        );
        assert_eq!(package_family_from_aumid("Chrome"), None);
        assert_eq!(package_family_from_aumid("!App"), None);
    }
}
