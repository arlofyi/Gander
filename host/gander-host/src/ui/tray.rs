// SPDX-License-Identifier: Apache-2.0
//! The tray icon, its menu (Open Gander, Pause listener 10 min, Disconnect everyone, Quit) and
//! native notifications.
//!
//! The tray is created on the thread that runs the event loop (the main thread) and its events are
//! forwarded as [`Command`]s over a channel to the application, which wakes up through
//! `egui::Context::request_repaint`. Left click opens the tray flyout (an egui window, because
//! `tray-icon` only offers a native menu), double click opens the Gander window, right click opens
//! the menu.
//!
//! [`Tray::notify`] shows a plain notification (no buttons) through the notification icon itself
//! (`Shell_NotifyIconW` with `NIF_INFO`; Windows 10 and 11 show it as a toast in the Action
//! Center), so "Pixel 9 connected" costs no window and no GL context.
//!
//! Menu ids and their mapping to commands are plain data ([`command_for_menu_id`]) and tested; the
//! Windows icon itself is exercised by `gander-host --ui-smoke`.

use std::sync::mpsc::Sender;

use super::tray_state::TrayState;

/// How long "Pause listener" pauses.
pub const PAUSE_MINUTES: u64 = 10;

/// What the tray asks the application to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Open (or focus) the settings window.
    Open,
    /// Show or hide the tray flyout (left click).
    Flyout,
    /// Open the Add device window.
    AddDevice,
    /// Pause the listener for [`PAUSE_MINUTES`], or resume if it is paused.
    TogglePause,
    /// Drop every session now.
    KillSwitch,
    /// Stop Gander.
    Quit,
}

/// Menu item ids.
pub mod id {
    /// Open Gander.
    pub const OPEN: &str = "gander.open";
    /// Pause or resume the listener.
    pub const PAUSE: &str = "gander.pause";
    /// Kill switch.
    pub const KILL: &str = "gander.kill";
    /// Quit.
    pub const QUIT: &str = "gander.quit";
}

/// The command for a menu item id.
pub fn command_for_menu_id(menu_id: &str) -> Option<Command> {
    match menu_id {
        id::OPEN => Some(Command::Open),
        id::PAUSE => Some(Command::TogglePause),
        id::KILL => Some(Command::KillSwitch),
        id::QUIT => Some(Command::Quit),
        _ => None,
    }
}

/// The pause item's text for the current state.
pub fn pause_label(paused: bool) -> String {
    if paused {
        "Resume listener".to_owned()
    } else {
        format!("Pause listener for {PAUSE_MINUTES} minutes")
    }
}

#[cfg(windows)]
pub use imp::Tray;

#[cfg(not(windows))]
pub use stub::Tray;

#[cfg(windows)]
mod imp {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    use super::{Command, Sender, TrayState, command_for_menu_id, id, pause_label};

    /// The tray icon. Not `Send`: keep it on the thread that made it.
    pub struct Tray {
        icon: TrayIcon,
        pause_item: MenuItem,
        shown: Option<(TrayState, String, bool)>,
        /// The icon edge in use; the notification area asks for another one when the DPI changes.
        icon_size: u32,
    }

