// SPDX-License-Identifier: Apache-2.0
//! The tray application: a tray-only phase on a plain Win32 message loop, and eframe sessions that
//! exist only while a window is showing.
//!
//! # Two phases (host footprint, 2026-11-02)
//!
//! - **Tray only** (most of the time): the tray icon lives on the main thread, whose messages are
//!   pumped by [`super::idle`] (no winit, no OpenGL, no egui). The host is polled once a second
//!   for the tray state and the toast triggers. Nothing is drawn.
//! - **Session**: when a window or a toast must show, `eframe::run_native` runs with
//!   `run_and_return`, so it returns once the session's root closes. eframe keeps its winit event
//!   loop in a thread local and reuses it, which is how it supports opening and closing windows
//!   more than once in a process (winit 0.30 itself cannot create a second event loop). The GL
//!   context, the egui context, fonts, textures and AccessKit go away with the session. The
//!   session ends by itself once no window and no toast is left.
//!
//! [`GanderApp`] is the state that outlives sessions (tray, polled data, toasts, page drafts);
//! [`Session`] borrows it for one `run_native`.
//!
//! # Why a session's root window is a 1x1 off-screen window
//!
//! Hiding the root is not an option on Windows: measured on 2026-11-02 (eframe 0.33.3, winit
//! 0.30.13, Windows 11 26200), after `ViewportCommand::Visible(false)` `update()` is never called
//! again, not even by `request_repaint_after` or a repaint from another thread. The root of a
//! session is therefore a transparent, borderless, click-through 1x1 window parked off screen,
//! without a taskbar button; every real window (Gander, Add a device, the toast) is an immediate
//! child viewport that exists only while it is open. Closing the last one closes the root, which
//! ends the session.
//!
//! # Windows
//!
//! - **Gander**: the settings window with a sidebar (Shares, Devices, Activity, Network, Settings).
//! - **Add a device**: its own window, because the pairing window exists only while it is open:
//!   opening it opens the pairing window, closing it (or the kill switch) closes it.
//! - **Toast**: a small always-on-top window in the corner for what needs a button (pairing
//!   requests, Stream prompts). Plain notices ("Pixel 9 connected", "is viewing", pause, kill
//!   switch) go to the native notification of the tray icon ([`Tray::notify`]) and start no
//!   session; the egui toast is only the fallback when the shell refuses.
//! - **Tray flyout**: left click on the tray icon (mockup 37). `tray-icon` only offers a native
//!   menu, so the flyout is a small borderless always-on-top egui window above the notification
//!   area; it closes when it loses focus.
//! - **Viewing pill**: while a device streams, an always-on-top orange pill at the top centre of
//!   the primary screen with Stop (design doc 9.3). It is its own window, so the stream (one HWND)
//!   never captures it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use egui::{ViewportBuilder, ViewportCommand, ViewportId};

use super::controller::{FolderPicker, HostController};
use super::model::{Page, UiData};
use super::pages::{self, Cx, Outbox};
use super::smoke::{SmokeDriver, Step};
use super::theme;
use super::toast::{NewToast, ToastAction, ToastKind, ToastQueue, Watcher, prompt_toast};
use super::tray::{Command, PAUSE_MINUTES, Tray};
use super::tray_state::TrayState;
use super::windows::{self, PageStates};

/// Poll interval while a window is open.
pub const POLL_OPEN: Duration = Duration::from_millis(500);
/// Poll interval while only the tray is showing.
pub const POLL_IDLE: Duration = Duration::from_millis(1000);

const TOAST_WIDTH: f32 = 364.0;

