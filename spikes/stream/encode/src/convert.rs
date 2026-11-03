// SPDX-License-Identifier: Apache-2.0
//! BGRA8 capture texture -> NV12 encoder input, scaled to the encode size, with the D3D11 video
//! processor on the same device (fixed-function on Intel and NVIDIA, no shaders).
//!
//! Output textures come from a small ring: the encoder holds an input sample until it has read
//! it, so a texture is only rewritten `ring` conversions later. The caller checks that fewer
//! than `ring - 1` samples are in flight.

use std::time::Instant;

use anyhow::{Context, Result};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_VIDEO_ENCODER, D3D11_QUERY_DESC, D3D11_QUERY_EVENT,
    D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Device, ID3D11DeviceContext, ID3D11Query,
    ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoContext1, ID3D11VideoDevice,
    ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
    ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
    DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::core::Interface;

struct Processor {
    in_w: u32,
    in_h: u32,
    enumerator: ID3D11VideoProcessorEnumerator,
    vp: ID3D11VideoProcessor,
    outputs: Vec<ID3D11VideoProcessorOutputView>,
    inputs: Vec<(ID3D11Texture2D, ID3D11VideoProcessorInputView)>,
}

/// Converter from BGRA textures of any size to NV12 textures of one fixed, even size.
pub struct Converter {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    vdev: ID3D11VideoDevice,
    vctx: ID3D11VideoContext,
    out_w: u32,
    out_h: u32,
    fps: u32,
    ring: Vec<ID3D11Texture2D>,
    next: usize,
    proc_: Option<Processor>,
    query: ID3D11Query,
    /// True when the NV12 textures carry `D3D11_BIND_VIDEO_ENCODER` (refused on some drivers).
    pub encoder_bind: bool,
    /// True when `ID3D11VideoContext1` colour spaces were applied (BT.709 limited output).
    pub colour_space_1: bool,
}

impl std::fmt::Debug for Converter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Converter({}x{})", self.out_w, self.out_h)
    }
}

/// Result of one conversion.
#[derive(Debug, Clone)]
pub struct Converted {
    /// The NV12 texture (from the ring), on the converter's device.
    pub texture: ID3D11Texture2D,
    /// Ring index of the texture.
    pub index: usize,
    /// Wall time from submitting the blit to the GPU signalling it finished, in ms.
    pub ms: f64,
}

impl Converter {
    /// A converter on `device` producing `out_w` x `out_h` NV12 (both must be even).
    pub fn new(
        device: &ID3D11Device,
        out_w: u32,
        out_h: u32,
        fps: u32,
        ring: usize,
    ) -> Result<Self> {
        anyhow::ensure!(
            out_w.is_multiple_of(2) && out_h.is_multiple_of(2),
            "NV12 needs even sizes"
        );
        let vdev: ID3D11VideoDevice = device.cast().context("ID3D11VideoDevice")?;
        // SAFETY: plain getter.
        let context = unsafe { device.GetImmediateContext() }?;
        let vctx: ID3D11VideoContext = context.cast().context("ID3D11VideoContext")?;
        let mut encoder_bind = true;
        let mut textures = Vec::with_capacity(ring);
        for _ in 0..ring {
            let make = |bind: i32| -> Result<ID3D11Texture2D> {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: out_w,
                    Height: out_h,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_NV12,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: bind as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut t = None;
                // SAFETY: valid descriptor and out pointer.
                unsafe { device.CreateTexture2D(&desc, None, Some(&mut t)) }?;
                t.context("no texture")
            };
            let t = if encoder_bind {
                match make(D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_VIDEO_ENCODER.0) {
                    Ok(t) => t,
                    Err(_) => {
                        encoder_bind = false;
                        make(D3D11_BIND_RENDER_TARGET.0).context("NV12 texture")?
                    }
                }
            } else {
                make(D3D11_BIND_RENDER_TARGET.0).context("NV12 texture")?
            };
            textures.push(t);
        }
        let mut query = None;
        let qd = D3D11_QUERY_DESC {
            Query: D3D11_QUERY_EVENT,
            MiscFlags: 0,
        };
        // SAFETY: valid descriptor and out pointer.
        unsafe { device.CreateQuery(&qd, Some(&mut query)) }.context("CreateQuery")?;
        Ok(Converter {
            device: device.clone(),
            context,
            vdev,
            vctx,
            out_w,
            out_h,
            fps,
            ring: textures,
            next: 0,
            proc_: None,
            query: query.context("no query")?,
            encoder_bind,
            colour_space_1: false,
        })
    }

    /// The ring textures (all already written once the converter has run `ring` times).
    pub fn ring(&self) -> &[ID3D11Texture2D] {
        &self.ring
    }