    impl std::fmt::Debug for Tray {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Tray")
                .field("shown", &self.shown)
                .finish_non_exhaustive()
        }
    }

    fn icon_for(state: TrayState) -> Result<Icon, String> {
        let px = state.icon();
        Icon::from_rgba(px.rgba, px.width, px.height).map_err(|e| e.to_string())
    }

    impl Tray {
        /// Creates the icon in the notification area with its menu. `commands` receives what the
        /// owner picks; `wake` is called after each send so the application runs its next frame.
        ///
        /// The tray event handlers can be installed once per process; a second call reports an
        /// error rather than silently losing events.
        pub fn new(
            commands: Sender<Command>,
            wake: impl Fn() + Send + Sync + 'static,
        ) -> Result<Self, String> {
            let menu = Menu::new();
            let open = MenuItem::with_id(id::OPEN, "Open Gander", true, None);
            let pause = MenuItem::with_id(id::PAUSE, pause_label(false), true, None);
            let kill = MenuItem::with_id(id::KILL, "Disconnect everyone", true, None);
            let quit = MenuItem::with_id(id::QUIT, "Quit Gander", true, None);
            menu.append_items(&[
                &open,
                &PredefinedMenuItem::separator(),
                &pause,
                &kill,
                &PredefinedMenuItem::separator(),
                &quit,
            ])
            .map_err(|e| e.to_string())?;

            let icon = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_icon(icon_for(TrayState::Idle)?)
                .with_tooltip("Gander")
                .build()
                .map_err(|e| e.to_string())?;

            let wake = std::sync::Arc::new(wake);
            let tx = commands.clone();
            let w = std::sync::Arc::clone(&wake);
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                if let Some(cmd) = command_for_menu_id(event.id.as_ref()) {
                    let _ = tx.send(cmd);
                    w();
                }
            }));
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                let cmd = match event {
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } => Some(Command::Flyout),
                    TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } => Some(Command::Open),
                    _ => None,
                };
                if let Some(cmd) = cmd {
                    let _ = commands.send(cmd);
                    wake();
                }
            }));
            Ok(Self {
                icon,
                pause_item: pause,
                shown: None,
                icon_size: crate::ui::icons::tray_size(),
            })
        }

        /// Shows `state` with `tooltip`; `paused` sets the pause item's text. Only changes what
        /// differs from the last call.
        pub fn show(
            &mut self,
            state: TrayState,
            tooltip: &str,
            paused: bool,
        ) -> Result<(), String> {
            let key = (state, tooltip.to_owned(), paused);
            let size = crate::ui::icons::tray_size();
            if self.shown.as_ref() == Some(&key) && self.icon_size == size {
                return Ok(());
            }
            if self.shown.as_ref().map(|s| s.0) != Some(state) || self.icon_size != size {
                self.icon_size = size;
                self.icon
                    .set_icon(Some(icon_for(state)?))
                    .map_err(|e| e.to_string())?;
            }
            self.icon
                .set_tooltip(Some(tooltip))
                .map_err(|e| e.to_string())?;
            if self.shown.as_ref().map(|s| s.2) != Some(paused) {
                self.pause_item.set_text(pause_label(paused));
            }
            self.shown = Some(key);
            Ok(())
        }

        /// Shows a plain notification from the tray icon: `title` (up to 63 characters) and
        /// `body` (up to 255). Errors when the shell refuses it; the caller then falls back to
        /// its own window.
        pub fn notify(&self, title: &str, body: &str) -> Result<(), String> {
            use windows_sys::Win32::UI::Shell::{
                NIF_INFO, NIIF_RESPECT_QUIET_TIME, NIIF_USER, NIM_MODIFY, NOTIFYICONDATAW,
                Shell_NotifyIconW,
            };

            fn fill(dst: &mut [u16], text: &str) {
                let max = dst.len() - 1;
                for (d, s) in dst.iter_mut().zip(text.encode_utf16().take(max)) {
                    *d = s;
                }
            }

            let hwnd = self.icon.window_handle();
            // SAFETY: NOTIFYICONDATAW is plain data; all-zero is a valid starting value.
            let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uFlags = NIF_INFO;
            nid.dwInfoFlags = NIIF_USER | NIIF_RESPECT_QUIET_TIME;
            fill(&mut nid.szInfoTitle, title);
            fill(&mut nid.szInfo, body);
            // tray-icon registers its icon with an id from a per-process counter that starts at
            // 1 and does not expose it; Gander makes one icon per process, so the first ids are
            // tried in turn and the first one the shell accepts is the icon.
            for uid in 1..=8 {
                nid.uID = uid;
                // SAFETY: `nid` is a fully initialised NOTIFYICONDATAW with its size set; the
                // window handle belongs to the live tray icon on this thread.
                if unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) } != 0 {
                    return Ok(());
                }
            }
            Err("the shell did not accept the notification".to_owned())
        }
    }
}

#[cfg(not(windows))]
mod stub {
    use super::{Command, Sender, TrayState};

    /// There is no tray on this platform yet.
    #[derive(Debug)]
    pub struct Tray;

    impl Tray {
        /// Always an error: tray icons are only built for Windows.
        pub fn new(
            _commands: Sender<Command>,
            _wake: impl Fn() + Send + Sync + 'static,
        ) -> Result<Self, String> {
            Err("tray icons are only built for Windows".to_owned())
        }

        /// Never called (there is no `Tray` value).
        pub fn notify(&self, _title: &str, _body: &str) -> Result<(), String> {
            Err("no notifications on this platform".to_owned())
        }

        /// Never called (there is no `Tray` value).
        pub fn show(
            &mut self,
            _state: TrayState,
            _tooltip: &str,
            _paused: bool,
        ) -> Result<(), String> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_has_exactly_the_four_commands_of_the_spec() {
        assert_eq!(command_for_menu_id(id::OPEN), Some(Command::Open));
        assert_eq!(command_for_menu_id(id::PAUSE), Some(Command::TogglePause));
        assert_eq!(command_for_menu_id(id::KILL), Some(Command::KillSwitch));
        assert_eq!(command_for_menu_id(id::QUIT), Some(Command::Quit));
        assert_eq!(command_for_menu_id("something else"), None);
    }

    #[test]
    fn the_pause_item_flips_its_text() {
        assert_eq!(pause_label(false), "Pause listener for 10 minutes");
        assert_eq!(pause_label(true), "Resume listener");
    }
}
