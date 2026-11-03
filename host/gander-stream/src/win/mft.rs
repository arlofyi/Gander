// SPDX-License-Identifier: Apache-2.0
// Moved from the stream spike (spikes/stream/capture, spikes/stream/encode) as production code.
//! Media Foundation start-up and hardware encoder MFT enumeration, per adapter (`MFTEnum2` with
//! `MFT_ENUM_ADAPTER_LUID`).

use anyhow::{Context, Result};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, MF_VERSION, MFCreateAttributes, MFMediaType_Video, MFSTARTUP_FULL,
    MFStartup, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG,
    MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_HARDWARE_URL_Attribute,
    MFT_ENUM_HARDWARE_VENDOR_ID_Attribute, MFT_FRIENDLY_NAME_Attribute, MFT_REGISTER_TYPE_INFO,
    MFT_TRANSFORM_CLSID_Attribute, MFTEnum2, MFVideoFormat_H264, MFVideoFormat_HEVC,
    MFVideoFormat_NV12,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::core::{GUID, PWSTR};

/// A video codec the hardware encoders produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    Hevc,
}

impl Codec {
    /// Media Foundation output subtype.
    pub fn subtype(self) -> GUID {
        match self {
            Codec::H264 => MFVideoFormat_H264,
            Codec::Hevc => MFVideoFormat_HEVC,
        }
    }

    /// Short name (`h264`, `hevc`), also the dump file extension.
    pub fn name(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
        }
    }

    /// Parses `h264` / `hevc` (also `h265`).
    pub fn parse(s: &str) -> Option<Codec> {
        match s.to_ascii_lowercase().as_str() {
            "h264" | "avc" => Some(Codec::H264),
            "hevc" | "h265" => Some(Codec::Hevc),
            _ => None,
        }
    }
}

/// Initialises COM (multithreaded apartment) and Media Foundation for this process. Idempotent
/// enough: repeated calls only bump reference counts (balance with [`shutdown`]).
pub fn startup() -> Result<()> {
    // SAFETY: plain initialisation; S_FALSE (already initialised) is success.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .context("CoInitializeEx")?;
    // SAFETY: plain initialisation with the SDK version constant.
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.context("MFStartup")?;
    Ok(())
}

/// Balances one [`startup`] (Media Foundation and COM reference counts).
pub fn shutdown() {
    // SAFETY: called once per successful `startup` on the same thread.
    unsafe {
        let _ = windows::Win32::Media::MediaFoundation::MFShutdown();
        windows::Win32::System::Com::CoUninitialize();
    }
}

/// One hardware encoder MFT as Media Foundation lists it.
#[derive(Debug, Clone)]
pub struct EncoderInfo {
    /// `MFT_FRIENDLY_NAME_Attribute`.
    pub name: String,
    /// `MFT_ENUM_HARDWARE_VENDOR_ID_Attribute` (e.g. `VEN_8086`).
    pub vendor: Option<String>,
    /// `MFT_ENUM_HARDWARE_URL_Attribute` (identifies the device the MFT is bound to).
    pub url: Option<String>,
    /// `MFT_ENUM_ADAPTER_LUID` on the activation object, when present.
    pub luid: Option<i64>,
    /// `MFT_TRANSFORM_CLSID_Attribute`.
    pub clsid: Option<GUID>,
    /// The activation object (create the MFT with `ActivateObject`).
    pub activate: IMFActivate,
}

fn string_attr(a: &IMFAttributes, key: &GUID) -> Option<String> {
    let mut p = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: valid out pointers; the string is CoTaskMemAlloc'd and freed below.
    unsafe { a.GetAllocatedString(key, &mut p, &mut len) }.ok()?;
    // SAFETY: `p` is a NUL-terminated string returned by the call above.
    let s = unsafe { p.to_string() }.ok();
    // SAFETY: allocated by Media Foundation with CoTaskMemAlloc.
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}

