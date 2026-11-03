// SPDX-License-Identifier: Apache-2.0
//! The test window: class, window procedure, back buffer, popup menu, tooltip and dialog.
//!
//! Everything in this file runs on the window thread. Per-window state lives in a
//! reference-counted [`WindowCtx`] whose pointer is stored in `GWLP_USERDATA`; each message
//! handler takes its own counted reference, so the state stays alive even if a nested modal loop
//! (menu, message box) destroys the window underneath it.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use anyhow::{Result, bail};
use gander_testwin::layout::{BASE_DPI, Layout, Rect};
use gander_testwin::protocol::{CTRL_D, CTRL_P};
use gander_testwin::render::{Canvas, ClockTime, FrameState, draw_frame};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, BitBlt,
    CLIP_DEFAULT_PRECIS, ClientToScreen, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DeleteDC, DeleteObject, ETO_CLIPPED, EndPaint,
    ExtTextOutW, FF_DONTCARE, FW_NORMAL, GetDC, HBITMAP, HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, ReleaseDC, SRCCOPY, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Controls::{
    ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, TOOLTIPS_CLASSW, TTF_ABSOLUTE,
    TTF_TRACK, TTM_ADDTOOLW, TTM_TRACKACTIVATE, TTM_TRACKPOSITION, TTS_ALWAYSTIP, TTS_NOPREFIX,
    TTTOOLINFOW,
};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    DPI_AWARENESS_CONTEXT_UNAWARE, GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CREATESTRUCTW, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyMenu, DestroyWindow, DispatchMessageW, EndMenu, EnumThreadWindows, GWLP_USERDATA,
    GetClassNameW, GetClientRect, GetMessageW, GetWindowLongPtrW, IDC_ARROW, IsIconic, LoadCursorW,
    MB_ICONINFORMATION, MB_OK, MF_SEPARATOR, MF_STRING, MSG, MessageBoxW, PostMessageW,
    PostQuitMessage, RegisterClassExW, SHOW_WINDOW_CMD, SIZE_MINIMIZED, SW_MINIMIZE, SW_RESTORE,
    SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN,
    TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CHAR, WM_CLOSE, WM_DESTROY,
    WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_NCCREATE, WM_NCDESTROY, WM_PAINT,
    WM_PRINTCLIENT, WM_SIZE, WM_SYSKEYDOWN, WNDCLASSEXW, WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW,
    WS_POPUP,
};
use windows::core::{BOOL, PCWSTR, PWSTR, w};

use super::shared::{TYPED_KEEP, lock, shared};

/// Posted by the ticker once per frame.
pub const WM_APP_TICK: u32 = WM_APP + 1;
/// Sent by the pipe: open a second window; the result is its handle, 0 on failure.
pub const WM_APP_OPEN_SECOND: u32 = WM_APP + 2;
/// Sent by the pipe: minimise the main window.
pub const WM_APP_MINIMISE: u32 = WM_APP + 3;
/// Sent by the pipe: restore the main window.
pub const WM_APP_RESTORE: u32 = WM_APP + 4;
/// Posted by the pipe: open the popup menu and tooltip (modal, so never sent).
pub const WM_APP_POPUP: u32 = WM_APP + 5;
/// Posted by the pipe: open the modal dialog (never sent).
pub const WM_APP_DIALOG: u32 = WM_APP + 6;
/// Sent by the pipe: close the menu, tooltip and any dialog.
pub const WM_APP_DISMISS: u32 = WM_APP + 7;
/// Sent by the pipe (`park`): repaint every window now (frame 0 while parked).
pub const WM_APP_PARK: u32 = WM_APP + 8;

const CLASS_NAME: PCWSTR = w!("GanderTestWindow");
const TOOLTIP_TEXT: PCWSTR = w!("Gander test tooltip");

const ID_CYCLE_COLOUR: usize = 1001;
const ID_OPEN_SECOND: usize = 1002;
const ID_QUIT: usize = 1003;

/// Which window of the process this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The first window; closing it ends the process.
    Main,
    /// A window opened by `open-second` or the popup menu.
    Second,
}

