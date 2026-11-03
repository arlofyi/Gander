// SPDX-License-Identifier: Apache-2.0
//! End-to-end tests: launch the real `gander-testwin` window, drive it over its control pipe and
//! read its pixels back with `PrintWindow`.
//!
//! These tests open a real window on the interactive desktop for a few seconds (shown without
//! activation, so they do not take keyboard focus). They need a desktop session, as the CI
//! runner and a logged-in developer machine have.

#![cfg(windows)]

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::AsRawHandle;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use gander_testwin::layout::{BOX_PALETTE, COUNTER_MASK, Layout, PATCH_A_RGB, PATCH_B_RGB, Rgb};
use gander_testwin::protocol::{CTRL_D, CTRL_P};
use gander_testwin::render::{decode_counter, patch_matches};
use gander_testwin::sec::current_user_sid_string;

use windows::Win32::Foundation::{HANDLE, HLOCAL, HWND, LPARAM, LocalFree, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
};
use windows::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
    SE_KERNEL_OBJECT,
};
use windows::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};
use windows::Win32::Storage::Xps::{PW_CLIENTONLY, PrintWindow};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
    VK_CONTROL, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetForegroundWindow, GetWindowTextW, PostMessageW, SetForegroundWindow, WM_CHAR,
    WM_LBUTTONDOWN,
};
use windows::core::PWSTR;

const EXE: &str = env!("CARGO_BIN_EXE_gander-testwin");

/// A running window process; killed on drop so a failing test never leaves one behind.
struct Testwin {
    child: Child,
    pipe: String,
    pid: u32,
    main_hwnd: isize,
}

impl Testwin {
    fn spawn(extra: &[&str]) -> Testwin {
        let mut child = Command::new(EXE)
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn gander-testwin");
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("read the ready line");
        let line = line.trim().to_owned();
        assert!(
            line.starts_with("gander-testwin ready "),
            "unexpected first line: {line:?}"
        );
        let field = |key: &str| -> String {
            line.split_whitespace()
                .find_map(|w| w.strip_prefix(key))
                .unwrap_or_else(|| panic!("no {key} in {line:?}"))
                .to_owned()
        };
        Testwin {
            pid: field("pid=").parse().unwrap(),
            pipe: field("pipe="),
            main_hwnd: isize::from_str_radix(field("hwnd=0x").as_str(), 16).unwrap(),
            child,
        }
    }

    fn client(&self) -> Client {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // ERROR_PIPE_BUSY (231) means another client is connected; retry for a moment.
            match OpenOptions::new().read(true).write(true).open(&self.pipe) {
                Ok(f) => {
                    return Client {
                        io: BufReader::new(f),
                    };
                }
                Err(e) if e.raw_os_error() == Some(231) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("cannot open {}: {e}", self.pipe),
            }
        }
    }

    fn wait_exit(&mut self, within: Duration) -> std::process::ExitStatus {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "process did not exit within {within:?}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for Testwin {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Client {
    io: BufReader<File>,
}

impl Client {
    fn send(&mut self, line: &str) -> String {
        let f = self.io.get_mut();
        f.write_all(format!("{line}\n").as_bytes()).unwrap();
        f.flush().unwrap();
        let mut reply = String::new();
        self.io.read_line(&mut reply).unwrap();
        reply.trim_end().to_owned()
    }

    fn state(&mut self) -> Reply {
        Reply::new(self.send("state?"))
    }
}

/// A reply line with `key=value` access.
struct Reply(String);

impl Reply {
    fn new(s: String) -> Reply {
        assert!(s.starts_with("ok"), "expected ok, got {s:?}");
        Reply(s)
    }

    fn get(&self, key: &str) -> &str {
        let prefix = format!("{key}=");
        self.0
            .split_whitespace()
            .find_map(|w| w.strip_prefix(prefix.as_str()))
            .unwrap_or_else(|| panic!("no {key} in {:?}", self.0))
    }

    fn num(&self, key: &str) -> u32 {
        self.get(key)
            .parse()
            .unwrap_or_else(|_| panic!("{key} not a number in {:?}", self.0))
    }
}

