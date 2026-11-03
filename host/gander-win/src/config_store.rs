// SPDX-License-Identifier: Apache-2.0
//! Owner-only config file storage on Windows: the [`SecureFileStore`] behind
//! `gander_core::config`.
//!
//! Every write creates a temporary file next to the target **with** a protected DACL that holds a
//! single ACE (full access for the current user), writes and flushes it, then renames it over the
//! target. The file is never readable by anyone else, not even for an instant, and a crash leaves
//! either the old or the new file. A rename on the same volume keeps the temporary file's security
//! descriptor. A missing parent directory is created with the same owner-only DACL (inheritable).

use std::ffi::{OsStr, c_void};
use std::fs::File;
use std::io::{self, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};

use gander_core::config::SecureFileStore;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_SUCCESS, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
    GetSecurityDescriptorControl, GetTokenInformation, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ALL_ACCESS, FILE_ATTRIBUTE_NORMAL,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// `ACCESS_ALLOWED_ACE_TYPE` from winnt.h.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

/// The Windows [`SecureFileStore`]: owner-only DACL, atomic replace.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsConfigStore;

impl SecureFileStore for WindowsConfigStore {
    fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write_owner_only(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
        if !parent.exists() {
            create_owner_only_dir(parent)?;
        }
        let tmp = temp_name(path);
        let result =
            write_new_owner_only(&tmp, contents).and_then(|()| std::fs::rename(&tmp, path));
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }
}

/// The per-user config directory, `%APPDATA%\Gander`.
pub fn config_dir() -> io::Result<PathBuf> {
    std::env::var_os("APPDATA")
        .map(|appdata| PathBuf::from(appdata).join("Gander"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "APPDATA is not set"))
}

/// The current user's SID as a string (`S-1-5-21-...`), for the Task Scheduler logon task.
pub fn current_user_sid_string() -> io::Result<String> {
    CurrentUserSid::new()?.to_sddl_string()
}

/// Creates `dir` (and any missing ancestors; only `dir` itself gets the owner-only DACL, with
/// inheritance so new children are owner-only too).
pub fn create_owner_only_dir(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let sd = OwnerOnlyDescriptor::new(true)?;
    let wide = wide(dir.as_os_str());
    let sa = sd.attributes();
    // SAFETY: `wide` is NUL-terminated and outlives the call; `sa` points at a valid descriptor
    // owned by `sd`, which lives until the end of this function.
    let ok = unsafe { CreateDirectoryW(wide.as_ptr(), &sa) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Whether `path` has a protected DACL with exactly one ACE that allows the current user full
/// access. Used by tests and by the tray's self-check.
pub fn is_owner_only(path: &Path) -> io::Result<bool> {
    let user = CurrentUserSid::new()?;
    let wide = wide(path.as_os_str());
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: out-pointers are valid locals; `wide` is NUL-terminated. On success `sd` is a
    // LocalAlloc'd buffer that `dacl` points into; it is freed below.
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut sd,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let _free = LocalGuard(sd);

    let mut control: u16 = 0;
    let mut revision: u32 = 0;
    // SAFETY: `sd` is the valid descriptor returned above.
    if unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if control & SE_DACL_PROTECTED == 0 || dacl.is_null() {
        return Ok(false);
    }
    // SAFETY: `dacl` is non-null and points into `sd`, which is still alive.
    if unsafe { (*dacl).AceCount } != 1 {
        return Ok(false);
    }
    let mut ace: *mut c_void = ptr::null_mut();
    // SAFETY: index 0 exists (AceCount == 1); `ace` receives a pointer into the ACL.
    if unsafe { GetAce(dacl, 0, &mut ace) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
    // SAFETY: GetAce returned a valid ACE pointer; the header is read before interpreting the
    // rest, and for ACCESS_ALLOWED_ACE_TYPE the SID starts at `SidStart`.
    let owner_only = unsafe {
        (*ace).Header.AceType == ACCESS_ALLOWED_ACE_TYPE
            && (*ace).Mask & FILE_ALL_ACCESS == FILE_ALL_ACCESS
            && EqualSid(ptr::addr_of_mut!((*ace).SidStart).cast(), user.sid()) != 0
    };
    Ok(owner_only)
}

fn temp_name(path: &Path) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp-{}-{n}", std::process::id()));
    path.with_file_name(name)
}

fn write_new_owner_only(path: &Path, contents: &[u8]) -> io::Result<()> {
    let sd = OwnerOnlyDescriptor::new(false)?;
    let sa = sd.attributes();
    let wide = wide(path.as_os_str());
    // SAFETY: `wide` is NUL-terminated; `sa` references `sd`, alive for the call. CREATE_NEW
    // fails rather than reusing an existing file (and its DACL).
    let handle: HANDLE = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            0,
            &sa,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is a fresh, owned file handle; `File` takes ownership and closes it.
    let mut file = unsafe { File::from_raw_handle(handle) };
    file.write_all(contents)?;
    file.sync_all()
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Frees a LocalAlloc'd pointer on drop.
struct LocalGuard(*mut c_void);

impl Drop for LocalGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from an API documented to return LocalAlloc memory.
            unsafe { LocalFree(self.0) };
        }
    }
}

/// The current process token's user SID.
struct CurrentUserSid {
    /// Backing buffer for TOKEN_USER (u64 for alignment).
    buf: Vec<u64>,
}

