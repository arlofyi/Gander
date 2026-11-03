// SPDX-License-Identifier: Apache-2.0
//! Thin, safe wrappers over the NT and Win32 calls the path resolver needs. Every `unsafe` block
//! is here; `mod.rs` holds the policy.

use std::io;
use std::mem::{offset_of, size_of};
use std::ops::ControlFlow;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_OPEN, FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
    NtCreateFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_NO_MORE_FILES, GetLastError, HANDLE, INVALID_HANDLE_VALUE, NTSTATUS,
    OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING,
};
use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_ID_BOTH_DIR_INFO, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_READ_DATA,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileAttributeTagInfo,
    FileBasicInfo, FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo, FileStandardInfo,
    GetFileInformationByHandleEx, GetFinalPathNameByHandleW, OPEN_EXISTING, ReOpenFile,
    SYNCHRONIZE, VOLUME_NAME_DOS, VOLUME_NAME_NONE,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

/// Read data / list directory, read attributes, and wait on the handle (synchronous I/O).
/// `FILE_READ_DATA` and `FILE_LIST_DIRECTORY` are the same bit, so one mask fits both kinds.
const ACCESS: u32 = FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
/// Never block the owner's own apps.
const SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

pub(super) const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
pub(super) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// `NtCreateFile(RootDirectory = parent, ObjectName = name)` with `FILE_OPEN_REPARSE_POINT`, so
/// no reparse point is ever followed. An empty `name` opens a new handle to `parent` itself.
/// Returns the NTSTATUS on failure.
pub(super) fn open_relative(
    parent: BorrowedHandle<'_>,
    name: &[u16],
) -> Result<OwnedHandle, NTSTATUS> {
    let byte_len = u16::try_from(name.len() * 2).map_err(|_| STATUS_OBJECT_NAME_INVALID)?;
    let uname = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        // NtCreateFile does not write through ObjectName.
        Buffer: name.as_ptr().cast_mut(),
    };
    let attrs = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &uname,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: ptr::null(),
        SecurityQualityOfService: ptr::null(),
    };
    let mut handle: HANDLE = ptr::null_mut();
    let mut iosb = IO_STATUS_BLOCK::default();
    // SAFETY: every pointer refers to a live local (`uname`, `name` via `uname.Buffer`, `attrs`,
    // `handle`, `iosb`) that outlives the call; no EA buffer is passed.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            ACCESS,
            &attrs,
            &mut iosb,
            ptr::null(),
            0,
            SHARE,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(status);
    }
    // SAFETY: NtCreateFile succeeded, so `handle` is a valid handle we now own.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// `STATUS_OBJECT_NAME_INVALID`.
pub(super) const STATUS_OBJECT_NAME_INVALID: NTSTATUS = 0xC000_0033_u32 as i32;

/// Converts an NTSTATUS to an `io::Error` carrying the matching Win32 error code.
pub(super) fn status_to_io(status: NTSTATUS) -> io::Error {
    // SAFETY: pure conversion function without pointers.
    let code = unsafe { RtlNtStatusToDosError(status) };
    io::Error::from_raw_os_error(code as i32)
}

/// Opens the share root BY PATH (the only path-based open), following a root that is itself a
/// link: the spec resolves the root once, at share creation and at startup.
pub(super) fn open_root(path: &Path) -> io::Result<OwnedHandle> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call; no security attributes or template.
    let h = unsafe {
        CreateFileW(
            wide.as_ptr(),
            ACCESS,
            SHARE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW succeeded; we own the handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(h) })
}

/// Opens a new handle to the same file object WITHOUT `FILE_OPEN_REPARSE_POINT`, so filters
/// that own an allowed reparse tag (cloud files, WOF, dedup) serve the real content. The open is
/// relative to the existing handle; no path is parsed.
pub(super) fn reopen(handle: BorrowedHandle<'_>) -> io::Result<OwnedHandle> {
    // SAFETY: `handle` is a valid borrowed handle for the duration of the call.
    let h = unsafe {
        ReOpenFile(
            handle.as_raw_handle(),
            ACCESS,
            SHARE,
            FILE_FLAG_BACKUP_SEMANTICS,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: ReOpenFile succeeded; we own the new handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(h) })
}

fn query<T: Default>(handle: BorrowedHandle<'_>, class: i32) -> io::Result<T> {
    let mut info = T::default();
    // SAFETY: `info` is a properly aligned, writable `T` of the size passed; `class` matches `T`
    // at every call site in this module.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle.as_raw_handle(),
            class,
            (&mut info as *mut T).cast(),
            size_of::<T>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}

/// `(FileAttributes, ReparseTag)` of the opened item, from the handle.
pub(super) fn attribute_tag(handle: BorrowedHandle<'_>) -> io::Result<(u32, u32)> {
    let info: FILE_ATTRIBUTE_TAG_INFO = query(handle, FileAttributeTagInfo)?;
    let tag = if info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        info.ReparseTag
    } else {
        0
    };
    Ok((info.FileAttributes, tag))
}