fn eventually(what: &str, within: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn hwnd(h: isize) -> HWND {
    HWND(h as *mut c_void)
}

/// Pixels of a window's client area as `0x00RRGGBB`, read with `PrintWindow(PW_CLIENTONLY)`.
struct Shot {
    w: i32,
    h: i32,
    px: Vec<u32>,
}

/// Makes this test process per-monitor DPI aware. Without it Windows virtualises the client
/// rectangle of another process's window (640x480 instead of the real 960x720 at 150 %), and
/// every pixel position would be off. Anything that reads the window's pixels must do the same.
fn become_dpi_aware() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: plain process-wide call with a valid constant.
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    });
}

impl Shot {
    fn take(window: isize) -> Shot {
        become_dpi_aware();
        let mut rc = RECT::default();
        // SAFETY: `rc` is a valid out pointer.
        unsafe { GetClientRect(hwnd(window), &mut rc) }.expect("GetClientRect");
        let (w, h) = (rc.right, rc.bottom);
        assert!(w > 0 && h > 0, "empty client area {w}x{h}");
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
        // SAFETY: standard GDI sequence; every object created here is released below, and the
        // pixel copy reads exactly the `w * h` pixels of the DIB section while it is selected.
        let px = unsafe {
            let screen = GetDC(None);
            let dib = CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0)
                .expect("CreateDIBSection");
            let dc = CreateCompatibleDC(Some(screen));
            let old = SelectObject(dc, dib.into());
            let printed = PrintWindow(hwnd(window), dc, PW_CLIENTONLY);
            assert!(printed.as_bool(), "PrintWindow failed");
            let px = std::slice::from_raw_parts(bits.cast::<u32>(), (w * h) as usize).to_vec();
            SelectObject(dc, old);
            let _ = DeleteObject(dib.into());
            let _ = DeleteDC(dc);
            ReleaseDC(None, screen);
            px
        };
        let shot = Shot { w, h, px };
        shot.dump_if_requested(window);
        shot
    }

    /// With `GANDER_TESTWIN_DUMP_DIR` set, writes the shot as a binary PPM for eyeballing.
    fn dump_if_requested(&self, window: isize) {
        let Some(dir) = std::env::var_os("GANDER_TESTWIN_DUMP_DIR") else {
            return;
        };
        let mut data = format!(
            "P6
{} {}
255
",
            self.w, self.h
        )
        .into_bytes();
        for p in &self.px {
            data.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
        }
        let _ = std::fs::write(
            std::path::Path::new(&dir).join(format!("shot-{window:x}.ppm")),
            data,
        );
    }

    fn at(&self, x: i32, y: i32) -> Option<Rgb> {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return None;
        }
        let p = self.px[(y * self.w + x) as usize];
        Some(Rgb((p >> 16) as u8, (p >> 8) as u8, p as u8))
    }
}

fn dpi_of(c: &mut Client) -> u32 {
    Reply::new(c.send("layout?")).num("dpi")
}

/// Reads the counter and checks both patches of `window`; returns the decoded frame number.
fn verify_pixels(c: &mut Client, window: isize) -> u32 {
    let dpi = dpi_of(c);
    let shot = Shot::take(window);
    let layout = Layout::new(shot.w, shot.h, dpi);
    let at = |x, y| shot.at(x, y);
    assert!(
        patch_matches(layout.patch_a, PATCH_A_RGB, 0, at),
        "patch A is not exact (255,0,255)"
    );
    assert!(
        patch_matches(layout.patch_b, PATCH_B_RGB, 0, at),
        "patch B is not exact (0,255,0)"
    );
    decode_counter(&layout, at).expect("counter strip is readable")
}

#[test]
fn self_test_exits_by_itself_with_code_zero() {
    let started = Instant::now();
    let mut w = Testwin::spawn(&["--self-test-secs", "2", "--no-focus"]);
    let status = w.wait_exit(Duration::from_secs(10));
    let took = started.elapsed();
    assert!(status.success(), "exit status {status:?}");
    assert!(
        took >= Duration::from_millis(1800),
        "exited early after {took:?}"
    );
    assert!(took < Duration::from_secs(8), "exited late after {took:?}");
}

