// SPDX-License-Identifier: Apache-2.0
// Moved from the stream spike (spikes/stream/capture, spikes/stream/encode) as production code.
//! DXGI adapter enumeration and D3D11 device creation suitable for sharing with a Media
//! Foundation encoder (BGRA + video support, multithread protected).

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HMODULE, LUID, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_FLAG, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
    D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device, ID3D11Multithread,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice,
    IDXGIFactory1,
};
use windows::core::Interface;

/// One display output (monitor) attached to an adapter.
#[derive(Debug, Clone)]
pub struct OutputInfo {
    /// GDI device name, e.g. `\\.\DISPLAY1`.
    pub device_name: String,
    /// Desktop rectangle in physical pixels.
    pub desktop: RECT,
    /// `HMONITOR` value, for matching with `MonitorFromWindow`.
    pub monitor: isize,
}

/// Facts about one hardware adapter.
#[derive(Debug, Clone)]
pub struct AdapterInfo {
    /// Index in `IDXGIFactory1::EnumAdapters1` order.
    pub index: u32,
    /// Driver-reported name.
    pub name: String,
    /// PCI vendor id (0x8086 Intel, 0x10DE NVIDIA, 0x1002 AMD).
    pub vendor_id: u32,
    /// PCI device id.
    pub device_id: u32,
    /// Adapter LUID, stable for the boot; compare with [`device_luid`].
    pub luid: i64,
    /// Dedicated video memory in MiB.
    pub dedicated_mib: u64,
    /// User-mode driver version `a.b.c.d` from `CheckInterfaceSupport`.
    pub driver_version: Option<String>,
    /// Outputs (monitors) this adapter scans out to.
    pub outputs: Vec<OutputInfo>,
}

/// An adapter handle plus its facts.
#[derive(Debug, Clone)]
pub struct Adapter {
    /// The DXGI adapter, to pass to [`create_device`].
    pub adapter: IDXGIAdapter1,
    /// Its facts.
    pub info: AdapterInfo,
}

fn luid_i64(l: LUID) -> i64 {
    ((l.HighPart as i64) << 32) | l.LowPart as i64
}

fn wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

/// Hardware adapters in DXGI order (adapter 0 owns the primary display). Software adapters
/// (Microsoft Basic Render Driver) are skipped.
pub fn adapters() -> Result<Vec<Adapter>> {
    // SAFETY: plain factory creation.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.context("CreateDXGIFactory1")?;
    let mut out = Vec::new();
    for index in 0.. {
        // SAFETY: enumeration ends with DXGI_ERROR_NOT_FOUND, which ends the loop.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        // SAFETY: valid adapter.
        let desc = unsafe { adapter.GetDesc1() }.context("GetDesc1")?;
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        // SAFETY: the IID is a valid interface id; an error only means "unknown".
        let driver_version = unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID) }
            .ok()
            .map(|v| {
                let v = v as u64;
                format!(
                    "{}.{}.{}.{}",
                    v >> 48,
                    (v >> 32) & 0xFFFF,
                    (v >> 16) & 0xFFFF,
                    v & 0xFFFF
                )
            });
        let mut outputs = Vec::new();
        for o in 0.. {
            // SAFETY: enumeration ends with an error, which ends the loop.
            let Ok(output) = (unsafe { adapter.EnumOutputs(o) }) else {
                break;
            };
            // SAFETY: valid output.
            if let Ok(d) = unsafe { output.GetDesc() } {
                outputs.push(OutputInfo {
                    device_name: wide(&d.DeviceName),
                    desktop: d.DesktopCoordinates,
                    monitor: d.Monitor.0 as isize,
                });
            }
        }
        out.push(Adapter {
            adapter,
            info: AdapterInfo {
                index,
                name: wide(&desc.Description),
                vendor_id: desc.VendorId,
                device_id: desc.DeviceId,
                luid: luid_i64(desc.AdapterLuid),
                dedicated_mib: desc.DedicatedVideoMemory as u64 / (1024 * 1024),
                driver_version,
                outputs,
            },
        });
    }
    Ok(out)
}

/// Creates a D3D11 device for capture and encode on `adapter` (`None`: the default adapter).
///
/// Flags: `BGRA_SUPPORT` (WGC surfaces are BGRA8) and `VIDEO_SUPPORT` (Media Foundation / DXVA),
/// falling back to BGRA only if the driver refuses video support. The device is set
/// multithread-protected, as `IMFDXGIDeviceManager` requires and as WGC's free-threaded frame
/// pool needs (frames arrive on a thread-pool thread).
pub fn create_device(adapter: Option<&IDXGIAdapter1>) -> Result<ID3D11Device> {
    let base: IDXGIAdapter;
    let (adapter, driver) = match adapter {
        Some(a) => {
            base = a.cast()?;
            (Some(&base), D3D_DRIVER_TYPE_UNKNOWN)
        }
        None => (None, D3D_DRIVER_TYPE_HARDWARE),
    };
    let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
    let attempt = |flags: D3D11_CREATE_DEVICE_FLAG| -> Result<ID3D11Device> {
        let mut device = None;
        let mut level = D3D_FEATURE_LEVEL::default();
        // SAFETY: all out pointers are valid locals; `adapter` outlives the call.
        unsafe {
            D3D11CreateDevice(
                adapter,
                driver,
                HMODULE::default(),
                flags,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut level),
                None,
            )
        }?;
        device.context("D3D11CreateDevice returned no device")
    };
    let device = attempt(D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT)
        .or_else(|_| attempt(D3D11_CREATE_DEVICE_BGRA_SUPPORT))
        .context("D3D11CreateDevice")?;
    let mt: ID3D11Multithread = device.cast().context("ID3D11Multithread")?;
    // SAFETY: plain setter on a live device.
    let _ = unsafe { mt.SetMultithreadProtected(true) };
    Ok(device)
}

/// LUID of the adapter a device (and so every texture created on it) lives on.
pub fn device_luid(device: &ID3D11Device) -> Result<i64> {
    let dxgi: IDXGIDevice = device.cast()?;
    // SAFETY: valid device; returns its adapter.
    let adapter = unsafe { dxgi.GetAdapter() }?;
    // SAFETY: valid adapter.
    let desc = unsafe { adapter.GetDesc() }?;
    Ok(luid_i64(desc.AdapterLuid))
}

/// True when the device was created with `D3D11_CREATE_DEVICE_VIDEO_SUPPORT`.
pub fn has_video_support(device: &ID3D11Device) -> bool {
    // SAFETY: plain getter.
    let flags = unsafe { device.GetCreationFlags() };
    flags & D3D11_CREATE_DEVICE_VIDEO_SUPPORT.0 != 0
}

/// True when the device is multithread protected.
pub fn is_multithread_protected(device: &ID3D11Device) -> bool {
    device
        .cast::<ID3D11Multithread>()
        // SAFETY: plain getter.
        .map(|mt| unsafe { mt.GetMultithreadProtected() }.as_bool())
        .unwrap_or(false)
}
