// SPDX-License-Identifier: Apache-2.0
//! Windows Imaging Component decoding: first frame, downscaled decode (the WIC scaler uses the
//! codec's own reduced-resolution decode where it has one, e.g. JPEG DCT scaling), EXIF
//! orientation, embedded ICC profile converted to sRGB, output as straight RGBA. Metadata is not
//! carried into the output.

use std::fs::File;
use std::os::windows::io::AsRawHandle;

use gander_core::tiles::{Pyramid, displayed_rect_to_stored};
use gander_win::render::job::TILE_OUT_OF_RANGE_MSG;
use image::metadata::Orientation;
use image::{DynamicImage, RgbaImage};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, GUID_WICPixelFormat32bppRGBA,
    IWICBitmapDecoder, IWICBitmapFrameDecode, IWICBitmapSource, IWICColorContext,
    IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapInterpolationModeHighQualityCubic,
    WICBitmapPaletteTypeMedianCut, WICColorContextProfile, WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::VT_UI2;
use windows::core::{GUID, HRESULT, Interface, w};

use crate::{Failure, MAX_SOURCE_PIXELS, Rendered, Target};

/// `WINCODEC_ERR_COMPONENTNOTFOUND`: no decoder for this container (codec pack missing).
const WINCODEC_ERR_COMPONENTNOTFOUND: HRESULT = HRESULT(0x8898_2F50_u32 as i32);
/// `WINCODEC_ERR_UNKNOWNIMAGEFORMAT`.
const WINCODEC_ERR_UNKNOWNIMAGEFORMAT: HRESULT = HRESULT(0x8898_2F07_u32 as i32);

/// Maps a WIC error: "no decoder" is unsupported (exit 2), anything else a decoder error (4).
pub fn classify(e: windows::core::Error) -> Failure {
    let code = e.code();
    if code == WINCODEC_ERR_COMPONENTNOTFOUND || code == WINCODEC_ERR_UNKNOWNIMAGEFORMAT {
        Failure::Unsupported(format!("no WIC decoder ({code:?})"))
    } else {
        Failure::Decode(format!("WIC: {code:?}"))
    }
}

/// The imaging factory (COM must be initialised on this thread).
pub fn factory() -> Rendered<IWICImagingFactory> {
    // SAFETY: COM is initialised by the caller; standard in-proc activation.
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
        .map_err(Failure::decode)
}

/// Decodes the file behind `file` directly through its handle into a `size` box.
pub fn decode_file(file: &File, size: u32) -> Rendered<RgbaImage> {
    decode_file_target(file, Target::Box(size))
}

/// Decodes the file behind `file` directly through its handle.
pub fn decode_file_target(file: &File, target: Target) -> Rendered<RgbaImage> {
    let factory = factory()?;
    // SAFETY: the handle is valid for the lifetime of `file`, which outlives the decoder use
    // below (the decoder is dropped before this function returns).
    let decoder = unsafe {
        factory.CreateDecoderFromFileHandle(
            file.as_raw_handle() as usize,
            std::ptr::null(),
            WICDecodeMetadataCacheOnDemand,
        )
    }
    .map_err(classify)?;
    decode_first_frame(&factory, &decoder, target, None)
}

/// Dimensions (after EXIF orientation) of the first frame of the file, without decoding pixels.
pub fn frame_size_file(file: &File) -> Rendered<(u32, u32)> {
    let factory = factory()?;
    // SAFETY: as in `decode_file_target`; the decoder is dropped before returning.
    let decoder = unsafe {
        factory.CreateDecoderFromFileHandle(
            file.as_raw_handle() as usize,
            std::ptr::null(),
            WICDecodeMetadataCacheOnDemand,
        )
    }
    .map_err(classify)?;
    frame_size(&decoder, None)
}

/// Dimensions (after orientation) of the first frame of an in-memory image.
pub fn frame_size_bytes(bytes: &[u8], orientation: Option<u16>) -> Rendered<(u32, u32)> {
    let factory = factory()?;
    // SAFETY: `bytes` outlives the stream and the decoder, both dropped before returning.
    let decoder = unsafe {
        let stream = factory.CreateStream().map_err(Failure::decode)?;
        stream
            .InitializeFromMemory(bytes)
            .map_err(Failure::decode)?;
        factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
    }
    .map_err(classify)?;
    frame_size(&decoder, orientation)
}

