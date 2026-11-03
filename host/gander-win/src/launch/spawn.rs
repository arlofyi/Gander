// SPDX-License-Identifier: Apache-2.0
//! Starting the app: `CreateProcessW` for desktop apps, `IApplicationActivationManager` for
//! packaged apps. Never `ShellExecute`, never a shell.

use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;

use gander_core::launch::command::LaunchCommand;
use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, IDataObject,
};
use windows::Win32::UI::Shell::{
    ASSOC_FILTER_NONE, ApplicationActivationManager, BHID_DataObject,
    IApplicationActivationManager, IAssocHandler, IObjectWithAppUserModelID, IShellItem,
    IShellItemArray, SHAssocEnumHandlers, SHCreateItemFromParsingName,
    SHCreateShellItemArrayFromShellItem,
};
use windows::core::{HSTRING, Interface as _, w};
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    CREATE_DEFAULT_ERROR_MODE, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    PROCESS_INFORMATION, STARTUPINFOW,
};

use super::LaunchError;

/// `ERROR_ELEVATION_REQUIRED`: the app's manifest asks for administrator rights.
const ERROR_ELEVATION_REQUIRED: i32 = 740;

/// A started process. The handle is held until discovery ends so the pid cannot be reused by
/// another process in the meantime.
#[derive(Debug)]
pub(crate) struct Spawned {
    pub(crate) pid: u32,
    _process: Option<OwnedHandle>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `CreateProcessW(lpApplicationName = program, lpCommandLine = quoted line)`: no search path, no
/// shell, no inherited handles. The working directory is the program's own folder (not the
/// share), and `CREATE_NO_WINDOW` keeps a console-subsystem handler from opening a console
/// window that discovery would have to ignore.
pub(crate) fn spawn(cmd: &LaunchCommand) -> Result<Spawned, LaunchError> {
    let app = wide(&cmd.program);
    let mut line = wide(&cmd.command_line());
    let cwd = Path::new(&cmd.program)
        .parent()
        .map(|p| wide(&p.to_string_lossy()));
    // SAFETY: all-zero STARTUPINFOW / PROCESS_INFORMATION are valid initial values.
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    // SAFETY: as above.
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `app`, `cwd` are NUL terminated and outlive the call; `line` is a mutable
    // NUL-terminated buffer as CreateProcessW requires; `si`/`pi` are valid.
    let ok = unsafe {
        CreateProcessW(
            app.as_ptr(),
            line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            CREATE_UNICODE_ENVIRONMENT | CREATE_DEFAULT_ERROR_MODE | CREATE_NO_WINDOW,
            ptr::null(),
            cwd.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
            &si,
            &mut pi,
        )
    };
    if ok == 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED) {
            return Err(LaunchError::NeedsElevation);
        }
        return Err(LaunchError::Spawn(err));
    }
    // SAFETY: the thread handle is ours and unused; the process handle is wrapped once and
    // closed by OwnedHandle's Drop.
    let process = unsafe {
        CloseHandle(pi.hThread);
        OwnedHandle::from_raw_handle(pi.hProcess)
    };
    Ok(Spawned {
        pid: pi.dwProcessId,
        _process: Some(process),
    })
}

/// COM for the current thread, uninitialised on drop if this call initialised it.
struct ComScope {
    initialised: bool,
}