/// A 32-bit top-down DIB section with its memory DC and caption font.
#[derive(Debug)]
struct Backbuffer {
    dc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    font: HFONT,
    old_font: HGDIOBJ,
    bits: *mut u32,
    w: i32,
    h: i32,
    dpi: u32,
}

impl Backbuffer {
    fn new(hwnd: HWND, w: i32, h: i32, dpi: u32) -> Option<Backbuffer> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        // SAFETY: the window DC is released before returning; `info` and `bits` outlive the
        // calls; the DIB section and memory DC are owned by the returned value and freed in Drop.
        unsafe {
            let screen = GetDC(Some(hwnd));
            let bitmap = CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0);
            let dc = CreateCompatibleDC(Some(screen));
            ReleaseDC(Some(hwnd), screen);
            let Ok(bitmap) = bitmap else {
                let _ = DeleteDC(dc);
                return None;
            };
            if bits.is_null() || dc.is_invalid() {
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(dc);
                return None;
            }
            let height_px = -(12 * dpi as i32 / BASE_DPI as i32);
            let font = CreateFontW(
                height_px,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                ANTIALIASED_QUALITY,
                (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
                w!("Segoe UI"),
            );
            let old_bitmap = SelectObject(dc, bitmap.into());
            let old_font = SelectObject(dc, font.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00C8C8C8));
            Some(Backbuffer {
                dc,
                bitmap,
                old_bitmap,
                font,
                old_font,
                bits: bits.cast(),
                w,
                h,
                dpi,
            })
        }
    }

    fn pixels(&mut self) -> &mut [u32] {
        // SAFETY: `bits` points at `w * h` pixels owned by the DIB section, which lives as long
        // as `self`, and `&mut self` guarantees exclusive access from this thread.
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize) }
    }

    /// Draws `text` with GDI into `rect`, clipped so it can never touch the colour patches.
    fn draw_caption(&self, rect: Rect, text: &str) {
        if rect.w <= 0 || rect.h <= 0 {
            return;
        }
        let wide: Vec<u16> = text.encode_utf16().collect();
        let clip = RECT {
            left: rect.x,
            top: rect.y,
            right: rect.right(),
            bottom: rect.bottom(),
        };
        // SAFETY: `dc` is the memory DC owned by `self`; `clip` and `wide` outlive the call.
        unsafe {
            let _ = ExtTextOutW(
                self.dc,
                rect.x,
                rect.y + rect.h / 4,
                ETO_CLIPPED,
                Some(&clip),
                PCWSTR(wide.as_ptr()),
                wide.len() as u32,
                None,
            );
        }
    }

    fn blit_to(&self, target: HDC) {
        // SAFETY: both DCs are valid for the call; the source is the memory DC owned by `self`.
        unsafe {
            let _ = BitBlt(target, 0, 0, self.w, self.h, Some(self.dc), 0, 0, SRCCOPY);
        }
    }
}