fn frame_size(decoder: &IWICBitmapDecoder, orientation: Option<u16>) -> Rendered<(u32, u32)> {
    // SAFETY: plain COM calls on a live interface with valid out-pointers.
    unsafe {
        let frame = decoder.GetFrame(0).map_err(classify)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).map_err(classify)?;
        if w == 0 || h == 0 {
            return Err(Failure::decode("image has no pixels"));
        }
        let orientation = orientation.unwrap_or_else(|| read_orientation(&frame));
        Ok(if swaps_axes(orientation) {
            (h, w)
        } else {
            (w, h)
        })
    }
}

/// EXIF orientations 5 to 8 rotate by a quarter turn, so width and height trade places.
fn swaps_axes(orientation: u16) -> bool {
    (5..=8).contains(&orientation)
}

/// Decodes an in-memory image (embedded RAW preview, Office thumbnail, rendered PDF page) into
/// a `size` box. `orientation` overrides the image's own EXIF orientation when the container
/// carries it (RAW files keep orientation in their own IFD0).
pub fn decode_bytes(bytes: &[u8], size: u32, orientation: Option<u16>) -> Rendered<RgbaImage> {
    decode_bytes_target(bytes, Target::Box(size), orientation)
}

/// [`decode_bytes`] with an explicit output sizing rule.
pub fn decode_bytes_target(
    bytes: &[u8],
    target: Target,
    orientation: Option<u16>,
) -> Rendered<RgbaImage> {
    let factory = factory()?;
    // SAFETY: `bytes` outlives the stream and the decoder, both dropped before returning.
    let decoder = unsafe {
        let stream = factory.CreateStream().map_err(Failure::decode)?;
        stream
            .InitializeFromMemory(bytes)
            .map_err(Failure::decode)?;
        factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
    }
    .map_err(classify)?;
    decode_first_frame(&factory, &decoder, target, orientation)
}

fn decode_first_frame(
    factory: &IWICImagingFactory,
    decoder: &IWICBitmapDecoder,
    target: Target,
    orientation: Option<u16>,
) -> Rendered<RgbaImage> {
    // SAFETY: plain COM calls on live interfaces with valid out-pointers.
    unsafe {
        let frame = decoder.GetFrame(0).map_err(classify)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).map_err(classify)?;
        if w == 0 || h == 0 {
            return Err(Failure::decode("image has no pixels"));
        }
        if u64::from(w) * u64::from(h) > MAX_SOURCE_PIXELS {
            return Err(Failure::Decode(format!(
                "declared {w}x{h} exceeds the {MAX_SOURCE_PIXELS} pixel limit"
            )));
        }
        let orientation = orientation.unwrap_or_else(|| read_orientation(&frame));
        // The target describes the displayed image; a quarter-turn orientation swaps the axes of
        // the stored pixels, so size them for the swapped box and rotate afterwards.
        let (tw, th) = if swaps_axes(orientation) {
            let (dw, dh) = target.fit(h, w);
            (dh, dw)
        } else {
            target.fit(w, h)
        };

        let mut source: IWICBitmapSource = frame.cast().map_err(Failure::decode)?;
        if (tw, th) != (w, h) {
            let scaler = factory.CreateBitmapScaler().map_err(Failure::decode)?;
            scaler
                .Initialize(&source, tw, th, WICBitmapInterpolationModeHighQualityCubic)
                .map_err(classify)?;
            source = scaler.cast().map_err(Failure::decode)?;
        }
        if let Some(converted) = to_srgb(factory, &frame, &source) {
            source = converted;
        }
        let converter = factory.CreateFormatConverter().map_err(Failure::decode)?;
        converter
            .Initialize(
                &source,
                &GUID_WICPixelFormat32bppRGBA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeMedianCut,
            )
            .map_err(classify)?;
        let stride = tw
            .checked_mul(4)
            .ok_or_else(|| Failure::decode("too wide"))?;
        let mut buf = vec![0u8; stride as usize * th as usize];
        converter
            .CopyPixels(std::ptr::null(), stride, &mut buf)
            .map_err(classify)?;
        let img = RgbaImage::from_raw(tw, th, buf).ok_or_else(|| Failure::decode("buffer size"))?;
        Ok(apply_orientation(img, orientation))
    }
}

