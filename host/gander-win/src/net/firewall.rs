// SPDX-License-Identifier: Apache-2.0
//! Read-only access to the Windows Firewall rule store for the connectivity self-check.
//!
//! Rules are read from the registry, which standard users may read:
//!
//! - `HKLM\SYSTEM\CurrentControlSet\Services\SharedAccess\Parameters\FirewallPolicy\FirewallRules`
//!   (local rules, including the Block rules a dismissed first-listen prompt creates),
//! - `HKLM\SOFTWARE\Policies\Microsoft\WindowsFirewall\FirewallRules` (Group Policy rules).
//!
//! Nothing here creates, edits or deletes a rule. `App=` paths have environment variables
//! expanded before matching.

use std::io;

use gander_core::net::firewall::{FirewallRule, blocking_rules, parse_rule};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_EXPAND_SZ, REG_SZ, RegCloseKey, RegEnumValueW,
    RegOpenKeyExW, RegQueryInfoKeyW,
};

/// Registry keys holding firewall rules.
pub const RULE_KEYS: [&str; 2] = [
    r"SYSTEM\CurrentControlSet\Services\SharedAccess\Parameters\FirewallPolicy\FirewallRules",
    r"SOFTWARE\Policies\Microsoft\WindowsFirewall\FirewallRules",
];

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Expands `%VAR%` references.
pub fn expand_env(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let src = wide(s);
    let mut buf = vec![0u16; 1024];
    for _ in 0..3 {
        // SAFETY: `src` is NUL-terminated; `buf` has `buf.len()` writable u16s.
        let n =
            unsafe { ExpandEnvironmentStringsW(src.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) }
                as usize;
        if n == 0 {
            return s.to_owned();
        }
        if n <= buf.len() {
            return String::from_utf16_lossy(&buf[..n.saturating_sub(1)]);
        }
        buf = vec![0u16; n];
    }
    s.to_owned()
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key was opened by RegOpenKeyExW.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

fn read_key(path: &str) -> io::Result<Vec<FirewallRule>> {
    let mut hkey: HKEY = std::ptr::null_mut();
    let wpath = wide(path);
    // SAFETY: valid NUL-terminated path; out pointer is live.
    let rc = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, wpath.as_ptr(), 0, KEY_READ, &mut hkey) };
    if rc == ERROR_FILE_NOT_FOUND {
        return Ok(Vec::new());
    }
    if rc != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    let key = Key(hkey);
    let (mut max_name, mut max_data) = (0u32, 0u32);
    // SAFETY: only the two out values are requested; the rest are null.
    let rc = unsafe {
        RegQueryInfoKeyW(
            key.0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut max_name,
            &mut max_data,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    let mut name = vec![0u16; max_name as usize + 1];
    let mut data = vec![0u16; (max_data as usize).div_ceil(2) + 1];
    let mut rules = Vec::new();
    let mut index = 0u32;
    loop {
        let mut name_len = name.len() as u32;
        let mut data_len = (data.len() * 2) as u32;
        let mut kind = 0u32;
        // SAFETY: buffers sized from RegQueryInfoKeyW; lengths passed in/out.
        let rc = unsafe {
            RegEnumValueW(
                key.0,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                std::ptr::null(),
                &mut kind,
                data.as_mut_ptr().cast(),
                &mut data_len,
            )
        };
        index += 1;
        if rc == ERROR_NO_MORE_ITEMS {
            break;
        }
        if rc != ERROR_SUCCESS {
            // A value added while enumerating can exceed the buffer; skip it.
            continue;
        }
        if kind != REG_SZ && kind != REG_EXPAND_SZ {
            continue;
        }
        let id = String::from_utf16_lossy(&name[..name_len as usize]);
        let units = (data_len as usize / 2).min(data.len());
        let text = String::from_utf16_lossy(&data[..units]);
        let text = text.trim_end_matches('\0');
        if let Some(mut rule) = parse_rule(&id, text) {
            rule.app = rule.app.map(|a| expand_env(&a));
            rules.push(rule);
        }
    }
    Ok(rules)
}

/// Every firewall rule from the local and Group Policy stores. Errors of one store are returned
/// alongside the rules of the others.
pub fn read_rules() -> (Vec<FirewallRule>, Vec<String>) {
    let mut rules = Vec::new();
    let mut errors = Vec::new();
    for key in RULE_KEYS {
        match read_key(key) {
            Ok(mut r) => rules.append(&mut r),
            Err(e) => errors.push(format!("cannot read firewall rules ({key}): {e}")),
        }
    }
    (rules, errors)
}

/// Active inbound Block rules naming `exe` or mentioning `marker` in their name.
pub fn find_blocking_rules(exe: &str, marker: &str) -> (Vec<FirewallRule>, Vec<String>) {
    let (rules, errors) = read_rules();
    let found = blocking_rules(&rules, exe, marker)
        .into_iter()
        .cloned()
        .collect();
    (found, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_rule_store_unelevated() {
        let (rules, errors) = read_rules();
        assert!(errors.is_empty(), "{errors:?}");
        assert!(
            !rules.is_empty(),
            "a Windows install always has built-in rules"
        );
        assert!(
            rules
                .iter()
                .all(|r| r.app.as_deref().is_none_or(|a| !a.contains("%SystemRoot%")))
        );
    }

    #[test]
    fn expands_environment() {
        let e = expand_env(r"%SystemRoot%\system32\svchost.exe");
        assert!(!e.contains('%'));
        assert!(e.to_ascii_lowercase().ends_with(r"\system32\svchost.exe"));
        assert_eq!(expand_env("plain"), "plain");
    }
}