/// Basic and standard information: `(attributes, creation, last_write, size, is_dir)`.
pub(super) fn stat(handle: BorrowedHandle<'_>) -> io::Result<(u32, i64, i64, u64, bool)> {
    let basic: FILE_BASIC_INFO = query(handle, FileBasicInfo)?;
    let standard: FILE_STANDARD_INFO = query(handle, FileStandardInfo)?;
    Ok((
        basic.FileAttributes,
        basic.CreationTime,
        basic.LastWriteTime,
        u64::try_from(standard.EndOfFile).unwrap_or(0),
        standard.Directory,
    ))
}

fn final_path(handle: BorrowedHandle<'_>, flags: u32) -> io::Result<Vec<u16>> {
    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: `buf` is writable for `buf.len()` u16s.
        let n = unsafe {
            GetFinalPathNameByHandleW(
                handle.as_raw_handle(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                flags,
            )
        } as usize;
        if n == 0 {
            return Err(io::Error::last_os_error());
        }
        if n < buf.len() {
            buf.truncate(n);
            return Ok(buf);
        }
        // `n` is the required size including the terminator.
        buf.resize(n + 1, 0);
    }
}

/// `GetFinalPathNameByHandleW(VOLUME_NAME_DOS | FILE_NAME_NORMALIZED)`: `\\?\C:\...`.
pub(super) fn final_dos_path(handle: BorrowedHandle<'_>) -> io::Result<Vec<u16>> {
    final_path(handle, VOLUME_NAME_DOS | FILE_NAME_NORMALIZED)
}

/// The stored (normalised: long, as-cased) name of the last component of the opened item.
pub(super) fn stored_name(handle: BorrowedHandle<'_>) -> io::Result<Vec<u16>> {
    let full = final_path(handle, VOLUME_NAME_NONE | FILE_NAME_NORMALIZED)?;
    let start = full
        .iter()
        .rposition(|&c| c == u16::from(b'\\'))
        .map_or(0, |i| i + 1);
    Ok(full[start..].to_vec())
}

/// Ordinal, case-insensitive UTF-16 equality (the file system's own case rule is upper-case
/// ordinal, which is what `CompareStringOrdinal(.., TRUE)` implements).
pub(super) fn eq_ignore_case(a: &[u16], b: &[u16]) -> bool {
    let (Ok(la), Ok(lb)) = (i32::try_from(a.len()), i32::try_from(b.len())) else {
        return false;
    };
    // SAFETY: both slices are valid for the lengths passed.
    unsafe { CompareStringOrdinal(a.as_ptr(), la, b.as_ptr(), lb, 1) == CSTR_EQUAL }
}

/// One raw entry of `FileIdBothDirectoryInfo`.
#[derive(Debug)]
pub(super) struct RawDirEntry<'a> {
    pub name: &'a [u16],
    pub attributes: u32,
    pub reparse_tag: u32,
    pub size: u64,
    pub creation: i64,
    pub last_write: i64,
}

/// Enumerates the directory behind `handle` (from the start) and calls `f` per entry until it
/// returns `Break` or the directory ends.
pub(super) fn enumerate(
    handle: BorrowedHandle<'_>,
    mut f: impl FnMut(RawDirEntry<'_>) -> ControlFlow<()>,
) -> io::Result<()> {
    // 64 KiB, 8-byte aligned.
    let mut buf = vec![0u64; 8 * 1024];
    let byte_len = (buf.len() * 8) as u32;
    let mut class = FileIdBothDirectoryRestartInfo;
    loop {
        // SAFETY: `buf` is writable and aligned for FILE_ID_BOTH_DIR_INFO records.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle.as_raw_handle(),
                class,
                buf.as_mut_ptr().cast(),
                byte_len,
            )
        };
        if ok == 0 {
            // SAFETY: reads the calling thread's last-error value.
            let err = unsafe { GetLastError() };
            if err == ERROR_NO_MORE_FILES {
                return Ok(());
            }
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        class = FileIdBothDirectoryInfo;
        let base = buf.as_ptr().cast::<u8>();
        let mut offset = 0usize;
        loop {
            let header_end = offset + offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
            if header_end > byte_len as usize {
                return Err(io::Error::other("directory record out of bounds"));
            }
            // SAFETY: `offset..header_end` lies inside `buf` (checked above); the read is
            // unaligned-safe.
            let info =
                unsafe { ptr::read_unaligned(base.add(offset).cast::<FILE_ID_BOTH_DIR_INFO>()) };
            let name_units = info.FileNameLength as usize / 2;
            if header_end + name_units * 2 > byte_len as usize {
                return Err(io::Error::other("directory record name out of bounds"));
            }
            // SAFETY: the name lies inside `buf` (checked above); records are 8-byte aligned and
            // the name offset is even, so the u16 slice is aligned. `buf` is not mutated while
            // the slice is alive.
            let name = unsafe {
                std::slice::from_raw_parts(base.add(header_end).cast::<u16>(), name_units)
            };
            let is_reparse = info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
            let entry = RawDirEntry {
                name,
                attributes: info.FileAttributes,
                // For reparse points the EaSize field carries the reparse tag.
                reparse_tag: if is_reparse { info.EaSize } else { 0 },
                size: u64::try_from(info.EndOfFile).unwrap_or(0),
                creation: info.CreationTime,
                last_write: info.LastWriteTime,
            };
            if f(entry).is_break() {
                return Ok(());
            }
            if info.NextEntryOffset == 0 {
                break;
            }
            offset += info.NextEntryOffset as usize;
        }
    }
}