/// Deep-zoom tile `(level, x, y)` of the first frame of the file behind `file`, decoded straight
/// from its handle: only the tile's area is read and scaled (see [`decode_tile_frame`]).
pub fn tile_file(file: &File, level: u32, x: u32, y: u32) -> Rendered<RgbaImage> {
    let factory = factory()?;
    // SAFETY: as in `decode_file_target`; the decoder is dropped before returning.
    let decoder = unsafe {
        factory.CreateDecoderFromFileHandle(
            file.as_raw_handle() as usize,
            std::ptr::null(),
            WICDecodeMetadataCacheOnDemand,
        )
    }
    .map_err(classify)?;
    decode_tile_frame(&factory, &decoder, None, level, x, y)
}

/// [`tile_file`] for an in-memory image (the JPEG embedded in a RAW file); `orientation`
/// overrides the image's own EXIF orientation as in [`decode_bytes`].
pub fn tile_bytes(
    bytes: &[u8],
    orientation: Option<u16>,
    level: u32,
    x: u32,
    y: u32,
) -> Rendered<RgbaImage> {
    let factory = factory()?;
    // SAFETY: `bytes` outlives the stream and the decoder, both dropped before returning.
    let decoder = unsafe {
        let stream = factory.CreateStream().map_err(Failure::decode)?;
        stream
            .InitializeFromMemory(bytes)
            .map_err(Failure::decode)?;
        factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
    }
    .map_err(classify)?;
    decode_tile_frame(&factory, &decoder, orientation, level, x, y)
}

/// Decodes one tile of the first frame. The pyramid is that of the displayed image (after EXIF
/// orientation); the tile's area is mapped back to stored pixels, cut out with a WIC clipper and
/// scaled to the tile size, so a 6000 x 4500 photo is never decoded in full for one tile (the
/// codec reads only the rows and blocks the clipped area needs; a whole-image tile at the lowest
/// level goes through the scaler alone, which lets JPEG use its reduced-size decode).
fn decode_tile_frame(
    factory: &IWICImagingFactory,
    decoder: &IWICBitmapDecoder,
    orientation: Option<u16>,
    level: u32,
    x: u32,
    y: u32,
) -> Rendered<RgbaImage> {
    // SAFETY: plain COM calls on live interfaces with valid out-pointers.
    unsafe {
        let frame = decoder.GetFrame(0).map_err(classify)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).map_err(classify)?;
        if w == 0 || h == 0 {
            return Err(Failure::decode("image has no pixels"));
        }
        if u64::from(w) * u64::from(h) > MAX_SOURCE_PIXELS {
            return Err(Failure::Decode(format!(
                "declared {w}x{h} exceeds the {MAX_SOURCE_PIXELS} pixel limit"
            )));
        }
        let orientation = orientation.unwrap_or_else(|| read_orientation(&frame));
        let swapped = swaps_axes(orientation);
        let (dw, dh) = if swapped { (h, w) } else { (w, h) };
        let pyramid =
            Pyramid::new(dw, dh).ok_or_else(|| Failure::unsupported("image too large to tile"))?;
        let tile = pyramid
            .tile(level, x, y)
            .ok_or_else(|| Failure::decode(TILE_OUT_OF_RANGE_MSG))?;
        let (sx, sy, sw, sh) = displayed_rect_to_stored(
            orientation,
            (w, h),
            (tile.src_x, tile.src_y, tile.src_width, tile.src_height),
        );
        let (tw, th) = if swapped {
            (tile.height, tile.width)
        } else {
            (tile.width, tile.height)
        };

        let mut source: IWICBitmapSource = frame.cast().map_err(Failure::decode)?;
        if (sx, sy, sw, sh) != (0, 0, w, h) {
            let clipper = factory.CreateBitmapClipper().map_err(Failure::decode)?;
            let rect = WICRect {
                X: sx as i32,
                Y: sy as i32,
                Width: sw as i32,
                Height: sh as i32,
            };
            clipper.Initialize(&source, &rect).map_err(classify)?;
            source = clipper.cast().map_err(Failure::decode)?;
        }
        if (tw, th) != (sw, sh) {
            let scaler = factory.CreateBitmapScaler().map_err(Failure::decode)?;
            scaler
                .Initialize(&source, tw, th, WICBitmapInterpolationModeHighQualityCubic)
                .map_err(classify)?;
            source = scaler.cast().map_err(Failure::decode)?;
        }
        if let Some(converted) = to_srgb(factory, &frame, &source) {
            source = converted;
        }
        let converter = factory.CreateFormatConverter().map_err(Failure::decode)?;
        converter
            .Initialize(
                &source,
                &GUID_WICPixelFormat32bppRGBA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeMedianCut,
            )
            .map_err(classify)?;
        let stride = tw
            .checked_mul(4)
            .ok_or_else(|| Failure::decode("too wide"))?;
        let mut buf = vec![0u8; stride as usize * th as usize];
        converter
            .CopyPixels(std::ptr::null(), stride, &mut buf)
            .map_err(classify)?;
        let img = RgbaImage::from_raw(tw, th, buf).ok_or_else(|| Failure::decode("buffer size"))?;
        Ok(apply_orientation(img, orientation))
    }
}