/// Opens the popup menu (a modal loop that ignores a bare `WM_QUIT`) and waits until it is up.
fn open_menu(c: &mut Client) {
    assert_eq!(c.send("popup"), "ok");
    eventually("menu open", Duration::from_secs(3), || {
        c.state().num("menu") == 1
    });
}

#[test]
fn self_test_deadline_ends_the_process_even_with_the_popup_menu_open() {
    let started = Instant::now();
    let mut w = Testwin::spawn(&["--self-test-secs", "3", "--no-focus"]);
    open_menu(&mut w.client());
    let status = w.wait_exit(Duration::from_secs(10));
    assert!(
        status.success(),
        "exit status {status:?} (4 means the forced-exit backstop fired)"
    );
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn quit_command_ends_the_process_even_with_the_popup_menu_open() {
    let mut w = Testwin::spawn(&["--self-test-secs", "30", "--no-focus"]);
    let mut c = w.client();
    open_menu(&mut c);
    assert_eq!(c.send("quit"), "ok");
    let status = w.wait_exit(Duration::from_secs(8));
    assert!(
        status.success(),
        "exit status {status:?} (4 means the forced-exit backstop fired)"
    );
}

#[test]
fn help_and_usage_errors() {
    let help = Command::new(EXE).arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--self-test"));
    let bad = Command::new(EXE).arg("--no-such-flag").output().unwrap();
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unknown option"));
}

#[test]
fn window_pipe_and_pixels_end_to_end() {
    let mut tw = Testwin::spawn(&[
        "--self-test-secs",
        "45",
        "--no-focus",
        "Gander",
        "WP-40",
        "test",
    ]);
    assert_eq!(tw.pipe, format!(r"\\.\pipe\gander-testwin-{}", tw.pid));
    let mut c = tw.client();

    // The pipe is restricted to the current user: one protected allow entry for our SID.
    {
        let handle = HANDLE(c.io.get_ref().as_raw_handle());
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `handle` is the open pipe handle; the descriptor returned is freed below.
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                Some(&mut sd),
            )
        };
        assert_eq!(status.0, 0, "GetSecurityInfo");
        let mut text = PWSTR::null();
        // SAFETY: `sd` was just returned by GetSecurityInfo; `text` is a valid out pointer.
        unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                sd,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                None,
            )
        }
        .unwrap();
        // SAFETY: `text` is a NUL terminated string allocated by the call above.
        let sddl = unsafe { text.to_string() }.unwrap();
        // SAFETY: both allocations came from the system and are released once.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(text.0.cast())));
            let _ = LocalFree(Some(HLOCAL(sd.0)));
        }
        let sid = current_user_sid_string().unwrap();
        assert!(sddl.starts_with("D:P(A;"), "DACL must be protected: {sddl}");
        assert_eq!(
            sddl.matches('(').count(),
            1,
            "exactly one ACE expected: {sddl}"
        );
        assert!(
            sddl.contains(&format!(";;;{sid})")),
            "ACE must be for {sid}: {sddl}"
        );
    }

    // Basic protocol behaviour and errors.
    assert_eq!(c.send("ping"), "ok pong");
    assert_eq!(c.send("nonsense"), "err unknown-command");
    assert!(c.send("ping extra").starts_with("err bad-argument"));
    assert!(c.send("tone 5").starts_with("err bad-argument"));
    // Longer than 1 s is refused (the tone itself is played only by the audio tests, WP-55).
    assert!(c.send("tone 440 1001").starts_with("err bad-argument"));

    // layout? agrees with the library's geometry for the live client size and dpi.
    let layout_reply = Reply::new(c.send("layout?"));
    let dpi = layout_reply.num("dpi");
    let (cw, ch) = layout_reply.get("client").split_once('x').unwrap();
    let layout = Layout::new(cw.parse().unwrap(), ch.parse().unwrap(), dpi);
    assert_eq!(layout_reply.get("patch_a"), layout.patch_a.csv());
    assert_eq!(layout_reply.get("patch_b"), layout.patch_b.csv());
    assert_eq!(layout_reply.get("strip"), layout.strip.csv());
    assert_eq!(layout_reply.get("arena"), layout.arena.csv());
    assert_eq!(layout_reply.get("patch_a_rgb"), "FF00FF");
    assert_eq!(layout_reply.get("patch_b_rgb"), "00FF00");
    assert_eq!(layout_reply.get("strip_bits"), "24");

    // The frame counter runs at 60 Hz. The frame number counts from process start, so first wait
    // until painting has settled, then compare the counter with the wall clock.
    eventually("first frames painted", Duration::from_secs(5), || {
        Reply::new(c.send("frame?")).num("frame") > 5
    });
    let t0 = Instant::now();
    let f1 = Reply::new(c.send("frame?")).num("frame");
    std::thread::sleep(Duration::from_millis(1000));
    let f2 = Reply::new(c.send("frame?")).num("frame");
    let secs = t0.elapsed().as_secs_f64();
    let rate = f64::from(f2.wrapping_sub(f1)) / secs;
    assert!(
        (50.0..=70.0).contains(&rate),
        "frame counter ran at {rate:.1} frames/s"
    );

    // Pixels: exact patches, and a counter that lies between two pipe readings.
    let before = Reply::new(c.send("frame?")).num("frame") & COUNTER_MASK;
    let decoded = verify_pixels(&mut c, tw.main_hwnd);
    let after = Reply::new(c.send("frame?")).num("frame") & COUNTER_MASK;
    assert!(
        before <= decoded && decoded <= after,
        "decoded counter {decoded} outside [{before}, {after}]"
    );

    // The window title comes from argv.
    assert_eq!(window_text(tw.main_hwnd), "Gander WP-40 test");
    let windows = Reply::new(c.send("windows?"));
    assert_eq!(windows.num("count"), 1);

    // Clicks: a hit on the box changes its colour; a miss only counts as a click.
    let s0 = c.state();
    let colour0 = s0.get("colour").to_owned();
    assert_eq!(colour0, BOX_PALETTE[0].hex());
    let bx: Vec<i32> = s0
        .get("box")
        .split(',')
        .map(|v| v.parse().unwrap())
        .collect();
    let (hit_x, hit_y) = (bx[0] + bx[2] / 2, bx[1] + bx[3] / 2);
    post_click(tw.main_hwnd, hit_x, hit_y);
    eventually(
        "colour change after a click on the box",
        Duration::from_secs(3),
        || c.state().get("colour") != colour0,
    );
    let s1 = c.state();
    assert_eq!(s1.get("colour"), BOX_PALETTE[1].hex());
    assert_eq!(s1.num("hits"), 1);
    assert_eq!(s1.num("clicks"), 1);
    // Patch A centre can never be under the box.
    let (miss_x, miss_y) = layout.patch_a.centre();
    post_click(tw.main_hwnd, miss_x, miss_y);
    eventually("miss counted as a click", Duration::from_secs(3), || {
        c.state().num("clicks") == 2
    });
    let s2 = c.state();
    assert_eq!(s2.num("hits"), 1);
    assert_eq!(s2.get("colour"), BOX_PALETTE[1].hex());
    assert_eq!(s2.get("last_click"), format!("{miss_x},{miss_y}"));

    // A second window of the same process, with its own handle and the same content.
    let second = Reply::new(c.send("open-second Second window"));
    let second_hwnd =
        isize::from_str_radix(second.get("hwnd").trim_start_matches("0x"), 16).unwrap();
    assert_ne!(second_hwnd, tw.main_hwnd);
    assert_eq!(window_text(second_hwnd), "Second window");
    let windows = Reply::new(c.send("windows?"));
    assert_eq!(windows.num("count"), 2);
    assert!(
        windows
            .get("hwnd")
            .starts_with(&format!("0x{:X}", tw.main_hwnd))
    );
    std::thread::sleep(Duration::from_millis(150));
    verify_pixels(&mut c, second_hwnd);

    // Popup menu and tooltip: both open, the animation keeps running underneath, dismiss closes.
    assert_eq!(c.send("popup"), "ok");
    eventually("menu and tooltip open", Duration::from_secs(3), || {
        let s = c.state();
        s.num("menu") == 1 && s.num("tooltip") == 1
    });
    let m1 = Reply::new(c.send("frame?")).num("frame");
    std::thread::sleep(Duration::from_millis(400));
    let m2 = Reply::new(c.send("frame?")).num("frame");
    assert!(
        m2 > m1 + 10,
        "frames stalled while the menu was open: {m1} -> {m2}"
    );
    assert_eq!(c.send("dismiss"), "ok");
    eventually("menu and tooltip closed", Duration::from_secs(3), || {
        let s = c.state();
        s.num("menu") == 0 && s.num("tooltip") == 0
    });

    // Keyboard path: Ctrl+P and Ctrl+D reach the window as WM_CHAR control codes, so posting
    // them works without focus.
    post_char(tw.main_hwnd, CTRL_P);
    eventually(
        "Ctrl+P opens menu and tooltip",
        Duration::from_secs(3),
        || {
            let s = c.state();
            s.num("menu") == 1 && s.num("tooltip") == 1
        },
    );
    assert_eq!(c.send("dismiss"), "ok");
    eventually(
        "menu and tooltip closed again",
        Duration::from_secs(3),
        || {
            let s = c.state();
            s.num("menu") == 0 && s.num("tooltip") == 0
        },
    );
    post_char(tw.main_hwnd, CTRL_D);
    eventually("Ctrl+D opens the dialog", Duration::from_secs(3), || {
        c.state().num("dialog") == 1
    });
    assert_eq!(c.send("dismiss"), "ok");
    eventually("dialog closed again", Duration::from_secs(3), || {
        c.state().num("dialog") == 0
    });

    // Modal dialog.
    assert_eq!(c.send("dialog"), "ok");
    eventually("dialog open", Duration::from_secs(3), || {
        c.state().num("dialog") == 1
    });
    let d1 = Reply::new(c.send("frame?")).num("frame");
    std::thread::sleep(Duration::from_millis(400));
    let d2 = Reply::new(c.send("frame?")).num("frame");
    assert!(
        d2 > d1 + 10,
        "frames stalled while the dialog was open: {d1} -> {d2}"
    );
    assert_eq!(c.send("dismiss"), "ok");
    eventually("dialog closed", Duration::from_secs(3), || {
        c.state().num("dialog") == 0
    });

    // Minimise: painting stops; restore: it resumes.
    assert_eq!(c.send("minimise"), "ok");
    eventually("minimised", Duration::from_secs(3), || {
        c.state().num("minimised") == 1
    });
    std::thread::sleep(Duration::from_millis(100));
    let p1 = Reply::new(c.send("frame?")).num("frame");
    std::thread::sleep(Duration::from_millis(300));
    let p2 = Reply::new(c.send("frame?")).num("frame");
    assert_eq!(p1, p2, "main window kept painting while minimised");
    assert_eq!(c.send("restore"), "ok");
    eventually("restored", Duration::from_secs(3), || {
        c.state().num("minimised") == 0
    });
    eventually("painting resumes", Duration::from_secs(3), || {
        Reply::new(c.send("frame?")).num("frame") > p2 + 5
    });

    // colour? is the short form.
    let colour = Reply::new(c.send("colour?"));
    assert_eq!(colour.get("colour"), BOX_PALETTE[1].hex());
    assert_eq!(colour.num("index"), 1);

    // quit shuts the process down cleanly even while a modal dialog is open.
    assert_eq!(c.send("dialog"), "ok");
    eventually("dialog open again", Duration::from_secs(3), || {
        c.state().num("dialog") == 1
    });
    assert_eq!(c.send("quit"), "ok");
    let status = tw.wait_exit(Duration::from_secs(8));
    assert!(status.success(), "exit status {status:?}");
}

