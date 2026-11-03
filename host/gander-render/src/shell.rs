// SPDX-License-Identifier: Apache-2.0
//! Shell thumbnails through `IShellItemImageFactory` (video posters, audio cover art, and any
//! type with a registered thumbnail provider).
//!
//! `SIIGBF_THUMBNAILONLY` makes the shell fail instead of returning the type's icon, so an
//! icon-only result is rejected by construction ("no thumbnail is better than a generic icon").
//! The shell needs a parsing name; it is derived inside the worker from the inherited handle
//! (`GetFinalPathNameByHandleW`), so it names the file the resolver opened, not a phone path.
//! Third-party providers crash and hang routinely; the job object and watchdog contain them.

use std::fs::File;
use std::os::windows::io::AsRawHandle;

use image::RgbaImage;
use windows::Win32::Foundation::{HANDLE, SIZE};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDIBits, GetObjectW, HGDIOBJ,
};
use windows::Win32::Storage::FileSystem::{FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW};
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_THUMBNAILONLY,
};
use windows::core::PCWSTR;

use crate::{Failure, Rendered};

/// Thumbnail of `file` from the shell at (about) `size` pixels.
pub fn thumb(file: &File, size: u32) -> Rendered<RgbaImage> {
    let path = path_of(file)?;
    // SAFETY: COM (STA) is initialised by the caller; `path` is NUL-terminated and outlives the
    // call; the HBITMAP is owned here and deleted below.
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None::<&IBindCtx>)
                .map_err(|e| Failure::Unsupported(format!("shell item: {:?}", e.code())))?;
        let side = i32::try_from(size).unwrap_or(512);
        let bitmap = factory
            .GetImage(
                SIZE { cx: side, cy: side },
                SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK,
            )
            .map_err(|e| Failure::Unsupported(format!("no shell thumbnail: {:?}", e.code())))?;
        let result = bitmap_to_rgba(HGDIOBJ(bitmap.0));
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        result
    }
}

/// `\\?\C:\...` (or `\\?\UNC\server\...`) of the open file, NUL-terminated, in the plain form
/// the shell parses.
fn path_of(file: &File) -> Rendered<Vec<u16>> {
    let handle = HANDLE(file.as_raw_handle());
    let mut buf = vec![0u16; 1024];
    loop {
        // SAFETY: valid handle; `buf` is a writable buffer of the stated length.
        let n =
            unsafe { GetFinalPathNameByHandleW(handle, &mut buf, FILE_NAME_NORMALIZED) } as usize;
        if n == 0 {
            return Err(Failure::decode("cannot name the file for the shell"));
        }
        if n < buf.len() {
            buf.truncate(n);
            break;
        }
        if n > 32_768 {
            return Err(Failure::decode("path too long"));
        }
        buf.resize(n + 1, 0);
    }
    let s = String::from_utf16_lossy(&buf);
    let plain = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        s
    };
    Ok(plain.encode_utf16().chain(Some(0)).collect())
}

/// Copies a shell HBITMAP out as straight RGBA. Shell thumbnails are 32-bit, possibly
/// premultiplied; a bitmap whose alpha is all zero is opaque.
unsafe fn bitmap_to_rgba(obj: HGDIOBJ) -> Rendered<RgbaImage> {
    // SAFETY: caller passes a live HBITMAP; buffers are sized from its BITMAP description.
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(
            obj,
            std::mem::size_of::<BITMAP>() as i32,
            Some((&raw mut bm).cast()),
        ) == 0
        {
            return Err(Failure::decode("unreadable shell bitmap"));
        }
        let (w, h) = (bm.bmWidth.unsigned_abs(), bm.bmHeight.unsigned_abs());
        if w == 0 || h == 0 || w > 4096 || h > 4096 {
            return Err(Failure::decode("shell bitmap has an odd size"));
        }
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; w as usize * h as usize * 4];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(
            dc,
            windows::Win32::Graphics::Gdi::HBITMAP(obj.0),
            0,
            h,
            Some(px.as_mut_ptr().cast()),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        if lines == 0 {
            return Err(Failure::decode("GetDIBits failed"));
        }
        let all_zero_alpha = px.chunks_exact(4).all(|p| p[3] == 0);
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2); // BGRA -> RGBA
            if all_zero_alpha {
                p[3] = 255;
            } else if p[3] > 0 && p[3] < 255 {
                let a = u16::from(p[3]);
                for c in &mut p[..3] {
                    *c = ((u16::from(*c) * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        RgbaImage::from_raw(w, h, px).ok_or_else(|| Failure::decode("buffer size"))
    }
}