/// Applies an EXIF orientation value (1..=8; anything else is treated as 1).
pub fn apply_orientation(img: RgbaImage, exif: u16) -> RgbaImage {
    let Some(o) = u8::try_from(exif).ok().and_then(Orientation::from_exif) else {
        return img;
    };
    if o == Orientation::NoTransforms {
        return img;
    }
    let mut dynamic = DynamicImage::ImageRgba8(img);
    dynamic.apply_orientation(o);
    dynamic.into_rgba8()
}

/// EXIF orientation of the frame via the format-independent photo metadata policy, else 1.
fn read_orientation(frame: &IWICBitmapFrameDecode) -> u16 {
    // SAFETY: COM calls with a zero-initialised PROPVARIANT, cleared afterwards; the union is
    // read only after checking the variant type.
    unsafe {
        let Ok(reader) = frame.GetMetadataQueryReader() else {
            return 1;
        };
        for name in [
            w!("System.Photo.Orientation"),
            w!("/app1/ifd/{ushort=274}"),
            w!("/ifd/{ushort=274}"),
        ] {
            let mut pv = PROPVARIANT::default();
            if reader.GetMetadataByName(name, &mut pv).is_ok() {
                let inner = &pv.Anonymous.Anonymous;
                let value = if inner.vt == VT_UI2 {
                    Some(inner.Anonymous.uiVal)
                } else {
                    None
                };
                let _ = PropVariantClear(&mut pv);
                if let Some(v) = value {
                    return v;
                }
            }
        }
        1
    }
}

/// Converts `source` to sRGB when the frame carries an ICC profile. Any failure keeps the
/// original pixels (a slightly off colour beats no thumbnail).
fn to_srgb(
    factory: &IWICImagingFactory,
    frame: &IWICBitmapFrameDecode,
    source: &IWICBitmapSource,
) -> Option<IWICBitmapSource> {
    // SAFETY: COM calls on live interfaces; the context array is sized by the first query.
    unsafe {
        let mut count = 0u32;
        frame.GetColorContexts(&mut [], &mut count).ok()?;
        if count == 0 {
            return None;
        }
        let mut contexts: Vec<Option<IWICColorContext>> = Vec::new();
        for _ in 0..count {
            contexts.push(Some(factory.CreateColorContext().ok()?));
        }
        let mut actual = 0u32;
        frame.GetColorContexts(&mut contexts, &mut actual).ok()?;
        let profile = contexts
            .into_iter()
            .take(actual as usize)
            .flatten()
            .find(|c| c.GetType().is_ok_and(|t| t == WICColorContextProfile))?;
        let srgb = factory.CreateColorContext().ok()?;
        srgb.InitializeFromExifColorSpace(1).ok()?;
        let transform = factory.CreateColorTransformer().ok()?;
        let dest: GUID = GUID_WICPixelFormat32bppBGRA;
        transform.Initialize(source, &profile, &srgb, &dest).ok()?;
        transform.cast().ok()
    }
}