fn post_click(window: isize, x: i32, y: i32) {
    let lparam = ((y as u16 as isize) << 16) | (x as u16 as isize);
    // SAFETY: posting a mouse message to a window of the test process; a stale handle just fails.
    unsafe {
        PostMessageW(
            Some(hwnd(window)),
            WM_LBUTTONDOWN,
            WPARAM(1),
            LPARAM(lparam),
        )
    }
    .expect("PostMessageW");
}

fn post_char(window: isize, code: u32) {
    // SAFETY: posting a character message to a window of the test process; a stale handle just
    // fails.
    unsafe {
        PostMessageW(
            Some(hwnd(window)),
            WM_CHAR,
            WPARAM(code as usize),
            LPARAM(1),
        )
    }
    .expect("PostMessageW");
}

fn window_text(window: isize) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: `buf` is a valid buffer for the call.
    let n = unsafe { GetWindowTextW(hwnd(window), &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    Default::default()
                },
                ..Default::default()
            },
        },
    }
}

/// Real keystrokes: Ctrl+P opens the popup menu and the tooltip, Escape closes them.
///
/// Ignored by default because it sends keyboard input to whatever window is in the foreground. It
/// only types after confirming the test window is the foreground window, and skips (passes with a
/// note) when Windows will not hand it the foreground. Run with
/// `cargo test -p gander-testwin --test window -- --ignored --nocapture` on an idle desktop.
#[test]
#[ignore = "sends real keystrokes; needs the desktop foreground"]
fn ctrl_p_opens_popup_menu_and_tooltip() {
    let mut tw = Testwin::spawn(&["--self-test-secs", "30", "Ctrl+P test"]);
    let mut c = tw.client();
    let target = hwnd(tw.main_hwnd);
    let is_foreground = || {
        // SAFETY: plain query.
        let foreground = unsafe { GetForegroundWindow() };
        foreground == target
    };
    // SAFETY: plain call on the test window.
    let _ = unsafe { SetForegroundWindow(target) };
    std::thread::sleep(Duration::from_millis(500));
    if !is_foreground() {
        eprintln!("SKIPPED: Windows did not give the test window the foreground");
        let _ = c.send("quit");
        return;
    }
    let send = |inputs: &[INPUT]| {
        assert!(is_foreground(), "lost the foreground; refusing to type");
        // SAFETY: `inputs` is a valid slice of fully initialised INPUT records.
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
        assert_eq!(sent as usize, inputs.len());
    };
    send(&[
        key(VK_CONTROL, false),
        key(VIRTUAL_KEY(0x50), false),
        key(VIRTUAL_KEY(0x50), true),
        key(VK_CONTROL, true),
    ]);
    eventually(
        "Ctrl+P opens menu and tooltip",
        Duration::from_secs(3),
        || {
            let s = c.state();
            s.num("menu") == 1 && s.num("tooltip") == 1
        },
    );
    send(&[key(VK_ESCAPE, false), key(VK_ESCAPE, true)]);
    eventually(
        "Escape closes menu and tooltip",
        Duration::from_secs(3),
        || {
            let s = c.state();
            s.num("menu") == 0 && s.num("tooltip") == 0
        },
    );
    assert_eq!(c.send("quit"), "ok");
    assert!(tw.wait_exit(Duration::from_secs(8)).success());
}