fn luid_attr(a: &IMFAttributes) -> Option<i64> {
    // SAFETY: plain getters; errors mean "absent or other type".
    if let Ok(v) = unsafe { a.GetUINT64(&MFT_ENUM_ADAPTER_LUID) } {
        return Some(v as i64);
    }
    let mut buf = [0u8; 8];
    let mut n = 0u32;
    // SAFETY: buffer of 8 bytes, size out pointer valid.
    unsafe { a.GetBlob(&MFT_ENUM_ADAPTER_LUID, &mut buf, Some(&mut n)) }.ok()?;
    // LUID { LowPart: u32, HighPart: i32 } in memory order.
    let low = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as i64;
    let high = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as i64;
    Some((high << 32) | low)
}

/// Converts a LUID packed as `(HighPart << 32) | LowPart` (the [`super::d3d`] form) into
/// the in-memory `LUID` bytes.
fn luid_bytes(luid: i64) -> [u8; 8] {
    let low = (luid & 0xFFFF_FFFF) as u32;
    let high = (luid >> 32) as i32;
    let mut b = [0u8; 8];
    b[..4].copy_from_slice(&low.to_le_bytes());
    b[4..].copy_from_slice(&high.to_le_bytes());
    b
}

/// Hardware encoder MFTs taking NV12 and producing `codec`, optionally restricted to the adapter
/// with LUID `luid` (packed `(HighPart << 32) | LowPart`).
pub fn hardware_encoders(codec: Codec, luid: Option<i64>) -> Result<Vec<EncoderInfo>> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: codec.subtype(),
    };
    let attrs = match luid {
        Some(l) => {
            let mut a = None;
            // SAFETY: valid out pointer.
            unsafe { MFCreateAttributes(&mut a, 1) }.context("MFCreateAttributes")?;
            let a = a.context("no attributes")?;
            // SAFETY: 8-byte LUID blob.
            unsafe { a.SetBlob(&MFT_ENUM_ADAPTER_LUID, &luid_bytes(l)) }.context("SetBlob")?;
            Some(a)
        }
        None => None,
    };
    let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: type infos live for the call; the returned array is CoTaskMemAlloc'd and each
    // element is taken (moved out) exactly once below before the array is freed.
    unsafe {
        MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            Some(&input),
            Some(&output),
            attrs.as_ref(),
            &mut list,
            &mut count,
        )
    }
    .context("MFTEnum2")?;
    let mut out = Vec::new();
    for i in 0..count as usize {
        // SAFETY: `list` has `count` elements; each is read (moved) once.
        let item = unsafe { std::ptr::read(list.add(i)) };
        let Some(activate) = item else { continue };
        let a: &IMFAttributes = &activate;
        out.push(EncoderInfo {
            name: string_attr(a, &MFT_FRIENDLY_NAME_Attribute).unwrap_or_default(),
            vendor: string_attr(a, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute),
            url: string_attr(a, &MFT_ENUM_HARDWARE_URL_Attribute),
            luid: luid_attr(a),
            // SAFETY: plain getter.
            clsid: unsafe { a.GetGUID(&MFT_TRANSFORM_CLSID_Attribute) }.ok(),
            activate: activate.clone(),
        });
    }
    if !list.is_null() {
        // SAFETY: the array itself was CoTaskMemAlloc'd by MFTEnum2; its elements were moved out.
        unsafe { CoTaskMemFree(Some(list as *const _)) };
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luid_round_trip() {
        let l = 0x0000_0001_0001_6248_i64;
        let b = luid_bytes(l);
        assert_eq!(b, [0x48, 0x62, 0x01, 0x00, 0x01, 0, 0, 0]);
    }

    #[test]
    fn codec_parse() {
        assert_eq!(Codec::parse("H265"), Some(Codec::Hevc));
        assert_eq!(Codec::parse("h264"), Some(Codec::H264));
        assert_eq!(Codec::parse("vp9"), None);
    }
}