impl CurrentUserSid {
    fn new() -> io::Result<Self> {
        let mut token: HANDLE = ptr::null_mut();
        // SAFETY: GetCurrentProcess returns a pseudo-handle; `token` is a valid out-pointer.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = Self::from_token(token);
        // SAFETY: `token` was opened above and is closed exactly once.
        unsafe { CloseHandle(token) };
        result
    }

    fn from_token(token: HANDLE) -> io::Result<Self> {
        let mut len: u32 = 0;
        // SAFETY: size query with a null buffer; fails with ERROR_INSUFFICIENT_BUFFER and sets len.
        unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut len) };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: `buf` holds at least `len` bytes and is 8-byte aligned, enough for TOKEN_USER.
        let ok = unsafe {
            GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { buf })
    }

    fn sid(&self) -> *mut c_void {
        let user = self.buf.as_ptr().cast::<TOKEN_USER>();
        // SAFETY: `buf` was filled by GetTokenInformation(TokenUser); the SID points into it.
        unsafe { (*user).User.Sid }
    }

    fn to_sddl_string(&self) -> io::Result<String> {
        let mut out: *mut u16 = ptr::null_mut();
        // SAFETY: `sid()` is valid while `self` lives; `out` receives a LocalAlloc'd string.
        if unsafe { ConvertSidToStringSidW(self.sid(), &mut out) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let _free = LocalGuard(out.cast());
        let mut n = 0;
        // SAFETY: `out` is a NUL-terminated wide string; scan to the terminator.
        while unsafe { *out.add(n) } != 0 {
            n += 1;
        }
        // SAFETY: `out` is valid for `n` u16 values (checked above).
        let slice = unsafe { std::slice::from_raw_parts(out, n) };
        Ok(String::from_utf16_lossy(slice))
    }
}

/// A self-relative security descriptor: protected DACL, one full-access ACE for the current user.
struct OwnerOnlyDescriptor(LocalGuard);

impl OwnerOnlyDescriptor {
    fn new(inheritable: bool) -> io::Result<Self> {
        let sid = CurrentUserSid::new()?.to_sddl_string()?;
        let flags = if inheritable { "OICI" } else { "" };
        let sddl = format!("D:P(A;{flags};FA;;;{sid})");
        let wide = wide(OsStr::new(&sddl));
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated; `sd` receives a LocalAlloc'd descriptor.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(LocalGuard(sd)))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gander_core::config::{
        AppCatalogue, ConfigFile, DevicesFile, HostConfig, ShareConfig, ShareGrant,
    };
    use gander_core::protocol::files::{Permission, StreamDecision};

    /// A unique scratch directory under the system temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "gander-wp10-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sample_config(root: &Path) -> HostConfig {
        let mut cfg = HostConfig::default();
        cfg.shares.push(ShareConfig {
            id: "shr_test".into(),
            name: "Test".into(),
            root: root.to_path_buf(),
            show_hidden: false,
            show_path: false,
            hidden_patterns: vec![],
            stream_default: StreamDecision::Ask,
            grants: vec![ShareGrant {
                device: "dev_test".into(),
                permissions: vec![Permission::List, Permission::Fetch],
            }],
        });
        cfg
    }

    #[test]
    fn save_creates_owner_only_dir_and_files() {
        let scratch = Scratch::new("acl");
        let dir = scratch.0.join("Gander");
        let store = WindowsConfigStore;
        let cfg = sample_config(&scratch.0);

        cfg.save(&store, &dir).unwrap();
        DevicesFile::default().save(&store, &dir).unwrap();
        AppCatalogue::default().save(&store, &dir).unwrap();

        assert!(is_owner_only(&dir).unwrap(), "config dir is not owner-only");
        for name in ["config.toml", "devices.toml", "apps.toml"] {
            assert!(
                is_owner_only(&dir.join(name)).unwrap(),
                "{name} is not owner-only"
            );
        }
        assert_eq!(HostConfig::load(&store, &dir).unwrap(), cfg);

        // No temporary files are left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn overwrite_keeps_owner_only_and_replaces_contents() {
        let scratch = Scratch::new("replace");
        let dir = scratch.0.join("Gander");
        let store = WindowsConfigStore;
        let mut cfg = sample_config(&scratch.0);
        cfg.save(&store, &dir).unwrap();
        cfg.shares[0].name = "Renamed".into();
        cfg.save(&store, &dir).unwrap();
        assert!(is_owner_only(&HostConfig::path_in(&dir)).unwrap());
        assert_eq!(
            HostConfig::load(&store, &dir).unwrap().shares[0].name,
            "Renamed"
        );
    }

    #[test]
    fn default_acl_file_is_not_owner_only() {
        // Guards against an `is_owner_only` that always says yes: a file created by std inherits
        // the temp directory's DACL, which is not protected.
        let scratch = Scratch::new("plain");
        std::fs::create_dir_all(&scratch.0).unwrap();
        let plain = scratch.0.join("plain.txt");
        std::fs::write(&plain, b"x").unwrap();
        assert!(!is_owner_only(&plain).unwrap());
    }

    #[test]
    fn missing_file_reads_as_none() {
        let scratch = Scratch::new("missing");
        assert!(
            WindowsConfigStore
                .read(&scratch.0.join("nope.toml"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn invalid_config_is_not_written() {
        let scratch = Scratch::new("invalid");
        let dir = scratch.0.join("Gander");
        let mut cfg = HostConfig::default();
        cfg.listener.port = 0;
        assert!(cfg.save(&WindowsConfigStore, &dir).is_err());
        assert!(!HostConfig::path_in(&dir).exists());
    }
}