impl Drop for Backbuffer {
    fn drop(&mut self) {
        // SAFETY: the GDI objects were created in `new`, are deselected before deletion, and are
        // deleted exactly once.
        unsafe {
            SelectObject(self.dc, self.old_font);
            SelectObject(self.dc, self.old_bitmap);
            let _ = DeleteObject(self.font.into());
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Per-window state.
#[derive(Debug)]
struct WindowCtx {
    hwnd: Cell<HWND>,
    role: Role,
    title: String,
    /// Frame this window last painted; click hit tests use the box position of this frame.
    last_frame: Cell<u32>,
    backbuffer: RefCell<Option<Backbuffer>>,
    tooltip: Cell<HWND>,
}

impl WindowCtx {
    fn client_size(&self) -> Option<(i32, i32)> {
        let mut rc = RECT::default();
        // SAFETY: `rc` is a valid out pointer; a stale handle just makes the call fail.
        unsafe { GetClientRect(self.hwnd.get(), &mut rc) }.ok()?;
        Some((rc.right, rc.bottom))
    }

    fn dpi(&self) -> u32 {
        // SAFETY: plain query; returns 0 for an invalid handle.
        let dpi = unsafe { GetDpiForWindow(self.hwnd.get()) };
        if dpi == 0 { BASE_DPI } else { dpi }
    }

    fn layout(&self) -> Option<Layout> {
        let (w, h) = self.client_size()?;
        Some(Layout::new(w, h, self.dpi()))
    }

    fn minimised(&self) -> bool {
        // SAFETY: plain query on a window handle.
        unsafe { IsIconic(self.hwnd.get()) }.as_bool()
    }

    /// Draws `frame` into the back buffer and presents it. Skipped while minimised.
    fn render(&self, frame: u32) {
        if self.minimised() {
            return;
        }
        // `park`: the box stays at its frame-0 position, whatever repaints the window.
        let frame = if shared().parked.load(Ordering::Relaxed) {
            0
        } else {
            frame
        };
        let Some((w, h)) = self.client_size() else {
            return;
        };
        if w <= 0 || h <= 0 {
            return;
        }
        let s = shared();
        let dpi = self.dpi();
        let layout = Layout::new(w, h, dpi);
        {
            let mut slot = self.backbuffer.borrow_mut();
            if slot
                .as_ref()
                .is_none_or(|b| b.w != w || b.h != h || b.dpi != dpi)
            {
                *slot = Backbuffer::new(self.hwnd.get(), w, h, dpi);
            }
            let Some(bb) = slot.as_mut() else { return };
            let state = FrameState {
                frame,
                clock: local_clock(),
                box_rgb: s.box_rgb(),
            };
            if let Some(mut canvas) = Canvas::new(bb.pixels(), w as usize, h as usize) {
                draw_frame(&mut canvas, &layout, &state);
            }
            let caption = format!(
                "{}  |  pid {}  |  hits {} / clicks {}",
                self.title,
                std::process::id(),
                s.hits.load(Ordering::Relaxed),
                s.clicks.load(Ordering::Relaxed)
            );
            bb.draw_caption(layout.caption, &caption);
        }
        self.last_frame.set(frame);
        if self.role == Role::Main {
            *lock(&s.client) = (w, h, dpi);
            s.painted_frame.store(frame, Ordering::Relaxed);
        }
        // SAFETY: the window DC is released before returning.
        unsafe {
            let hdc = GetDC(Some(self.hwnd.get()));
            if let Some(bb) = self.backbuffer.borrow().as_ref() {
                bb.blit_to(hdc);
            }
            ReleaseDC(Some(self.hwnd.get()), hdc);
        }
    }

    /// Copies the back buffer to `hdc`; false when nothing has been drawn yet.
    fn paint_to(&self, hdc: HDC) -> bool {
        match self.backbuffer.borrow().as_ref() {
            Some(bb) => {
                bb.blit_to(hdc);
                true
            }
            None => false,
        }
    }
}

fn local_clock() -> ClockTime {
    // SAFETY: GetLocalTime has no preconditions.
    let t = unsafe { GetLocalTime() };
    ClockTime {
        hour: t.wHour as u8,
        minute: t.wMinute as u8,
        second: t.wSecond as u8,
        milli: t.wMilliseconds,
    }
}

/// The context of `hwnd` with its own counted reference, if the window belongs to this module.
fn ctx_of(hwnd: HWND) -> Option<Rc<WindowCtx>> {
    // SAFETY: plain query; 0 for windows without our context.
    let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowCtx;
    if raw.is_null() {
        return None;
    }
    // SAFETY: `raw` came from Rc::into_raw in `create_window` and is released only in
    // WM_NCDESTROY, on this same thread, so it is live here; taking an extra count first gives
    // this caller an independent reference.
    unsafe {
        Rc::increment_strong_count(raw);
        Some(Rc::from_raw(raw))
    }
}

fn def_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: forwards the arguments of the message being processed.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Enables per-monitor v2 DPI awareness so the window is never bitmap-scaled by the system.
pub fn set_dpi_awareness() {
    // SAFETY: plain call with a valid constant; failure (already set) is fine.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Makes the process DPI-unaware (`--dpi-unaware`): on a scaled monitor the system stretches the
/// window's 96-dpi bitmap and hands it logical coordinates, as for an old desktop app.
pub fn set_dpi_unaware() {
    // SAFETY: plain call with a valid constant; failure (already set) is fine.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_UNAWARE);
    }
}

/// Registers the tooltip control class and the window class.
pub fn register_class() -> Result<()> {
    let icc = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_BAR_CLASSES,
    };
    // SAFETY: `icc` is fully initialised and outlives the call.
    unsafe {
        let _ = InitCommonControlsEx(&icc);
    }
    // SAFETY: null module name means the current executable.
    let module = unsafe { GetModuleHandleW(None) }?;
    // SAFETY: loads a stock cursor.
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.ok();
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wnd_proc),
        hInstance: HINSTANCE(module.0),
        hCursor: cursor.unwrap_or_default(),
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    // SAFETY: `class` is fully initialised and the name and procedure are 'static.
    if unsafe { RegisterClassExW(&class) } == 0 {
        bail!(
            "RegisterClassExW failed: {}",
            windows::core::Error::from_thread()
        );
    }
    Ok(())
}

/// Creates a window of the test class and shows it with `show`.
pub fn create_window(role: Role, title: &str, show: SHOW_WINDOW_CMD) -> Result<HWND> {
    // SAFETY: plain query.
    let dpi = unsafe { GetDpiForSystem() };
    let layout = Layout::initial(dpi);
    let mut frame = RECT {
        left: 0,
        top: 0,
        right: layout.client_w,
        bottom: layout.client_h,
    };
    // SAFETY: `frame` is a valid in/out rectangle.
    unsafe {
        let _ = AdjustWindowRectExForDpi(
            &mut frame,
            WS_OVERLAPPEDWINDOW,
            false,
            WINDOW_EX_STYLE(0),
            dpi,
        );
    }
    let ctx = Rc::new(WindowCtx {
        hwnd: Cell::new(HWND::default()),
        role,
        title: title.to_owned(),
        last_frame: Cell::new(0),
        backbuffer: RefCell::new(None),
        tooltip: Cell::new(HWND::default()),
    });
    // The window owns this reference from WM_NCCREATE until WM_NCDESTROY.
    let raw = Rc::into_raw(ctx);
    let wide_title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: null module name means the current executable.
    let module = unsafe { GetModuleHandleW(None) }?;
    // SAFETY: the class is registered, `wide_title` is NUL terminated and outlives the call, and
    // `raw` stays valid until WM_NCDESTROY releases it.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            PCWSTR(wide_title.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            frame.right - frame.left,
            frame.bottom - frame.top,
            None,
            None,
            Some(HINSTANCE(module.0)),
            Some(raw.cast::<c_void>()),
        )
    }?;
    // SAFETY: plain call on the window just created.
    unsafe {
        let _ = ShowWindow(hwnd, show);
    }
    Ok(hwnd)
}

