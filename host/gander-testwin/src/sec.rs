// SPDX-License-Identifier: Apache-2.0
//! Security descriptor for the control pipe: current user only.

use std::ffi::c_void;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{PCWSTR, PWSTR};

/// A self-relative security descriptor allocated by the system; freed on drop.
#[derive(Debug)]
pub struct OwnedSecurityDescriptor {
    ptr: PSECURITY_DESCRIPTOR,
}

impl OwnedSecurityDescriptor {
    /// Raw pointer for `SECURITY_ATTRIBUTES::lpSecurityDescriptor`; valid while `self` lives.
    pub fn as_ptr(&self) -> *mut c_void {
        self.ptr.0
    }
}

impl Drop for OwnedSecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: `ptr` was allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW,
        // which documents LocalFree as the way to release it, and is freed exactly once here.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.ptr.0)));
        }
    }
}

/// The SID of the user running this process, as a string such as `S-1-5-21-...`.
pub fn current_user_sid_string() -> Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: GetCurrentProcess returns a pseudo handle that needs no closing, and `token` is a
    // valid out pointer for the duration of the call.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .context("OpenProcessToken")?;
    let result = sid_string_from_token(token);
    // SAFETY: `token` was opened above and is closed exactly once.
    unsafe {
        let _ = CloseHandle(token);
    }
    result
}

fn sid_string_from_token(token: HANDLE) -> Result<String> {
    let mut needed = 0u32;
    // SAFETY: a size query with a null buffer; the call is expected to fail with
    // ERROR_INSUFFICIENT_BUFFER and report the required length in `needed`.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
    if needed == 0 {
        bail!("GetTokenInformation(TokenUser) reported no size");
    }
    // u64 backing store so the buffer is 8-byte aligned for TOKEN_USER.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buf` is at least `needed` bytes, aligned, and outlives the call.
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            needed,
            &mut needed,
        )
    }
    .context("GetTokenInformation(TokenUser)")?;
    // SAFETY: the buffer now holds a TOKEN_USER written by the call above.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut text = PWSTR::null();
    // SAFETY: `sid` points into `buf`, which is still alive; `text` is a valid out pointer.
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.context("ConvertSidToStringSidW")?;
    // SAFETY: on success `text` is a NUL terminated UTF-16 string allocated by the system.
    let s = unsafe { text.to_string() };
    // SAFETY: `text` was allocated by ConvertSidToStringSidW and is released once.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(text.0.cast::<c_void>())));
    }
    s.context("SID string is not valid UTF-16")
}

/// SDDL text granting full control to `sid` and to nobody else, with inheritance blocked.
pub fn current_user_only_sddl(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})")
}

/// A descriptor whose DACL is protected and holds a single allow entry for the current user.
/// Administrators and SYSTEM get no entry; the pipe is for the interactive user only.
pub fn current_user_only() -> Result<OwnedSecurityDescriptor> {
    let sddl = current_user_only_sddl(&current_user_sid_string()?);
    let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `wide` is a NUL terminated UTF-16 string that outlives the call and `sd` is a valid
    // out pointer.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(wide.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
    }
    .context("ConvertStringSecurityDescriptorToSecurityDescriptorW")?;
    Ok(OwnedSecurityDescriptor { ptr: sd })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sid_is_a_user_sid() {
        let sid = current_user_sid_string().unwrap();
        // S-1-5-21-... for local and domain accounts, S-1-12-1-... for Entra ID (Azure AD) accounts.
        assert!(sid.starts_with("S-1-"), "{sid}");
    }

    #[test]
    fn sddl_has_exactly_one_entry_and_is_protected() {
        let sddl = current_user_only_sddl("S-1-5-21-1-2-3-1001");
        assert_eq!(sddl, "D:P(A;;GA;;;S-1-5-21-1-2-3-1001)");
        assert_eq!(sddl.matches('(').count(), 1);
    }

    #[test]
    fn descriptor_can_be_built() {
        let sd = current_user_only().unwrap();
        assert!(!sd.as_ptr().is_null());
    }
}
