// SPDX-License-Identifier: Apache-2.0
//! Content identity of an open file (volume serial, 128-bit file id, size, last-write time) and
//! the shell thumbnail-provider lookup. Windows only.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_BASIC_INFO, FILE_ID_INFO, FILE_STANDARD_INFO, FileBasicInfo, FileIdInfo, FileStandardInfo,
    GetFileInformationByHandleEx,
};
use windows_sys::Win32::UI::Shell::{ASSOCF_NONE, ASSOCSTR_SHELLEXTENSION, AssocQueryStringW};

use super::cache::FileIdentity;

/// Interface id of `IThumbnailProvider`, the shell-extension key a thumbnail handler registers.
const THUMBNAIL_PROVIDER_IID: &str = "{e357fccd-a995-4576-b01f-234630154e96}";

/// Reads the identity of an open file. Moving or renaming the file keeps it; writing to it
/// changes size or last-write time.
pub fn file_identity(file: &File) -> io::Result<FileIdentity> {
    let h = file.as_raw_handle() as HANDLE;
    // SAFETY: zeroed plain-old-data structs; each call receives a buffer of the size of the
    // struct for its information class, and `h` is a valid open handle for the call.
    unsafe {
        let mut id: FILE_ID_INFO = zeroed();
        if GetFileInformationByHandleEx(
            h,
            FileIdInfo,
            (&raw mut id).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut basic: FILE_BASIC_INFO = zeroed();
        if GetFileInformationByHandleEx(
            h,
            FileBasicInfo,
            (&raw mut basic).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut std_info: FILE_STANDARD_INFO = zeroed();
        if GetFileInformationByHandleEx(
            h,
            FileStandardInfo,
            (&raw mut std_info).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        if std_info.Directory {
            return Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                "thumbnails are for files",
            ));
        }
        Ok(FileIdentity {
            volume: id.VolumeSerialNumber,
            file_id: u128::from_le_bytes(id.FileId.Identifier),
            size: u64::try_from(std_info.EndOfFile).unwrap_or(0),
            mtime: basic.LastWriteTime,
        })
    }
}

/// Whether the shell has a thumbnail provider (`IThumbnailProvider`) registered for `.ext`.
pub fn has_shell_thumbnail_provider(ext: &str) -> bool {
    // Shell association queries need a COM apartment and a live MTA (see `crate::com`).
    let _com = crate::com::ComScope::enter();
    let assoc: Vec<u16> = OsStr::new(&format!(".{ext}"))
        .encode_wide()
        .chain(Some(0))
        .collect();
    let extra: Vec<u16> = OsStr::new(THUMBNAIL_PROVIDER_IID)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut buf = [0u16; 128];
    let mut len = buf.len() as u32;
    // SAFETY: NUL-terminated inputs; `buf`/`len` describe a valid output buffer.
    let hr = unsafe {
        AssocQueryStringW(
            ASSOCF_NONE,
            ASSOCSTR_SHELLEXTENSION,
            assoc.as_ptr(),
            extra.as_ptr(),
            buf.as_mut_ptr(),
            &mut len,
        )
    };
    hr >= 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_survives_rename_and_changes_on_write() {
        let dir = std::env::temp_dir().join(format!("gander-identity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.bin");
        std::fs::write(&a, b"hello").unwrap();
        let before = file_identity(&File::open(&a).unwrap()).unwrap();
        let b = dir.join("b.bin");
        std::fs::rename(&a, &b).unwrap();
        let renamed = file_identity(&File::open(&b).unwrap()).unwrap();
        assert_eq!(before, renamed);
        std::fs::write(&b, b"hello, world").unwrap();
        let written = file_identity(&File::open(&b).unwrap()).unwrap();
        assert_ne!(before, written);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn shortcut_types_have_no_lookup_surprises() {
        // Not asserting presence (machine dependent), only that the call is safe for odd input.
        let _ = has_shell_thumbnail_provider("definitely-not-an-extension");
    }
}