/// Runs the message loop until `WM_QUIT`.
pub fn message_loop() {
    let mut msg = MSG::default();
    // SAFETY: `msg` is a valid out pointer; the loop is the standard Win32 pump on the thread
    // that owns the windows.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Destroys any window still open (after the loop ended).
pub fn destroy_all() {
    let handles: Vec<isize> = lock(&shared().windows).clone();
    for h in handles {
        // SAFETY: DestroyWindow accepts any handle; it fails harmlessly for stale ones.
        unsafe {
            let _ = DestroyWindow(HWND(h as *mut c_void));
        }
    }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    dispatch(hwnd, msg, wparam, lparam)
}

fn dispatch(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: for WM_NCCREATE lParam points at the CREATESTRUCTW whose lpCreateParams is the
        // pointer passed to CreateWindowExW (the Rc raw pointer).
        let raw = unsafe { (*(lparam.0 as *const CREATESTRUCTW)).lpCreateParams };
        // SAFETY: stores our context pointer in the window's user data.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize) };
        if let Some(ctx) = ctx_of(hwnd) {
            ctx.hwnd.set(hwnd);
        }
        lock(&shared().windows).push(hwnd.0 as isize);
        return def_proc(hwnd, msg, wparam, lparam);
    }
    let Some(ctx) = ctx_of(hwnd) else {
        return def_proc(hwnd, msg, wparam, lparam);
    };
    let s = shared();
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            if ctx.backbuffer.borrow().is_none() {
                ctx.render(s.latest_frame.load(Ordering::Relaxed));
            }
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: Begin/EndPaint bracket the paint on the window thread; `ps` is a valid
            // out structure.
            unsafe {
                let hdc = BeginPaint(hwnd, &mut ps);
                ctx.paint_to(hdc);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_PRINTCLIENT => {
            // PrintWindow(PW_CLIENTONLY) and WM_PRINT land here with the target DC in wParam.
            if ctx.backbuffer.borrow().is_none() {
                ctx.render(s.latest_frame.load(Ordering::Relaxed));
            }
            ctx.paint_to(HDC(wparam.0 as *mut c_void));
            LRESULT(0)
        }
        WM_SIZE => {
            if wparam.0 as u32 != SIZE_MINIMIZED {
                ctx.render(
                    ctx.last_frame
                        .get()
                        .max(s.latest_frame.load(Ordering::Relaxed)),
                );
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // SAFETY: for WM_DPICHANGED lParam points at the suggested window rectangle.
            let r = unsafe { *(lparam.0 as *const RECT) };
            // SAFETY: plain call on this window.
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            on_click(&ctx, x, y);
            LRESULT(0)
        }
        WM_CHAR => {
            // Ctrl+P and Ctrl+D arrive as the control characters DLE (0x10) and EOT (0x04) once
            // `TranslateMessage` has run. Handling the character rather than the key-down means
            // a posted `WM_CHAR` works without keyboard focus or synchronised key state, which
            // is what the input-injection spike wants to try.
            let repeat = lparam.0 & (1 << 30) != 0;
            {
                let mut typed = lock(&s.typed);
                typed.push(wparam.0 as u16);
                let excess = typed.len().saturating_sub(TYPED_KEEP);
                typed.drain(..excess);
            }
            if !repeat {
                match wparam.0 as u32 {
                    CTRL_P => open_popup(&ctx),
                    CTRL_D => open_dialog(&ctx),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            s.keydowns.fetch_add(1, Ordering::Relaxed);
            def_proc(hwnd, msg, wparam, lparam)
        }
        WM_APP_TICK => {
            s.tick_pending.store(false, Ordering::Release);
            let frame = s.latest_frame.load(Ordering::Relaxed);
            let handles: Vec<isize> = lock(&s.windows).clone();
            for h in handles {
                if let Some(c) = ctx_of(HWND(h as *mut c_void)) {
                    c.render(frame);
                }
            }
            LRESULT(0)
        }
        WM_APP_OPEN_SECOND => {
            let title = lock(&s.pending_title)
                .take()
                .unwrap_or_else(|| format!("{} (second)", s.title));
            match create_window(
                Role::Second,
                &title,
                windows::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE,
            ) {
                Ok(h) => LRESULT(h.0 as isize),
                Err(e) => {
                    eprintln!("gander-testwin: open-second failed: {e:#}");
                    LRESULT(0)
                }
            }
        }
        WM_APP_MINIMISE => {
            // SAFETY: plain call on this window.
            unsafe {
                let _ = ShowWindow(hwnd, SW_MINIMIZE);
            }
            LRESULT(1)
        }
        WM_APP_RESTORE => {
            // SAFETY: plain call on this window.
            unsafe {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            LRESULT(1)
        }
        WM_APP_POPUP => {
            open_popup(&ctx);
            LRESULT(0)
        }
        WM_APP_DIALOG => {
            open_dialog(&ctx);
            LRESULT(0)
        }
        WM_APP_DISMISS => {
            dismiss_all(&ctx);
            LRESULT(1)
        }
        WM_APP_PARK => {
            let handles: Vec<isize> = lock(&s.windows).clone();
            for h in handles {
                if let Some(c) = ctx_of(HWND(h as *mut c_void)) {
                    c.render(0);
                }
            }
            LRESULT(1)
        }
        WM_CLOSE => def_proc(hwnd, msg, wparam, lparam),
        WM_DESTROY => {
            if ctx.role == Role::Main {
                // SAFETY: plain call on the window thread.
                unsafe { PostQuitMessage(0) };
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            lock(&s.windows).retain(|h| *h != hwnd.0 as isize);
            // SAFETY: clears the user data, then releases the window's own reference (taken by
            // Rc::into_raw in `create_window`); the reference held by `ctx` keeps the state
            // alive for the rest of this call.
            unsafe {
                let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowCtx;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                if !raw.is_null() {
                    drop(Rc::from_raw(raw));
                }
            }
            def_proc(hwnd, msg, wparam, lparam)
        }
        _ => def_proc(hwnd, msg, wparam, lparam),
    }
}

/// Counts the click and, when it landed on the box, advances the box colour.
fn on_click(ctx: &WindowCtx, x: i32, y: i32) {
    let s = shared();
    s.clicks.fetch_add(1, Ordering::Relaxed);
    s.set_last_click(x, y);
    let Some(layout) = ctx.layout() else { return };
    if layout.box_rect(ctx.last_frame.get()).contains(x, y) {
        s.hits.fetch_add(1, Ordering::Relaxed);
        s.cycle_colour();
        // Show the new colour at once instead of on the next tick.
        ctx.render(ctx.last_frame.get());
    }
}

fn to_screen(hwnd: HWND, x: i32, y: i32) -> POINT {
    let mut p = POINT { x, y };
    // SAFETY: `p` is a valid in/out point.
    unsafe {
        let _ = ClientToScreen(hwnd, &mut p);
    }
    p
}

/// `TTTOOLINFOW` sized for comctl32 v5 (this executable has no v6 manifest).
fn tool_info(hwnd: HWND) -> TTTOOLINFOW {
    TTTOOLINFOW {
        cbSize: std::mem::offset_of!(TTTOOLINFOW, lpReserved) as u32,
        uFlags: TTF_TRACK | TTF_ABSOLUTE,
        hwnd,
        uId: 1,
        lpszText: PWSTR(TOOLTIP_TEXT.0.cast_mut()),
        ..Default::default()
    }
}

fn ensure_tooltip(ctx: &WindowCtx) -> Option<HWND> {
    if !ctx.tooltip.get().is_invalid() {
        return Some(ctx.tooltip.get());
    }
    // SAFETY: plain window creation owned by `ctx.hwnd`; owned windows die with their owner.
    let tip = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASSW,
            PCWSTR::null(),
            WS_POPUP | WINDOW_STYLE_BITS(TTS_ALWAYSTIP | TTS_NOPREFIX),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            Some(ctx.hwnd.get()),
            None,
            None,
            None,
        )
    }
    .ok()?;
    let info = tool_info(ctx.hwnd.get());
    // SAFETY: `info` outlives the synchronous call and its text pointer is 'static.
    unsafe {
        SendMessageW(
            tip,
            TTM_ADDTOOLW,
            None,
            Some(LPARAM(&info as *const _ as isize)),
        );
    }
    ctx.tooltip.set(tip);
    Some(tip)
}

/// Window style bits for the tooltip control's own `TTS_*` flags.
#[allow(non_snake_case)]
fn WINDOW_STYLE_BITS(bits: u32) -> windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE {
    windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(bits)
}

fn show_tooltip(ctx: &WindowCtx, at: POINT) {
    let Some(tip) = ensure_tooltip(ctx) else {
        return;
    };
    let info = tool_info(ctx.hwnd.get());
    let pos = (at.x as u16 as isize) | ((at.y as u16 as isize) << 16);
    // SAFETY: `info` outlives the synchronous calls; `tip` is a live tooltip control.
    unsafe {
        SendMessageW(tip, TTM_TRACKPOSITION, None, Some(LPARAM(pos)));
        SendMessageW(
            tip,
            TTM_TRACKACTIVATE,
            Some(WPARAM(1)),
            Some(LPARAM(&info as *const _ as isize)),
        );
    }
    shared().tooltip_visible.store(true, Ordering::Relaxed);
}

fn hide_tooltip(ctx: &WindowCtx) {
    let tip = ctx.tooltip.get();
    if !tip.is_invalid() {
        let info = tool_info(ctx.hwnd.get());
        // SAFETY: `info` outlives the synchronous call; `tip` is a live tooltip control.
        unsafe {
            SendMessageW(
                tip,
                TTM_TRACKACTIVATE,
                Some(WPARAM(0)),
                Some(LPARAM(&info as *const _ as isize)),
            );
        }
    }
    shared().tooltip_visible.store(false, Ordering::Relaxed);
}

/// Ctrl+P: shows the tooltip and the popup menu together at fixed spots in the arena. The menu is
/// modal on this thread; the frame clock keeps posting ticks into its message loop, so the
/// animation continues underneath.
fn open_popup(ctx: &Rc<WindowCtx>) {
    let s = shared();
    if s.menu_open.swap(true, Ordering::AcqRel) {
        return;
    }
    let hwnd = ctx.hwnd.get();
    let layout = ctx.layout().unwrap_or_else(|| Layout::initial(BASE_DPI));
    let unit = |v: i32| v * layout.dpi as i32 / BASE_DPI as i32;
    show_tooltip(
        ctx,
        to_screen(hwnd, layout.arena.x + unit(16), layout.arena.y + unit(8)),
    );

    // SAFETY: the menu is created, shown and destroyed within this function on the window
    // thread; the strings are 'static.
    let chosen = unsafe {
        match CreatePopupMenu() {
            Ok(menu) => {
                let _ = AppendMenuW(menu, MF_STRING, ID_CYCLE_COLOUR, w!("Cycle box colour"));
                let _ = AppendMenuW(menu, MF_STRING, ID_OPEN_SECOND, w!("Open second window"));
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("Quit"));
                let at = to_screen(hwnd, layout.arena.x + unit(16), layout.arena.y + unit(56));
                let _ = SetForegroundWindow(hwnd);
                let id = TrackPopupMenu(
                    menu,
                    TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RIGHTBUTTON,
                    at.x,
                    at.y,
                    None,
                    hwnd,
                    None,
                )
                .0;
                let _ = DestroyMenu(menu);
                id as usize
            }
            Err(_) => 0,
        }
    };
    hide_tooltip(ctx);
    s.menu_open.store(false, Ordering::Release);
    match chosen {
        ID_CYCLE_COLOUR => s.cycle_colour(),
        ID_OPEN_SECOND => {
            let _ = create_window(
                Role::Second,
                &format!("{} (second)", s.title),
                windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
            );
        }
        ID_QUIT => {
            // SAFETY: plain post to this window.
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        _ => {}
    }
}

/// Ctrl+D: a modal message box owned by this window.
fn open_dialog(ctx: &Rc<WindowCtx>) {
    let s = shared();
    if s.dialog_open.swap(true, Ordering::AcqRel) {
        return;
    }
    // SAFETY: the owner handle is live and the strings are 'static; the call returns when the
    // box is closed.
    unsafe {
        MessageBoxW(
            Some(ctx.hwnd.get()),
            w!("Gander test dialog (modal)"),
            w!("gander-testwin"),
            MB_OK | MB_ICONINFORMATION,
        );
    }
    s.dialog_open.store(false, Ordering::Release);
}

extern "system" fn close_if_dialog(hwnd: HWND, _lparam: LPARAM) -> BOOL {
    let mut class = [0u16; 16];
    // SAFETY: `class` is a valid buffer for the call.
    let n = unsafe { GetClassNameW(hwnd, &mut class) };
    // "#32770" is the class of message boxes and standard dialogs.
    if n > 0 && String::from_utf16_lossy(&class[..n as usize]) == "#32770" {
        // SAFETY: plain post to a window of this thread.
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    true.into()
}

/// Closes the popup menu, the tooltip and any modal dialog.
fn dismiss_all(ctx: &WindowCtx) {
    // SAFETY: EndMenu is called on the window thread; EnumThreadWindows calls back on this
    // thread only while it runs.
    unsafe {
        let _ = EndMenu();
        let _ = EnumThreadWindows(GetCurrentThreadId(), Some(close_if_dialog), LPARAM(0));
    }
    hide_tooltip(ctx);
}
