// SPDX-License-Identifier: Apache-2.0
//! Process helpers for Office automation: find, identify and stop the Office process a render
//! worker started.
//!
//! An Office instance created through COM is launched by the DCOM service, so it is not a child
//! of the worker and the worker's job object does not contain it. Cleanup therefore works by
//! identity: the process id the worker recorded, or - after a worker that was killed before it
//! could record anything - every Office process that was *started after the job began* and still
//! carries the `/automation` or `-Embedding` switch DCOM launches servers with. An Office the
//! user started themselves has neither property and is never touched.

use std::mem::{size_of, zeroed};
use std::time::Duration;

use windows_sys::Wdk::System::Threading::NtQueryInformationProcess;
use windows_sys::Win32::Foundation::{
    CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE, UNICODE_STRING, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows_sys::Win32::System::Threading::{
    BELOW_NORMAL_PRIORITY_CLASS, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SET_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, QueryFullProcessImageNameW,
    SetPriorityClass, TerminateProcess, WaitForSingleObject,
};

/// `ProcessCommandLineInformation` of `NtQueryInformationProcess` (Windows 8.1 and later).
const PROCESS_COMMAND_LINE_INFORMATION: i32 = 60;

/// Executable names of the Office applications Gander automates.
pub const OFFICE_EXES: [&str; 3] = ["POWERPNT.EXE", "WINWORD.EXE", "EXCEL.EXE"];

/// An owned process handle.
struct Proc(HANDLE);

impl Proc {
    fn open(pid: u32, access: u32) -> Option<Self> {
        // SAFETY: plain OpenProcess; a null result means failure.
        let h = unsafe { OpenProcess(access, 0, pid) };
        (!h.is_null()).then_some(Proc(h))
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by OpenProcess and is closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

/// The current time as FILETIME ticks (100 ns since 1601), the unit of process creation times.
pub fn now_filetime() -> u64 {
    let mut ft = FILETIME::default();
    // SAFETY: valid out-pointer.
    unsafe { GetSystemTimeAsFileTime(&mut ft) };
    ticks(ft)
}

fn ticks(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

/// Process ids of every running process whose image name is `exe_name` (case-insensitive).
pub fn pids_named(exe_name: &str) -> Vec<u32> {
    let mut out = Vec::new();
    // SAFETY: snapshot handle is checked and closed; the entry struct is sized before the first
    // call as the API requires.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut entry);
        while ok != 0 {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if name.eq_ignore_ascii_case(exe_name) {
                out.push(entry.th32ProcessID);
            }
            ok = Process32NextW(snap, &mut entry);
        }
        CloseHandle(snap);
    }
    out
}

/// Whether any Office application is running right now (started by anyone).
pub fn any_office_running() -> bool {
    OFFICE_EXES.iter().any(|exe| !pids_named(exe).is_empty())
}

/// Creation time of `pid` in FILETIME ticks.
pub fn started_at(pid: u32) -> Option<u64> {
    let p = Proc::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let (mut c, mut e, mut k, mut u) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: valid handle and out-pointers.
    (unsafe { GetProcessTimes(p.0, &mut c, &mut e, &mut k, &mut u) } != 0).then(|| ticks(c))
}

/// Full image path of the process behind `p`.
fn image_path(p: &Proc) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: valid handle; `buf`/`len` describe a writable buffer.
    (unsafe { QueryFullProcessImageNameW(p.0, 0, buf.as_mut_ptr(), &mut len) } != 0)
        .then(|| String::from_utf16_lossy(&buf[..len as usize]))
}

/// The command line of `pid`, if it can be read (same-user processes can).
pub fn command_line(pid: u32) -> Option<String> {
    let p = Proc::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut buf = vec![0u8; 8192];
    let mut needed = 0u32;
    // SAFETY: valid handle; the buffer is writable for its stated length.
    let status = unsafe {
        NtQueryInformationProcess(
            p.0,
            PROCESS_COMMAND_LINE_INFORMATION,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            &mut needed,
        )
    };
    if status < 0 || buf.len() < size_of::<UNICODE_STRING>() {
        return None;
    }
    // SAFETY: on success the buffer starts with a UNICODE_STRING whose `Buffer` points into the
    // same allocation (the data follows the header); `Length` bytes of it are valid UTF-16. The
    // bounds are checked against the buffer before the slice is formed.
    unsafe {
        let us = std::ptr::read_unaligned(buf.as_ptr().cast::<UNICODE_STRING>());
        let start = buf.as_ptr() as usize;
        let at = us.Buffer as usize;
        let units = usize::from(us.Length) / 2;
        if at < start || at + units * 2 > start + buf.len() || !at.is_multiple_of(2) {
            return None;
        }
        let wide = std::slice::from_raw_parts(at as *const u16, units);
        Some(String::from_utf16_lossy(wide))
    }
}

/// Whether `command_line` carries the switch DCOM launches an automation server with.
pub fn is_automation_command_line(command_line: &str) -> bool {
    let lower = command_line.to_ascii_lowercase();
    lower.contains("/automation")
        || lower.contains("-automation")
        || lower.contains("-embedding")
        || lower.contains("/embedding")
}

/// Terminates `pid` if it is a process named `exe_name` that started at or after `since`
/// (FILETIME ticks). Guards against a recycled process id. Returns whether it was terminated.
pub fn kill_if(pid: u32, exe_name: &str, since: u64) -> bool {
    let Some(p) = Proc::open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE) else {
        return false;
    };
    let named = image_path(&p).is_some_and(|path| {
        path.rsplit(['\\', '/'])
            .next()
            .is_some_and(|n| n.eq_ignore_ascii_case(exe_name))
    });
    if !named || started_at(pid).is_none_or(|t| t < since) {
        return false;
    }
    // SAFETY: valid handle with PROCESS_TERMINATE.
    unsafe { TerminateProcess(p.0, 1) != 0 }
}

/// Waits up to `timeout` for `pid` to exit; true when it has (or never existed).
pub fn wait_exit(pid: u32, timeout: Duration) -> bool {
    let Some(p) = Proc::open(pid, PROCESS_SYNCHRONIZE) else {
        return true;
    };
    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    // SAFETY: valid handle with SYNCHRONIZE.
    unsafe { WaitForSingleObject(p.0, ms) == WAIT_OBJECT_0 }
}

/// Lowers `pid` to below-normal priority ("the computer, a little"). Best effort.
pub fn set_below_normal(pid: u32) {
    if let Some(p) = Proc::open(pid, PROCESS_SET_INFORMATION) {
        // SAFETY: valid handle with PROCESS_SET_INFORMATION.
        unsafe { SetPriorityClass(p.0, BELOW_NORMAL_PRIORITY_CLASS) };
    }
}

/// Kills every Office process that was launched for automation at or after `since` (FILETIME
/// ticks). Used after a worker that may have been killed mid-activation. Returns how many were
/// terminated.
pub fn sweep_stray_office(since: u64) -> usize {
    OFFICE_EXES
        .iter()
        .map(|exe| sweep_automation(exe, since))
        .sum()
}

/// Kills every `exe_name` process that carries the automation switch and was started at or
/// after `since` (FILETIME ticks). An instance the user started (no switch) or one that is older
/// than `since` is never touched. Returns how many were terminated.
pub fn sweep_automation(exe_name: &str, since: u64) -> usize {
    pids_named(exe_name)
        .into_iter()
        .filter(|&pid| {
            let automation = command_line(pid).is_some_and(|c| is_automation_command_line(&c));
            automation && kill_if(pid, exe_name, since)
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_switch_is_recognised_case_insensitively() {
        let dcom = r#""C:\Program Files\Microsoft Office\Root\Office16\POWERPNT.EXE" /AUTOMATION"#;
        assert!(is_automation_command_line(dcom));
        assert!(is_automation_command_line("WINWORD.EXE -Embedding"));
        assert!(!is_automation_command_line(
            r#""C:\Office\POWERPNT.EXE" "D:\decks\q3.pptx""#
        ));
        assert!(!is_automation_command_line("EXCEL.EXE"));
    }

    #[test]
    fn own_process_is_found_and_never_killed_by_a_stale_filter() {
        let me = std::process::id();
        let exe = std::env::current_exe().unwrap();
        let name = exe.file_name().unwrap().to_string_lossy().into_owned();
        assert!(pids_named(&name).contains(&me));
        assert!(started_at(me).is_some());
        // Wrong name: refused. Right name but "started after now": refused.
        assert!(!kill_if(me, "definitely-not-this.exe", 0));
        assert!(!kill_if(me, &name, now_filetime() + 10_000_000_000));
        let cmd = command_line(me).expect("own command line");
        assert!(!is_automation_command_line(&cmd), "{cmd}");
    }

    #[test]
    fn missing_process_counts_as_exited() {
        assert!(wait_exit(u32::MAX - 1, Duration::from_millis(10)));
    }
}