/// `park` (WP-57): the box goes to its frame-0 position and stays there until `animate`.
#[test]
fn park_holds_the_box_at_its_start_position() {
    let tw = Testwin::spawn(&["--self-test-secs", "30", "--no-focus", "WP-57 park"]);
    let mut c = tw.client();
    eventually("first frames painted", Duration::from_secs(5), || {
        Reply::new(c.send("frame?")).num("frame") > 30
    });
    let layout_reply = Reply::new(c.send("layout?"));
    let dpi = layout_reply.num("dpi");
    let (cw, ch) = layout_reply.get("client").split_once('x').unwrap();
    let layout = Layout::new(cw.parse().unwrap(), ch.parse().unwrap(), dpi);
    let home = layout.box_rect(0).csv();
    assert_ne!(
        c.state().get("box"),
        home,
        "the box has moved away from its start before the park"
    );
    assert_eq!(c.send("park"), "ok");
    assert_eq!(c.state().get("box"), home);
    std::thread::sleep(Duration::from_millis(300));
    let parked = c.state();
    assert_eq!(parked.get("box"), home, "the box stays parked");
    assert_eq!(parked.num("frame"), 0, "every paint shows frame 0");
    assert_eq!(c.send("animate"), "ok");
    eventually("the box moves again", Duration::from_secs(5), || {
        c.state().get("box") != home
    });
}