impl ComScope {
    fn new() -> Self {
        // SAFETY: plain call; S_OK and S_FALSE both need a matching CoUninitialize,
        // RPC_E_CHANGED_MODE (already MTA) does not and is fine for this use.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        Self {
            initialised: hr.is_ok(),
        }
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.initialised {
            // SAFETY: balances the successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// Activates a packaged app for `file`; returns the app's process id, or 0 when the shell
/// started it without saying which process (the discovery then links the window to the launch
/// by package family).
///
/// 1. The shell association handler of the file's extension whose AppUserModelID is exactly
///    `aumid` (`SHAssocEnumHandlers`, `IAssocHandler::Invoke`): the app the policy approved,
///    through the path the "Open with" menu uses. The file itself is never executed and no
///    other handler is ever chosen. This is the only path that works for packaged *desktop*
///    apps (WinUI 3 or Win32 in a package: Microsoft Photos since 2023, Notepad on Windows 11):
///    `ActivateForFile` refuses them with `0x80270254` ("This app does not support the contract
///    specified"), after 0.7 to 2.5 s for Photos and 12 to 60 s for Notepad (measured on
///    Windows 11 build 26200, 2026-11-02).
/// 2. `IApplicationActivationManager::ActivateForFile` with the `open` verb (the UWP file
///    contract) when no registered handler carries that id.
pub(crate) fn activate_packaged(aumid: &str, file: &Path) -> Result<Spawned, LaunchError> {
    let _com = ComScope::new();
    let pid = match invoke_handler(aumid, file) {
        Ok(()) => 0,
        Err(_) => activate_for_file(aumid, file)?,
    };
    Ok(Spawned {
        pid,
        _process: None,
    })
}

fn activation(e: windows::core::Error) -> LaunchError {
    LaunchError::Activation(e.code().0)
}

fn activate_for_file(aumid: &str, file: &Path) -> Result<u32, LaunchError> {
    let path = HSTRING::from(file.as_os_str());
    let aumid = HSTRING::from(aumid);
    // SAFETY: COM is initialised on this thread; the strings outlive the calls; the interfaces
    // are released when they go out of scope.
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&path, None).map_err(activation)?;
        let items: IShellItemArray =
            SHCreateShellItemArrayFromShellItem(&item).map_err(activation)?;
        let manager: IApplicationActivationManager =
            CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER)
                .map_err(activation)?;
        manager
            .ActivateForFile(&aumid, &items, w!("open"))
            .map_err(activation)
    }
}

/// `HRESULT_FROM_WIN32(ERROR_NO_ASSOCIATION)`.
const HR_NO_ASSOCIATION: i32 = 0x8007_0483_u32 as i32;

/// Invokes, on `file`, the association handler of its extension whose AppUserModelID is
/// `aumid`. Fails when no registered handler has that id.
fn invoke_handler(aumid: &str, file: &Path) -> Result<(), LaunchError> {
    let ext = file
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .ok_or(LaunchError::Activation(HR_NO_ASSOCIATION))?;
    let ext = HSTRING::from(ext);
    let path = HSTRING::from(file.as_os_str());
    // SAFETY: COM is initialised on this thread (the caller's ComScope); the strings outlive the
    // calls; the id string the shell returns is freed with CoTaskMemFree; interfaces are
    // released when they go out of scope.
    unsafe {
        let handlers = SHAssocEnumHandlers(&ext, ASSOC_FILTER_NONE).map_err(activation)?;
        loop {
            let mut slot: [Option<IAssocHandler>; 1] = [None];
            let mut fetched = 0u32;
            if handlers.Next(&mut slot, Some(&mut fetched)).is_err() || fetched == 0 {
                return Err(LaunchError::Activation(HR_NO_ASSOCIATION));
            }
            let Some(handler) = slot[0].take() else {
                continue;
            };
            let Ok(with_id) = handler.cast::<IObjectWithAppUserModelID>() else {
                continue;
            };
            let Ok(id) = with_id.GetAppID() else {
                continue;
            };
            let same = id.to_string().is_ok_and(|s| s.eq_ignore_ascii_case(aumid));
            CoTaskMemFree(Some(id.0 as *const _));
            if !same {
                continue;
            }
            let item: IShellItem = SHCreateItemFromParsingName(&path, None).map_err(activation)?;
            let data: IDataObject = item
                .BindToHandler(None, &BHID_DataObject)
                .map_err(activation)?;
            return handler.Invoke(&data).map_err(activation);
        }
    }
}