/// Set by [`request_open_window`], taken by the next tick of either phase.
static OPEN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Wakes whichever loop is running (a session's egui context, or the tray-only message loop), so
/// a request from another thread is seen at once and not at the next poll.
type Waker = Box<dyn Fn() + Send + Sync>;
static WAKER: OnceLock<Waker> = OnceLock::new();

/// Asks the application to open (or focus) the Gander window. Safe to call from any thread, and
/// before the application exists: `gander-host --open` calls it before [`super::run`], and
/// `single_instance` calls it when a second start of the host asks the running one to show itself
/// (the Start menu entry).
pub fn request_open_window() {
    OPEN_REQUESTED.store(true, Ordering::SeqCst);
    if let Some(wake) = WAKER.get() {
        wake();
    }
}

/// Wakes the tray-only message loop of the thread that owns the tray (`WM_NULL` makes its wait
/// return).
#[cfg(windows)]
fn post_wake(thread_id: u32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_NULL};
    // SAFETY: posting a message to a thread id has no preconditions; if the thread is gone the
    // call just fails.
    unsafe { PostThreadMessageW(thread_id, WM_NULL, 0, 0) };
}

fn main_id() -> ViewportId {
    ViewportId::from_hash_of("gander-main")
}

fn add_device_id() -> ViewportId {
    ViewportId::from_hash_of("gander-add-device")
}

fn flyout_id() -> ViewportId {
    ViewportId::from_hash_of("gander-flyout")
}

fn pill_id() -> ViewportId {
    ViewportId::from_hash_of("gander-viewing-pill")
}

fn toast_id() -> ViewportId {
    ViewportId::from_hash_of("gander-toast")
}

/// The root viewport of a session: see the module docs.
pub fn root_viewport() -> ViewportBuilder {
    ViewportBuilder::default()
        .with_title("Gander (background)")
        .with_inner_size([1.0, 1.0])
        .with_position([-32000.0, -32000.0])
        .with_decorations(false)
        .with_resizable(false)
        .with_taskbar(false)
        .with_transparent(true)
        .with_mouse_passthrough(true)
        .with_active(false)
}

/// The running session's egui context, so a tray click can wake it (`None` between sessions,
/// when the tray-only loop handles the click itself).
type SessionSlot = Arc<Mutex<Option<egui::Context>>>;

/// The application state that outlives sessions. See the module docs.
#[allow(missing_debug_implementations)] // holds trait objects and the OS tray icon
pub struct GanderApp {
    ctrl: Arc<dyn HostController>,
    picker: Arc<dyn FolderPicker>,
    tray: Option<Tray>,
    tray_error: Option<String>,
    commands: Receiver<Command>,
    data: UiData,
    last_poll: Instant,
    main_open: bool,
    add_open: bool,
    page: Page,
    pages: PageStates,
    outbox: Outbox,
    toasts: ToastQueue,
    watcher: Watcher,
    quit: bool,
    root_updates: u64,
    frames: HashMap<String, u32>,
    smoke: Option<SmokeDriver>,
    session: SessionSlot,
    sessions: u32,
    flyout_open: bool,
    /// The flyout has had focus since it opened (it closes when it loses it).
    flyout_focused: bool,
    /// The stream on screen (device, file) and when it was first seen, for the elapsed time.
    stream_since: Option<((String, String), Instant)>,
    /// A session failed while only the pill needed it: do not retry until the stream changes.
    pill_suppressed: bool,
    /// The theme choice applied to the running session.
    theme: theme::ThemeChoice,
}

