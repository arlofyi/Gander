// SPDX-License-Identifier: Apache-2.0
// Moved from the stream spike (spikes/stream/capture, spikes/stream/encode) as production code.
//! Thin Windows Graphics Capture wrapper: one HWND -> BGRA8 `ID3D11Texture2D` frames on the
//! caller's device, delivered over a bounded channel with their QPC timestamps.
//!
//! The frame pool is free-threaded (no dispatcher queue or message loop needed); frames arrive on
//! a WinRT thread-pool thread and are forwarded as-is (zero copy). A [`Frame`] keeps its pool
//! buffer until it is dropped, so a consumer that holds frames throttles the capture instead of
//! racing it: with `buffers` pool buffers, at most `buffers` frames are in flight.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::Duration;

use anyhow::{Context, Result};
use windows::Foundation::Metadata::ApiInformation;
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{HSTRING, IInspectable, Interface};

use super::clock;

const SESSION_CLASS: &str = "Windows.Graphics.Capture.GraphicsCaptureSession";

/// Initialises WinRT (MTA) on the calling thread; harmless if already initialised.
pub fn init_winrt() {
    // SAFETY: plain initialisation call; S_FALSE / RPC_E_CHANGED_MODE are fine to ignore here.
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
}

/// True when WGC is available on this OS.
pub fn is_supported() -> bool {
    GraphicsCaptureSession::IsSupported().unwrap_or(false)
}

/// True when `GraphicsCaptureSession` has the named property on this build
/// (`IsCursorCaptureEnabled`, `IsBorderRequired`, `IncludeSecondaryWindows`, `MinUpdateInterval`,
/// `DirtyRegionMode`).
pub fn session_property_present(name: &str) -> bool {
    ApiInformation::IsPropertyPresent(&HSTRING::from(SESSION_CLASS), &HSTRING::from(name))
        .unwrap_or(false)
}

/// Session options; `None` leaves the OS default.
#[derive(Debug, Clone)]
pub struct CaptureOptions {
    /// `IsCursorCaptureEnabled`.
    pub cursor: Option<bool>,
    /// `IsBorderRequired` (false asks for no yellow border).
    pub border_required: Option<bool>,
    /// `IncludeSecondaryWindows` (popup menus, tooltips, owned dialogs).
    pub include_secondary_windows: Option<bool>,
    /// `MinUpdateInterval`.
    pub min_update_interval: Option<Duration>,
    /// Frame pool buffers (2..=4 sensible); also the channel capacity.
    pub buffers: i32,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        CaptureOptions {
            cursor: Some(false),
            border_required: None,
            include_secondary_windows: None,
            min_update_interval: None,
            buffers: 3,
        }
    }
}

/// Which options the OS accepted (`Err` text when a setter failed or the property is missing).
#[derive(Debug, Clone, Default)]
pub struct Applied {
    /// Result of setting `IsCursorCaptureEnabled`.
    pub cursor: Option<Result<(), String>>,
    /// Result of setting `IsBorderRequired`.
    pub border_required: Option<Result<(), String>>,
    /// Result of setting `IncludeSecondaryWindows`.
    pub include_secondary_windows: Option<Result<(), String>>,
    /// Result of setting `MinUpdateInterval`.
    pub min_update_interval: Option<Result<(), String>>,
}

/// One captured frame. Dropping it returns the buffer to the pool.
#[derive(Debug)]
pub struct Frame {
    frame: Direct3D11CaptureFrame,
    texture: ID3D11Texture2D,
    width: u32,
    height: u32,
    system_relative_time_100ns: i64,
    arrival_100ns: i64,
    seq: u64,
}

// SAFETY: `Direct3D11CaptureFrame` is an agile WinRT object and the texture belongs to a
// multithread-protected device ([`super::d3d::create_device`]); the frame is used by one thread
// at a time (moved through the channel), never shared.
unsafe impl Send for Frame {}

impl Frame {
    /// The BGRA8 texture, on the device passed to [`Capture::start`]. It can be larger than
    /// [`Frame::size`] for one frame after a resize; only the top-left `size()` is content.
    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }

    /// Content size in physical pixels (window bounds as DWM composes them).
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// `SystemRelativeTime`: when DWM composed the frame, QPC clock in 100 ns units.
    pub fn system_relative_time_100ns(&self) -> i64 {
        self.system_relative_time_100ns
    }

    /// When the `FrameArrived` handler ran, same clock.
    pub fn arrival_100ns(&self) -> i64 {
        self.arrival_100ns
    }

    /// Sequence number of delivered frames (starts at 0).
    pub fn seq(&self) -> u64 {
        self.seq
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        let _ = self.frame.Close();
    }
}

