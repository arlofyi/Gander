// SPDX-License-Identifier: Apache-2.0
//! CPU readback of a captured texture through a reusable staging texture (for verification
//! only; the encode path keeps frames on the GPU).

use anyhow::{Context, Result};
use gander_testwin::layout::Rgb;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};

/// A captured image as `0x00RRGGBB` pixels.
#[derive(Debug, Clone)]
pub struct Image {
    /// Width in pixels.
    pub w: i32,
    /// Height in pixels.
    pub h: i32,
    /// Row-major pixels, `0x00RRGGBB`.
    pub px: Vec<u32>,
}

impl Image {
    /// Pixel at (x, y) or `None` outside the image.
    pub fn at(&self, x: i32, y: i32) -> Option<Rgb> {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return None;
        }
        let p = self.px[(y * self.w + x) as usize];
        Some(Rgb((p >> 16) as u8, (p >> 8) as u8, p as u8))
    }

    /// Writes a binary PPM (for eyeballing; never commit captures of the real desktop).
    pub fn write_ppm(&self, path: &std::path::Path) -> std::io::Result<()> {
        let mut data = format!("P6\n{} {}\n255\n", self.w, self.h).into_bytes();
        for p in &self.px {
            data.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
        }
        std::fs::write(path, data)
    }
}

/// Copies textures to the CPU, reusing one staging texture per size.
#[derive(Debug)]
pub struct Readback {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
}

impl Readback {
    /// Readback on `device` (must be the device the textures live on).
    pub fn new(device: &ID3D11Device) -> Result<Readback> {
        // SAFETY: plain getter on a live device.
        let context = unsafe { device.GetImmediateContext() }.context("GetImmediateContext")?;
        Ok(Readback {
            device: device.clone(),
            context,
            staging: None,
        })
    }

    /// Reads the top-left `w` x `h` of a BGRA8 texture.
    pub fn read(&mut self, texture: &ID3D11Texture2D, w: u32, h: u32) -> Result<Image> {
        let mut src = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: valid out pointer.
        unsafe { texture.GetDesc(&mut src) };
        let (w, h) = (w.min(src.Width), h.min(src.Height));
        anyhow::ensure!(w > 0 && h > 0, "empty frame");
        if !matches!(&self.staging, Some((_, sw, sh)) if *sw == w && *sh == h) {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: src.Format,
                SampleDesc: src.SampleDesc,
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut tex = None;
            // SAFETY: valid descriptor and out pointer.
            unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut tex)) }
                .context("CreateTexture2D staging")?;
            self.staging = Some((tex.context("no staging texture")?, w, h));
        }
        let Some((staging, _, _)) = &self.staging else {
            unreachable!("staging texture was just created");
        };
        let region = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: w,
            bottom: h,
            back: 1,
        };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: both textures are live on this device; the box lies within both; the mapped
        // memory is read only between Map and Unmap, row by row within RowPitch.
        let px = unsafe {
            self.context
                .CopySubresourceRegion(staging, 0, 0, 0, 0, texture, 0, Some(&region));
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .context("Map")?;
            let mut px = Vec::with_capacity((w * h) as usize);
            for y in 0..h as usize {
                let row = std::slice::from_raw_parts(
                    mapped.pData.cast::<u8>().add(y * mapped.RowPitch as usize),
                    w as usize * 4,
                );
                px.extend(row.chunks_exact(4).map(|b| {
                    // BGRA in memory.
                    (u32::from(b[2]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[0])
                }));
            }
            self.context.Unmap(staging, 0);
            px
        };
        Ok(Image {
            w: w as i32,
            h: h as i32,
            px,
        })
    }
}