impl GanderApp {
    /// Builds the application on the main thread, where the tray icon must be created and its
    /// messages pumped. No window, GL context or egui context exists until [`Session::new`].
    pub fn new(
        ctrl: Arc<dyn HostController>,
        picker: Arc<dyn FolderPicker>,
        smoke: Option<SmokeDriver>,
    ) -> Self {
        let (tx, commands): (Sender<Command>, Receiver<Command>) = channel();
        let session: SessionSlot = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&session);
        let wake = move || {
            if let Some(ctx) = slot.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                ctx.request_repaint();
            }
        };
        // A request from another thread (the Start menu entry) wakes whichever loop runs.
        let slot = Arc::clone(&session);
        #[cfg(windows)]
        // SAFETY: `GetCurrentThreadId` has no preconditions.
        let main_thread = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
        let _ = WAKER.set(Box::new(move || {
            if let Some(ctx) = slot.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                ctx.request_repaint();
            }
            #[cfg(windows)]
            post_wake(main_thread);
        }));
        let (tray, tray_error) = match Tray::new(tx, wake) {
            Ok(t) => (Some(t), None),
            Err(e) => {
                // The windows still work from the command line; say so rather than hide it.
                eprintln!("tray icon unavailable: {e}");
                (None, Some(e))
            }
        };
        if let (Some(s), Some(e)) = (&smoke, &tray_error) {
            s.note_tray_unavailable(e);
        }
        let data = UiData::fetch_with(&*ctrl, false);
        let mut app = Self {
            ctrl,
            picker,
            tray,
            tray_error,
            commands,
            data,
            last_poll: Instant::now(),
            main_open: false,
            add_open: false,
            page: Page::Shares,
            pages: PageStates::default(),
            outbox: Outbox::default(),
            toasts: ToastQueue::new(),
            watcher: Watcher::new(),
            quit: false,
            root_updates: 0,
            frames: HashMap::new(),
            smoke,
            session,
            sessions: 0,
            flyout_open: false,
            flyout_focused: false,
            stream_since: None,
            pill_suppressed: false,
            theme: theme::ThemeChoice::System,
        };
        // Baseline for the toast watcher, and the first tray state.
        app.apply_poll(false);
        app
    }

    /// Why the tray icon could not be created, if it could not.
    pub fn tray_error(&self) -> Option<&str> {
        self.tray_error.as_deref()
    }

    fn windows_open(&self) -> bool {
        self.main_open || self.add_open
    }

    fn pill_shown(&self) -> bool {
        self.data.status.stream.is_some() && !self.pill_suppressed
    }

    /// Whether a session must run: a window, the flyout, a toast or the viewing pill is showing.
    /// `[ui] gpu`, for the next window session.
    pub fn ui_gpu(&self) -> gander_core::config::UiGpu {
        self.ctrl.ui_gpu()
    }

    pub fn needs_session(&self) -> bool {
        self.windows_open() || !self.toasts.is_empty() || self.flyout_open || self.pill_shown()
    }

    /// Whether the owner picked Quit.
    pub fn quitting(&self) -> bool {
        self.quit
    }

    /// One step of the tray-only phase: tray commands, the poll when due, the smoke driver.
    pub fn idle_tick(&mut self) {
        self.drain_commands(None);
        if self.poll_due() {
            self.poll(self.windows_open());
        }
        self.apply_outbox(None);
        self.smoke_step(None);
    }

    /// A session ended (`error`: it could not start or it failed). A failed session leaves
    /// nothing open behind it: the pairing window closes with its window and the toasts are
    /// dropped, so the tray-only loop does not retry at once; the next tray click tries again.
    pub fn session_ended(&mut self, error: Option<String>) {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if let Some(e) = error {
            eprintln!("the window could not be shown ({e}); the tray keeps running");
            if let Some(d) = &mut self.smoke {
                d.fail(format!("a window session failed: {e}"));
            }
            self.close_add_device();
            self.main_open = false;
            self.flyout_open = false;
            self.pill_suppressed = true;
            let ids: Vec<u64> = self.toasts.items().iter().map(|t| t.id).collect();
            for id in ids {
                self.toasts.dismiss(id);
            }
        }
    }

    /// The owner quit: pairing must not stay open behind a window that is gone.
    pub fn shutdown(&mut self) {
        if self.add_open {
            self.close_add_device();
        }
        if let Some(d) = &self.smoke {
            d.finish(self.root_updates, self.sessions, &self.frames);
        }
    }

    /// How long the tray-only loop may wait for a message before the next [`Self::idle_tick`].
    pub fn idle_wait(&self) -> Duration {
        let mut wait = POLL_IDLE.saturating_sub(self.last_poll.elapsed());
        if self.smoke.is_some() {
            wait = wait.min(Duration::from_millis(50));
        }
        wait
    }

    fn touch(&mut self, key: &str) {
        *self.frames.entry(key.to_owned()).or_insert(0) += 1;
    }

    // ---- polling ----

    fn poll(&mut self, full: bool) {
        if full {
            self.data = UiData::fetch_with(&*self.ctrl, self.add_open);
        } else {
            self.data.refresh_light(&*self.ctrl);
        }
        self.last_poll = Instant::now();
        self.apply_poll(full);
    }

    fn apply_poll(&mut self, _full: bool) {
        let now = Instant::now();
        for toast in self.watcher.observe(&self.data.status, &self.data.pairing) {
            self.toast(toast);
        }
        // What the host side wants the owner to see ("Pixel 9 copied text from this computer").
        for notice in self.ctrl.take_notices() {
            self.notice(&notice.title, &notice.body);
        }
        // The elapsed time of the stream on screen, from this process's own clock.
        let key = self
            .data
            .status
            .stream
            .as_ref()
            .map(|s| (s.device_name.clone(), s.file_name.clone()));
        match (&key, &self.stream_since) {
            (Some(k), Some((seen, _))) if k == seen => {}
            (Some(k), _) => {
                self.stream_since = Some((k.clone(), now));
                self.pill_suppressed = false;
            }
            (None, _) => self.stream_since = None,
        }
        self.data.stream_elapsed = self.stream_since.as_ref().map(|(_, at)| at.elapsed());
        let pending = self
            .data
            .pairing
            .pending
            .as_ref()
            .map(|p| p.pairing_id.clone());
        self.toasts.prune(now, pending.as_ref());
        // A Stream prompt is shown until it is answered or times out on the host.
        let prompt = self.ctrl.stream_prompt();
        self.toasts.prune_prompts(prompt.as_ref().map(|p| p.id));
        if let Some(p) = &prompt
            && !self.toasts.has_prompt(p.id)
        {
            self.toasts.push(prompt_toast(p), now);
        }
        let state = TrayState::from_status(&self.data.status);
        if let Some(tray) = &mut self.tray
            && let Err(e) = tray.show(
                state,
                &state.tooltip(&self.data.status),
                self.data.status.paused_for.is_some(),
            )
        {
            eprintln!("tray icon update failed: {e}");
        }
    }

    fn poll_due(&self) -> bool {
        let every = if self.windows_open() {
            POLL_OPEN
        } else {
            POLL_IDLE
        };
        self.last_poll.elapsed() >= every
    }

    // ---- commands ----

    fn drain_commands(&mut self, ctx: Option<&egui::Context>) {
        if OPEN_REQUESTED.swap(false, Ordering::SeqCst) {
            self.open_main(ctx, None);
        }
        while let Ok(cmd) = self.commands.try_recv() {
            self.handle(ctx, cmd);
        }
    }

    fn handle(&mut self, ctx: Option<&egui::Context>, cmd: Command) {
        match cmd {
            Command::Open => {
                self.flyout_open = false;
                self.open_main(ctx, None);
            }
            Command::Flyout => {
                self.flyout_open = !self.flyout_open;
                self.flyout_focused = false;
                if self.flyout_open {
                    self.poll(true);
                }
            }
            Command::AddDevice => self.open_add_device(ctx),
            Command::TogglePause => {
                if self.data.status.paused_for.is_some() {
                    self.ctrl.resume_listener();
                    self.notice("Listener resumed", "Devices can connect again.");
                } else {
                    self.ctrl
                        .pause_listener(Duration::from_secs(PAUSE_MINUTES * 60));
                    self.notice(
                        "Listener paused",
                        &format!("No device can connect for {PAUSE_MINUTES} minutes. Every session was dropped."),
                    );
                }
                self.poll(self.windows_open());
            }
            Command::KillSwitch => {
                let now = time::OffsetDateTime::now_utc().to_offset(self.data.offset);
                let report = windows::KillReport::new(&self.data.status, now);
                self.ctrl.kill_switch();
                if self.add_open {
                    self.close_add_device();
                }
                self.notice(
                    &format!("Everyone was disconnected at {}", report.at),
                    "Every session was dropped and pairing is off. Paired devices can reconnect; revoke a device to lock it out.",
                );
                self.pages.kill = Some(report);
                self.poll(self.windows_open());
            }
            Command::Quit => self.quit = true,
        }
    }

    fn notice(&mut self, title: &str, body: &str) {
        self.toast(NewToast {
            kind: ToastKind::Notice,
            title: title.to_owned(),
            body: body.to_owned(),
            action: ToastAction::None,
        });
    }

    /// Queues a toast. Plain ones (no decision to make) go to the native notification of the
    /// tray icon, which needs no window; the egui toast is the fallback.
    fn toast(&mut self, toast: NewToast) {
        let plain = matches!(
            toast.kind,
            ToastKind::Notice | ToastKind::DeviceConnected | ToastKind::StreamStarted
        );
        if plain
            && let Some(tray) = &self.tray
            && tray.notify(&toast.title, &toast.body).is_ok()
        {
            return;
        }
        self.toasts.push(toast, Instant::now());
    }

    fn open_main(&mut self, ctx: Option<&egui::Context>, page: Option<Page>) {
        if let Some(p) = page {
            self.set_page(p);
        }
        if self.main_open {
            if let Some(ctx) = ctx {
                ctx.send_viewport_cmd_to(main_id(), ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd_to(main_id(), ViewportCommand::Focus);
            }
        } else {
            self.main_open = true;
            if self.page == Page::Network {
                self.ctrl.refresh_network();
            }
        }
        self.poll(true);
    }

    fn set_page(&mut self, page: Page) {
        if page == Page::Network && self.page != Page::Network {
            self.outbox.refresh_network = true;
        }
        self.page = page;
    }

    fn open_add_device(&mut self, ctx: Option<&egui::Context>) {
        if self.add_open {
            if let Some(ctx) = ctx {
                ctx.send_viewport_cmd_to(add_device_id(), ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd_to(add_device_id(), ViewportCommand::Focus);
            }
            return;
        }
        // The pairing window exists exactly while this window is open.
        self.pages.add_device = pages::add_device::State::default();
        // The payload is fetched again while the window is open; do not keep this copy.
        drop(self.ctrl.open_pairing_window());
        self.add_open = true;
        self.poll(true);
    }

    fn close_add_device(&mut self) {
        if self.add_open {
            self.ctrl.close_pairing_window();
            self.add_open = false;
            self.pages.add_device.clear_qr();
            self.poll(self.main_open);
        }
    }

    fn apply_outbox(&mut self, ctx: Option<&egui::Context>) {
        let out = std::mem::take(&mut self.outbox);
        if out.refresh_network {
            self.ctrl.refresh_network();
        }
        if out.close_add_device {
            self.close_add_device();
        }
        if out.open_add_device {
            self.open_add_device(ctx);
        }
        if let Some(choice) = out.theme {
            self.theme = choice;
            if let Some(ctx) = ctx {
                theme::set_choice(ctx, choice);
            }
        }
    }

    // ---- windows ----

    fn draw_main(&mut self, ctx: &egui::Context) {
        let builder = ViewportBuilder::default()
            .with_title("Gander")
            .with_inner_size([1040.0, 700.0])
            .with_min_inner_size([860.0, 540.0])
            .with_icon(window_icon());
        ctx.show_viewport_immediate(main_id(), builder, |ctx, _class| {
            if ctx.input(|i| i.viewport().close_requested()) {
                self.main_open = false;
                return;
            }
            let key = format!("page:{}", self.page.title().to_lowercase());
            self.touch(&key);
            let key = format!("main@{}", self.sessions);
            self.touch(&key);
            self.main_ui(ctx);
        });
    }

    fn main_ui(&mut self, ctx: &egui::Context) {
        let Self {
            data,
            ctrl,
            picker,
            outbox,
            pages: states,
            page,
            ..
        } = self;
        let mut cx = Cx {
            data,
            ctrl: &**ctrl,
            picker,
            out: outbox,
        };
        let out = windows::main_window(ctx, *page, states, &mut cx);
        if let Some(p) = out.next_page {
            self.set_page(p);
        }
        if out.kill {
            self.handle(Some(ctx), Command::KillSwitch);
        }
    }

    fn draw_add_device(&mut self, ctx: &egui::Context) {
        let builder = ViewportBuilder::default()
            .with_title("Add a device")
            .with_inner_size([720.0, 600.0])
            .with_min_inner_size([560.0, 480.0])
            .with_icon(window_icon());
        ctx.show_viewport_immediate(add_device_id(), builder, |ctx, _class| {
            if ctx.input(|i| i.viewport().close_requested()) {
                self.outbox.close_add_device = true;
                return;
            }
            self.touch("add-device");
            let Self {
                data,
                ctrl,
                picker,
                outbox,
                pages: states,
                ..
            } = self;
            let mut cx = Cx {
                data,
                ctrl: &**ctrl,
                picker,
                out: outbox,
            };
            windows::add_device_window(ctx, states, &mut cx);
        });
    }

    fn draw_toasts(&mut self, ctx: &egui::Context) {
        let n = self.toasts.items().len();
        let size = egui::vec2(TOAST_WIDTH, n as f32 * (windows::TOAST_HEIGHT + 8.0));
        let ppp = ctx.pixels_per_point();
        let pos = super::screen::bottom_right(super::screen::work_area(), ppp, size);
        let builder = ViewportBuilder::default()
            .with_title("Gander notification")
            .with_inner_size(size)
            .with_position(pos)
            .with_decorations(false)
            .with_resizable(false)
            .with_taskbar(false)
            .with_always_on_top()
            .with_active(false);
        let items = self.toasts.items().to_vec();
        let mut clicked: Vec<(u64, ToastAction)> = Vec::new();
        ctx.show_viewport_immediate(toast_id(), builder, |ctx, _class| {
            self.touch("toast");
            clicked = windows::toast_stack(ctx, &items);
        });
        for (id, action) in clicked {
            self.toasts.dismiss(id);
            match action {
                ToastAction::OpenAddDevice => self.open_add_device(Some(ctx)),
                ToastAction::OpenDevices => self.open_main(Some(ctx), Some(Page::Devices)),
                ToastAction::StopStream => {
                    self.ctrl.stop_stream();
                    self.poll(self.windows_open());
                }
                ToastAction::Answer(id, answer) => {
                    self.ctrl.answer_stream_prompt(id, answer);
                    self.poll(self.windows_open());
                }
                ToastAction::None => {}
            }
        }
    }

    fn draw_flyout(&mut self, ctx: &egui::Context) {
        let size = egui::vec2(
            windows::FLYOUT_WIDTH,
            windows::flyout_height(&self.data.status),
        );
        let ppp = ctx.pixels_per_point();
        let pos = super::screen::bottom_right(super::screen::work_area(), ppp, size);
        let builder = ViewportBuilder::default()
            .with_title("Gander")
            .with_inner_size(size)
            .with_position(pos)
            .with_decorations(false)
            .with_resizable(false)
            .with_taskbar(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_active(true);
        let mut out = windows::FlyoutOut::default();
        ctx.show_viewport_immediate(flyout_id(), builder, |ctx, _class| {
            self.touch("flyout");
            let focused = ctx.input(|i| i.viewport().focused);
            match focused {
                Some(true) => self.flyout_focused = true,
                // A flyout closes when the owner clicks elsewhere, like the system ones.
                Some(false) if self.flyout_focused => self.flyout_open = false,
                _ => {}
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Escape) || i.viewport().close_requested()) {
                self.flyout_open = false;
            }
            out = windows::tray_flyout(ctx, &self.data, PAUSE_MINUTES);
        });
        if out != windows::FlyoutOut::default() {
            self.flyout_open = false;
        }
        if let Some(p) = out.open {
            self.open_main(Some(ctx), Some(p));
        }
        if out.add_device {
            self.open_add_device(Some(ctx));
        }
        if out.toggle_pause {
            self.handle(Some(ctx), Command::TogglePause);
        }
        if out.kill {
            self.handle(Some(ctx), Command::KillSwitch);
        }
        if out.stop_stream {
            self.ctrl.stop_stream();
            self.poll(self.windows_open());
        }
        if out.quit {
            self.quit = true;
        }
    }

    fn draw_pill(&mut self, ctx: &egui::Context) {
        let Some(stream) = self.data.status.stream.clone() else {
            return;
        };
        let size = windows::PILL_SIZE;
        let ppp = ctx.pixels_per_point();
        let pos = super::screen::top_centre(super::screen::work_area(), ppp, size);
        let builder = ViewportBuilder::default()
            .with_title("Gander: being viewed")
            .with_inner_size(size)
            .with_position(pos)
            .with_decorations(false)
            .with_resizable(false)
            .with_taskbar(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_active(false);
        let elapsed = self.data.stream_elapsed;
        let mut stop = false;
        ctx.show_viewport_immediate(pill_id(), builder, |ctx, _class| {
            self.touch("pill");
            stop = windows::viewing_pill(ctx, &stream, elapsed);
        });
        if stop {
            self.ctrl.stop_stream();
            self.poll(self.windows_open());
        }
    }

    // ---- smoke ----

    fn smoke_step(&mut self, ctx: Option<&egui::Context>) {
        let Some(mut driver) = self.smoke.take() else {
            return;
        };
        let state_for = |s: TrayState| (s, s.tooltip(&self.data.status));
        let in_session = ctx.is_some();
        if let Some(step) = driver.poll(&self.frames, self.sessions, in_session, Instant::now()) {
            match step {
                Step::OpenPage(p) => self.open_main(ctx, Some(p)),
                Step::OpenAddDevice => self.open_add_device(ctx),
                Step::CloseAddDevice => self.close_add_device(),
                Step::Reopen => self.open_main(ctx, None),
                Step::CloseAll => {
                    self.main_open = false;
                    self.flyout_open = false;
                    self.close_add_device();
                    let ids: Vec<u64> = self.toasts.items().iter().map(|t| t.id).collect();
                    for id in ids {
                        self.toasts.dismiss(id);
                    }
                }
                Step::ShowToast => self.toasts.push(
                    NewToast {
                        kind: ToastKind::DeviceConnected,
                        title: "Pixel 9 connected".to_owned(),
                        body: "A paired device is now using this computer.".to_owned(),
                        action: ToastAction::OpenDevices,
                    },
                    Instant::now(),
                ),
                Step::OpenFlyout => {
                    self.flyout_open = true;
                    self.flyout_focused = false;
                }
                Step::NativeNotice => match &self.tray {
                    Some(tray) => driver.note_native_notice(tray.notify(
                        "Gander smoke test",
                        "This is what a plain Gander notification looks like.",
                    )),
                    None => driver.note_native_notice(Err("no tray icon".to_owned())),
                },
                Step::ClearToasts => {
                    let ids: Vec<u64> = self.toasts.items().iter().map(|t| t.id).collect();
                    for id in ids {
                        self.toasts.dismiss(id);
                    }
                }
                Step::TrayState(s) => {
                    let (state, tip) = state_for(s);
                    match &mut self.tray {
                        Some(tray) => {
                            if let Err(e) = tray.show(state, &tip, state == TrayState::Paused) {
                                driver.fail(format!("tray icon update failed: {e}"));
                            } else {
                                driver.note_tray_state(s);
                            }
                        }
                        None => driver.note_tray_state_skipped(s),
                    }
                }
                Step::Quit => self.quit = true,
            }
        }
        self.smoke = Some(driver);
    }

    /// How long until the next frame of a session is needed.
    fn next_wake(&self) -> Duration {
        let every = if self.windows_open() {
            POLL_OPEN
        } else {
            POLL_IDLE
        };
        let mut wake = every.saturating_sub(self.last_poll.elapsed());
        if let Some(at) = self.toasts.next_expiry() {
            wake = wake.min(at.saturating_duration_since(Instant::now()));
        }
        if self.smoke.is_some() {
            wake = wake.min(Duration::from_millis(50));
        }
        wake.max(Duration::from_millis(16))
    }
}

/// The icon of the Gander and Add a device windows, decoded once.
fn window_icon() -> Arc<egui::IconData> {
    static ICON: OnceLock<Arc<egui::IconData>> = OnceLock::new();
    Arc::clone(ICON.get_or_init(|| {
        let px = super::icons::window_icon();
        Arc::new(egui::IconData {
            rgba: px.rgba,
            width: px.width,
            height: px.height,
        })
    }))
}

/// One eframe session over the [`GanderApp`] state: see the module docs.
#[allow(missing_debug_implementations)] // borrows the application
pub struct Session<'a> {
    app: &'a mut GanderApp,
}

impl<'a> Session<'a> {
    /// Starts a session inside the eframe creator: themes the new context and lets the tray
    /// wake it.
    pub fn new(cc: &eframe::CreationContext<'_>, app: &'a mut GanderApp) -> Self {
        theme::install(&cc.egui_ctx);
        app.theme = app.data.settings.theme;
        theme::set_choice(&cc.egui_ctx, app.theme);
        *app.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(cc.egui_ctx.clone());
        app.sessions += 1;
        Self { app }
    }
}

impl eframe::App for Session<'_> {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let app = &mut *self.app;
        app.root_updates += 1;
        app.drain_commands(Some(ctx));
        if app.poll_due() {
            app.poll(app.windows_open());
        }
        if app.main_open {
            app.draw_main(ctx);
        }
        if app.add_open {
            app.draw_add_device(ctx);
        }
        if !app.toasts.is_empty() {
            app.draw_toasts(ctx);
        }
        app.data.stream_elapsed = app.stream_since.as_ref().map(|(_, at)| at.elapsed());
        if app.flyout_open {
            app.draw_flyout(ctx);
        }
        if app.pill_shown() {
            app.draw_pill(ctx);
        }
        app.apply_outbox(Some(ctx));
        app.smoke_step(Some(ctx));
        // Quit, or nothing left to show: close the root, which ends this session (and with it the
        // GL context and every egui resource). The tray-only loop takes over.
        if app.quit || !app.needs_session() {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        ctx.request_repaint_after(app.next_wake());
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        *self.app.session.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_window_is_tiny_off_screen_and_not_in_the_taskbar() {
        let v = root_viewport();
        assert_eq!(v.inner_size, Some(egui::vec2(1.0, 1.0)));
        assert!(v.position.is_some_and(|p| p.x < -1000.0 && p.y < -1000.0));
        assert_eq!(v.taskbar, Some(false));
        assert_eq!(v.decorations, Some(false));
        assert_eq!(v.mouse_passthrough, Some(true));
        // It must never be hidden: a hidden root stops `update` on Windows.
        assert_ne!(v.visible, Some(false));
    }
}