    fn processor(&mut self, in_w: u32, in_h: u32) -> Result<&mut Processor> {
        if !matches!(&self.proc_, Some(p) if p.in_w == in_w && p.in_h == in_h) {
            let rate = DXGI_RATIONAL {
                Numerator: self.fps,
                Denominator: 1,
            };
            let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: rate,
                InputWidth: in_w,
                InputHeight: in_h,
                OutputFrameRate: rate,
                OutputWidth: self.out_w,
                OutputHeight: self.out_h,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            // SAFETY: valid descriptor.
            let enumerator = unsafe { self.vdev.CreateVideoProcessorEnumerator(&desc) }
                .context("CreateVideoProcessorEnumerator")?;
            // SAFETY: valid enumerator, rate conversion index 0.
            let vp = unsafe { self.vdev.CreateVideoProcessor(&enumerator, 0) }
                .context("CreateVideoProcessor")?;
            let mut outputs = Vec::with_capacity(self.ring.len());
            for t in &self.ring {
                let od = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                    },
                };
                let mut v = None;
                // SAFETY: texture and enumerator live on this device; valid out pointer.
                unsafe {
                    self.vdev
                        .CreateVideoProcessorOutputView(t, &enumerator, &od, Some(&mut v))
                }
                .context("CreateVideoProcessorOutputView")?;
                outputs.push(v.context("no output view")?);
            }
            // Colour: full-range sRGB in, BT.709 limited range YCbCr out (what decoders assume
            // for untagged H.264).
            self.colour_space_1 = if let Ok(v1) = self.vctx.cast::<ID3D11VideoContext1>() {
                // SAFETY: plain state setters on a live processor.
                unsafe {
                    v1.VideoProcessorSetStreamColorSpace1(
                        &vp,
                        0,
                        DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                    );
                    v1.VideoProcessorSetOutputColorSpace1(
                        &vp,
                        DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                    );
                }
                true
            } else {
                false
            };
            // SAFETY: plain state setters on a live processor.
            unsafe {
                self.vctx.VideoProcessorSetStreamFrameFormat(
                    &vp,
                    0,
                    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                );
                self.vctx
                    .VideoProcessorSetStreamAutoProcessingMode(&vp, 0, false);
                let dst = RECT {
                    left: 0,
                    top: 0,
                    right: self.out_w as i32,
                    bottom: self.out_h as i32,
                };
                self.vctx
                    .VideoProcessorSetStreamDestRect(&vp, 0, true, Some(&dst));
                self.vctx
                    .VideoProcessorSetOutputTargetRect(&vp, true, Some(&dst));
            }
            self.proc_ = Some(Processor {
                in_w,
                in_h,
                enumerator,
                vp,
                outputs,
                inputs: Vec::new(),
            });
        }
        self.proc_.as_mut().context("processor")
    }

    /// Converts the top-left `src_w` x `src_h` of `src` (BGRA8 on this device), scaled to the
    /// whole output, into the next ring texture, and waits for the GPU to finish it.
    pub fn convert(&mut self, src: &ID3D11Texture2D, src_w: u32, src_h: u32) -> Result<Converted> {
        let mut sd = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: valid out pointer.
        unsafe { src.GetDesc(&mut sd) };
        let vdev = self.vdev.clone();
        let index = self.next;
        self.next = (self.next + 1) % self.ring.len();
        let p = self.processor(sd.Width, sd.Height)?;
        let view = match p.inputs.iter().find(|(t, _)| t == src) {
            Some((_, v)) => v.clone(),
            None => {
                let id = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    FourCC: 0,
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPIV {
                            MipSlice: 0,
                            ArraySlice: 0,
                        },
                    },
                };
                let mut v = None;
                // SAFETY: texture and enumerator on this device; valid out pointer.
                unsafe {
                    vdev.CreateVideoProcessorInputView(src, &p.enumerator, &id, Some(&mut v))
                }
                .context("CreateVideoProcessorInputView")?;
                let v = v.context("no input view")?;
                // WGC pools hold 2-4 textures; keep a few views, drop the oldest.
                if p.inputs.len() >= 8 {
                    p.inputs.remove(0);
                }
                p.inputs.push((src.clone(), v.clone()));
                v
            }
        };
        let vp = p.vp.clone();
        let out_view = p.outputs[index].clone();
        let src_rect = RECT {
            left: 0,
            top: 0,
            right: src_w.min(sd.Width) as i32,
            bottom: src_h.min(sd.Height) as i32,
        };
        let start = Instant::now();
        // SAFETY: all views and the processor live on this device; the stream struct holds a
        // counted reference that is released by ManuallyDrop::into_inner below.
        unsafe {
            self.vctx
                .VideoProcessorSetStreamSourceRect(&vp, 0, true, Some(&src_rect));
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(view)),
                ..Default::default()
            };
            let streams = [stream];
            let r = self.vctx.VideoProcessorBlt(&vp, &out_view, 0, &streams);
            let [stream] = streams;
            drop(std::mem::ManuallyDrop::into_inner(stream.pInputSurface));
            r.context("VideoProcessorBlt")?;
            self.context.End(&self.query);
            self.context.Flush();
            // GetData returns S_FALSE (a success code) until the event is reached, so read the
            // BOOL it writes instead of the HRESULT.
            loop {
                let mut done = windows::core::BOOL(0);
                let _ = self.context.GetData(
                    &self.query,
                    Some(&mut done as *mut _ as *mut std::ffi::c_void),
                    std::mem::size_of::<windows::core::BOOL>() as u32,
                    0,
                );
                if done.as_bool() {
                    break;
                }
                std::hint::spin_loop();
            }
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        Ok(Converted {
            texture: self.ring[index].clone(),
            index,
            ms,
        })
    }

    /// The device this converter runs on.
    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }
}
