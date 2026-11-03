// SPDX-License-Identifier: Apache-2.0
//! Matrix item C12: getting a capture texture from one adapter's device to another's.
//!
//! [`try_shared_handle`] tries the zero-copy route (an NT-handle shared texture opened on the
//! other device); [`CrossCopy`] is the CPU route that always works: copy to a staging texture
//! on the source device, map it, upload into a texture on the destination device.

use std::time::Instant;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_CPU_ACCESS_READ,
    D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX,
    D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING, ID3D11Device, ID3D11Device1, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIResource1;
use windows::core::Interface;

fn bgra_desc(w: u32, h: u32) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    }
}

/// Tries to share a BGRA texture created on `src` with `dst` through an NT handle (keyed mutex).
/// Returns a one-line description of what happened at each step.
pub fn try_shared_handle(src: &ID3D11Device, dst: &ID3D11Device) -> String {
    let mut desc = bgra_desc(256, 256);
    desc.MiscFlags =
        (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0) as u32;
    let mut tex = None;
    // SAFETY: valid descriptor and out pointer.
    if let Err(e) = unsafe { src.CreateTexture2D(&desc, None, Some(&mut tex)) } {
        return format!(
            "CreateTexture2D(shared NT handle) on source failed 0x{:08X}",
            e.code().0
        );
    }
    let Some(tex) = tex else {
        return "no texture".into();
    };
    let Ok(res) = tex.cast::<IDXGIResource1>() else {
        return "no IDXGIResource1".into();
    };
    // SAFETY: plain handle creation; closed below.
    let handle = match unsafe { res.CreateSharedHandle(None, GENERIC_ALL.0, None) } {
        Ok(h) => h,
        Err(e) => return format!("CreateSharedHandle failed 0x{:08X}", e.code().0),
    };
    let Ok(dst1) = dst.cast::<ID3D11Device1>() else {
        // SAFETY: handle from CreateSharedHandle.
        let _ = unsafe { CloseHandle(handle) };
        return "no ID3D11Device1 on destination".into();
    };
    // SAFETY: valid NT handle for the call.
    let r = unsafe { dst1.OpenSharedResource1::<ID3D11Texture2D>(handle) };
    // SAFETY: handle from CreateSharedHandle.
    let _ = unsafe { CloseHandle(handle) };
    match r {
        Ok(_) => "OpenSharedResource1 on the other adapter SUCCEEDED".into(),
        Err(e) => format!(
            "shared handle created; OpenSharedResource1 on the other adapter failed 0x{:08X} ({})",
            e.code().0,
            e.message()
        ),
    }
}

/// CPU copy of BGRA textures from one device to another.
pub struct CrossCopy {
    src_ctx: ID3D11DeviceContext,
    src: ID3D11Device,
    dst: ID3D11Device,
    dst_ctx: ID3D11DeviceContext,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    target: Option<(ID3D11Texture2D, u32, u32)>,
}

impl std::fmt::Debug for CrossCopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CrossCopy")
    }
}

impl CrossCopy {
    /// A copier from `src` (capture device) to `dst` (encoder device).
    pub fn new(src: &ID3D11Device, dst: &ID3D11Device) -> Result<CrossCopy> {
        // SAFETY: plain getters.
        let (src_ctx, dst_ctx) =
            unsafe { (src.GetImmediateContext()?, dst.GetImmediateContext()?) };
        Ok(CrossCopy {
            src_ctx,
            src: src.clone(),
            dst: dst.clone(),
            dst_ctx,
            staging: None,
            target: None,
        })
    }

    /// Copies the top-left `w` x `h` of `texture` (on the source device) into a texture on the
    /// destination device. Returns the destination texture and the wall time in ms (includes
    /// waiting for the source GPU and the upload).
    pub fn copy(
        &mut self,
        texture: &ID3D11Texture2D,
        w: u32,
        h: u32,
    ) -> Result<(ID3D11Texture2D, f64)> {
        let start = Instant::now();
        if !matches!(&self.staging, Some((_, sw, sh)) if *sw == w && *sh == h) {
            let mut d = bgra_desc(w, h);
            d.Usage = D3D11_USAGE_STAGING;
            d.BindFlags = 0;
            d.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            let mut t = None;
            // SAFETY: valid descriptor and out pointer.
            unsafe { self.src.CreateTexture2D(&d, None, Some(&mut t)) }.context("staging")?;
            self.staging = Some((t.context("no staging")?, w, h));
            let mut t = None;
            // SAFETY: valid descriptor and out pointer.
            unsafe {
                self.dst
                    .CreateTexture2D(&bgra_desc(w, h), None, Some(&mut t))
            }
            .context("target")?;
            self.target = Some((t.context("no target")?, w, h));
        }
        let (Some((staging, _, _)), Some((target, _, _))) = (&self.staging, &self.target) else {
            unreachable!("created above");
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
        // SAFETY: both textures live on their devices and the box lies within both; the mapped
        // pointer is read by UpdateSubresource between Map and Unmap with its own row pitch.
        unsafe {
            self.src_ctx
                .CopySubresourceRegion(staging, 0, 0, 0, 0, texture, 0, Some(&region));
            self.src_ctx
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .context("Map")?;
            self.dst_ctx
                .UpdateSubresource(target, 0, None, mapped.pData, mapped.RowPitch, 0);
            self.src_ctx.Unmap(staging, 0);
        }
        Ok((target.clone(), start.elapsed().as_secs_f64() * 1000.0))
    }
}