struct Shared {
    tx: SyncSender<Frame>,
    device: IDirect3DDevice,
    buffers: i32,
    last_size: std::sync::Mutex<SizeInt32>,
    closed: AtomicBool,
    seq: AtomicU64,
    dropped_full: AtomicU64,
    resized: AtomicU64,
}

// SAFETY: `IDirect3DDevice` wraps a multithread-protected D3D11 device and is agile; the rest
// is atomics, a mutex and a channel sender.
unsafe impl Send for Shared {}
// SAFETY: as above; every mutable field is atomic or behind a mutex.
unsafe impl Sync for Shared {}

/// A running capture of one window.
#[derive(Debug)]
pub struct Capture {
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    rx: Receiver<Frame>,
    shared: Arc<SharedHandle>,
    frame_token: i64,
    closed_token: i64,
    applied: Applied,
}

/// Debug-printable handle around [`Shared`].
struct SharedHandle(Shared);

impl std::fmt::Debug for SharedHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Shared")
    }
}

fn err_text(e: windows::core::Error) -> String {
    format!("0x{:08X} {}", e.code().0, e.message())
}

/// Wraps a D3D11 device for WinRT (`IDirect3DDevice`).
pub fn winrt_device(device: &ID3D11Device) -> Result<IDirect3DDevice> {
    let dxgi: IDXGIDevice = device.cast()?;
    // SAFETY: valid DXGI device.
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
        .context("CreateDirect3D11DeviceFromDXGIDevice")?;
    Ok(inspectable.cast()?)
}

/// Creates the capture item for a window.
pub fn item_for_window(hwnd: HWND) -> Result<GraphicsCaptureItem> {
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    // SAFETY: `hwnd` is a window handle; an invalid one returns an error.
    let item: GraphicsCaptureItem =
        unsafe { interop.CreateForWindow(hwnd) }.context("CreateForWindow")?;
    Ok(item)
}

impl Capture {
    /// Starts capturing `hwnd` into textures on `device`.
    pub fn start(hwnd: HWND, device: &ID3D11Device, opts: &CaptureOptions) -> Result<Capture> {
        init_winrt();
        anyhow::ensure!(is_supported(), "Windows Graphics Capture is not supported");
        let item = item_for_window(hwnd)?;
        let rt_device = winrt_device(device)?;
        let size = item.Size()?;
        let buffers = opts.buffers.clamp(1, 8);
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &rt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            buffers,
            size,
        )
        .context("CreateFreeThreaded")?;
        let session = pool.CreateCaptureSession(&item)?;

        let mut applied = Applied::default();
        if let Some(v) = opts.cursor {
            applied.cursor = Some(set_prop("IsCursorCaptureEnabled", || {
                session.SetIsCursorCaptureEnabled(v)
            }));
        }
        if let Some(v) = opts.border_required {
            applied.border_required = Some(set_prop("IsBorderRequired", || {
                session.SetIsBorderRequired(v)
            }));
        }
        if let Some(v) = opts.include_secondary_windows {
            applied.include_secondary_windows = Some(set_prop("IncludeSecondaryWindows", || {
                session.SetIncludeSecondaryWindows(v)
            }));
        }
        if let Some(d) = opts.min_update_interval {
            let ts = TimeSpan {
                Duration: (d.as_nanos() / 100) as i64,
            };
            applied.min_update_interval = Some(set_prop("MinUpdateInterval", || {
                session.SetMinUpdateInterval(ts)
            }));
        }

        let (tx, rx) = sync_channel(buffers as usize);
        let shared = Arc::new(SharedHandle(Shared {
            tx,
            device: rt_device,
            buffers,
            last_size: std::sync::Mutex::new(size),
            closed: AtomicBool::new(false),
            seq: AtomicU64::new(0),
            dropped_full: AtomicU64::new(0),
            resized: AtomicU64::new(0),
        }));

