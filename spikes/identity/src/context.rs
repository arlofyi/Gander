// SPDX-License-Identifier: Apache-2.0
//! The account context the spike runs in: is the process elevated, and at what integrity level.
//! This is what the plan asks to record when the LocalAPI pipe refuses a process.

#[derive(Debug, Default, Clone)]
pub struct ProcessContext {
    pub elevated: Option<bool>,
    /// `default` (UAC off or a non-admin account), `full` (elevated admin) or `limited`
    /// (filtered token of an admin account).
    pub elevation_type: Option<&'static str>,
    pub integrity: Option<&'static str>,
    pub windows_user: Option<String>,
}

#[cfg(windows)]
pub fn detect() -> ProcessContext {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_ELEVATION,
        TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenElevation, TokenElevationType,
        TokenIntegrityLevel,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut ctx = ProcessContext {
        windows_user: std::env::var("USERNAME").ok(),
        ..Default::default()
    };
    let mut token = std::ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo handle; `token` is a valid out-pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return ctx;
    }

    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut written = 0u32;
    // SAFETY: `token` is a valid token handle; the buffer is a TOKEN_ELEVATION of the size given.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            std::ptr::from_mut(&mut elevation).cast(),
            u32::try_from(size_of::<TOKEN_ELEVATION>()).unwrap_or(0),
            &mut written,
        )
    };
    if ok != 0 {
        ctx.elevated = Some(elevation.TokenIsElevated != 0);
    }

    let mut kind = 0i32;
    // SAFETY: as above with a 4-byte integer buffer.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevationType,
            std::ptr::from_mut(&mut kind).cast(),
            4,
            &mut written,
        )
    };
    if ok != 0 {
        ctx.elevation_type = Some(match kind {
            1 => "default",
            2 => "full",
            3 => "limited",
            _ => "unknown",
        });
    }

    // Integrity level: the last sub-authority of the label SID (0x1000 low, 0x2000 medium,
    // 0x3000 high, 0x4000 system).
    let mut buf = vec![0u8; 256];
    // SAFETY: the buffer is 256 bytes and its length is passed; `written` receives the need.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenIntegrityLevel,
            buf.as_mut_ptr().cast(),
            256,
            &mut written,
        )
    };
    if ok != 0 {
        // SAFETY: on success the buffer starts with a TOKEN_MANDATORY_LABEL whose SID points
        // into the same buffer.
        let rid = unsafe {
            let label = buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
            let sid = (*label).Label.Sid;
            let count = *GetSidSubAuthorityCount(sid);
            (count > 0).then(|| *GetSidSubAuthority(sid, u32::from(count) - 1))
        };
        ctx.integrity = rid.map(|r| match r {
            0..0x2000 => "low",
            0x2000..0x3000 => "medium",
            0x3000..0x4000 => "high",
            _ => "system",
        });
    }

    // SAFETY: `token` was opened above and is closed once.
    unsafe {
        CloseHandle(token);
    }
    ctx
}

#[cfg(not(windows))]
pub fn detect() -> ProcessContext {
    ProcessContext {
        windows_user: std::env::var("USER").ok(),
        ..Default::default()
    }
}
