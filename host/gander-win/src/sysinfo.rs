// SPDX-License-Identifier: Apache-2.0
//! Host facts for `GET /v1/me` and the pairing response (WP-14).

use windows_sys::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};

const CURRENT_VERSION_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn read_dword(value: &str) -> Option<u32> {
    let key = wide(CURRENT_VERSION_KEY);
    let name = wide(value);
    let mut data: u32 = 0;
    let mut len = size_of::<u32>() as u32;
    // SAFETY: NUL-terminated key and value names; `data`/`len` describe a 4-byte buffer.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut data).cast(),
            &mut len,
        )
    };
    (rc == 0).then_some(data)
}

fn read_string(value: &str) -> Option<String> {
    let key = wide(CURRENT_VERSION_KEY);
    let name = wide(value);
    let mut buf = [0u16; 64];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: NUL-terminated key and value names; `buf`/`len` describe a writable buffer in
    // bytes. RRF_RT_REG_SZ guarantees a NUL-terminated result on success.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return None;
    }
    let chars = (len as usize / 2).min(buf.len());
    let text = &buf[..chars];
    let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    String::from_utf16(&text[..end]).ok()
}

/// The Windows version as `major.minor.build`, for example `10.0.26200`. Read from the registry
/// (`GetVersionEx` reports a compatibility version to unmanifested processes). Empty when the
/// values cannot be read.
pub fn os_version() -> String {
    let (Some(major), Some(minor), Some(build)) = (
        read_dword("CurrentMajorVersionNumber"),
        read_dword("CurrentMinorVersionNumber"),
        read_string("CurrentBuildNumber"),
    ) else {
        return String::new();
    };
    if build.is_empty() || !build.bytes().all(|b| b.is_ascii_digit()) {
        return String::new();
    }
    format!("{major}.{minor}.{build}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_has_three_numeric_parts() {
        let v = super::os_version();
        let parts: Vec<&str> = v.split('.').collect();
        assert_eq!(parts.len(), 3, "{v}");
        assert!(parts.iter().all(|p| p.parse::<u32>().is_ok()), "{v}");
    }
}