        let s = shared.clone();
        let frame_token = pool.FrameArrived(&TypedEventHandler::<
            Direct3D11CaptureFramePool,
            IInspectable,
        >::new(move |sender, _| {
            let Some(pool) = sender.as_ref() else {
                return Ok(());
            };
            on_frame(&s.0, pool)
        }))?;
        let s = shared.clone();
        let closed_token = item.Closed(
            &TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
                s.0.closed.store(true, Ordering::SeqCst);
                Ok(())
            }),
        )?;
        session.StartCapture().context("StartCapture")?;
        Ok(Capture {
            item,
            pool,
            session,
            rx,
            shared,
            frame_token,
            closed_token,
            applied,
        })
    }

    /// Waits up to `timeout` for the next frame.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Frame> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Discards frames already queued.
    pub fn drain(&self) {
        while self.rx.try_recv().is_ok() {}
    }

    /// Which options the OS accepted.
    pub fn applied(&self) -> &Applied {
        &self.applied
    }

    /// The item (window) was closed.
    pub fn is_closed(&self) -> bool {
        self.shared.0.closed.load(Ordering::SeqCst)
    }

    /// Frames dropped because the consumer's channel was full.
    pub fn dropped_full(&self) -> u64 {
        self.shared.0.dropped_full.load(Ordering::SeqCst)
    }

    /// Times the pool was recreated for a new content size.
    pub fn resized(&self) -> u64 {
        self.shared.0.resized.load(Ordering::SeqCst)
    }

    /// Current item size as WGC reports it.
    pub fn item_size(&self) -> Option<(i32, i32)> {
        self.item.Size().ok().map(|s| (s.Width, s.Height))
    }

    /// Stops the capture and releases the pool.
    pub fn stop(self) {
        drop(self);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.frame_token);
        let _ = self.item.RemoveClosed(self.closed_token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
        self.drain();
    }
}

fn set_prop(name: &str, f: impl FnOnce() -> windows::core::Result<()>) -> Result<(), String> {
    if !session_property_present(name) {
        return Err("property not present on this build".into());
    }
    f().map_err(err_text)
}

fn on_frame(s: &Shared, pool: &Direct3D11CaptureFramePool) -> windows::core::Result<()> {
    let arrival = clock::now_100ns();
    let frame = pool.TryGetNextFrame()?;
    let size = frame.ContentSize()?;
    {
        let mut last = s.last_size.lock().unwrap_or_else(|p| p.into_inner());
        if size.Width != last.Width || size.Height != last.Height {
            // The buffer still has the old size: release it, recreate the pool, wait for the next.
            let _ = frame.Close();
            pool.Recreate(
                &s.device,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                s.buffers,
                size,
            )?;
            *last = size;
            s.resized.fetch_add(1, Ordering::SeqCst);
            return Ok(());
        }
    }
    let surface = frame.Surface()?;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
    // SAFETY: the surface is a D3D11 texture created by the pool on our device.
    let texture: ID3D11Texture2D = unsafe { access.GetInterface() }?;
    let srt = frame.SystemRelativeTime()?.Duration;
    let out = Frame {
        frame,
        texture,
        width: size.Width.max(0) as u32,
        height: size.Height.max(0) as u32,
        system_relative_time_100ns: srt,
        arrival_100ns: arrival,
        seq: s.seq.fetch_add(1, Ordering::SeqCst),
    };
    match s.tx.try_send(out) {
        Ok(()) | Err(TrySendError::Disconnected(_)) => {}
        Err(TrySendError::Full(_)) => {
            s.dropped_full.fetch_add(1, Ordering::SeqCst);
        }
    }
    Ok(())
}

/// Where the window is, in physical screen pixels (the caller must be per-monitor DPI aware).
#[derive(Debug, Clone, Copy)]
pub struct WindowGeometry {
    /// `DWMWA_EXTENDED_FRAME_BOUNDS`: the visible window bounds, what WGC captures.
    pub frame: RECT,
    /// Client area origin on screen.
    pub client_origin: POINT,
    /// Client size.
    pub client_w: i32,
    /// Client size.
    pub client_h: i32,
    /// `GetDpiForWindow`.
    pub dpi: u32,
}

impl WindowGeometry {
    /// Reads the geometry of `hwnd`.
    pub fn of(hwnd: HWND) -> Result<WindowGeometry> {
        let mut frame = RECT::default();
        // SAFETY: valid out buffer of the declared size.
        unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                (&mut frame as *mut RECT).cast(),
                std::mem::size_of::<RECT>() as u32,
            )
        }
        .context("DwmGetWindowAttribute")?;
        let mut origin = POINT::default();
        // SAFETY: valid out pointer.
        let _ = unsafe { ClientToScreen(hwnd, &mut origin) };
        let mut rc = RECT::default();
        // SAFETY: valid out pointer.
        unsafe { GetClientRect(hwnd, &mut rc) }.context("GetClientRect")?;
        // SAFETY: plain getter.
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        Ok(WindowGeometry {
            frame,
            client_origin: origin,
            client_w: rc.right,
            client_h: rc.bottom,
            dpi,
        })
    }

    /// Size of the captured content WGC should deliver.
    pub fn frame_size(&self) -> (i32, i32) {
        (
            self.frame.right - self.frame.left,
            self.frame.bottom - self.frame.top,
        )
    }

    /// Client origin inside a captured frame (frame pixel = client pixel + this).
    pub fn client_offset(&self) -> (i32, i32) {
        (
            self.client_origin.x - self.frame.left,
            self.client_origin.y - self.frame.top,
        )
    }
}
