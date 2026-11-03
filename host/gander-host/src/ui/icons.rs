// SPDX-License-Identifier: Apache-2.0
//! The Gander lens as pixels: the four tray icons and the window icon.
//!
//! The PNGs under `host/gander-host/assets/` are generated from the SVGs in `brand/` by
//! `cargo xtask icons` and committed, so the build needs no rasteriser. They are embedded here and
//! decoded on demand (a 32 px icon is 4 KiB of pixels).
//!
//! The tray asks Windows for its small-icon size (16 px at 100 % scale, 20 at 125 %, 24 at 150 %,
//! 32 at 200 %) and gets the matching drawing, so the lens is never scaled by the shell.

use super::tray_state::TrayState;

/// The tray icon edges shipped for every state, ascending.
pub const TRAY_SIZES: [u32; 4] = [16, 20, 24, 32];

macro_rules! tray_png {
    ($state:literal, $size:literal) => {
        include_bytes!(concat!(
            "../../assets/tray/gander-tray-",
            $state,
            "-",
            $size,
            ".png"
        ))
    };
}

/// The window icon (the app icon at 64 px).
const WINDOW_PNG: &[u8] = include_bytes!("../../assets/gander-window-64.png");

/// The embedded PNG for a tray state and one of [`TRAY_SIZES`].
fn tray_png(state: TrayState, size: u32) -> &'static [u8] {
    match (state, size) {
        (TrayState::Idle, 16) => tray_png!("idle", "16"),
        (TrayState::Idle, 20) => tray_png!("idle", "20"),
        (TrayState::Idle, 24) => tray_png!("idle", "24"),
        (TrayState::Idle, _) => tray_png!("idle", "32"),
        (TrayState::Connected, 16) => tray_png!("connected", "16"),
        (TrayState::Connected, 20) => tray_png!("connected", "20"),
        (TrayState::Connected, 24) => tray_png!("connected", "24"),
        (TrayState::Connected, _) => tray_png!("connected", "32"),
        (TrayState::Streaming, 16) => tray_png!("streaming", "16"),
        (TrayState::Streaming, 20) => tray_png!("streaming", "20"),
        (TrayState::Streaming, 24) => tray_png!("streaming", "24"),
        (TrayState::Streaming, _) => tray_png!("streaming", "32"),
        (TrayState::Paused, 16) => tray_png!("paused", "16"),
        (TrayState::Paused, 20) => tray_png!("paused", "20"),
        (TrayState::Paused, 24) => tray_png!("paused", "24"),
        (TrayState::Paused, _) => tray_png!("paused", "32"),
    }
}

/// The shipped size to use for a system small-icon edge (`SM_CXSMICON`): the smallest one that
/// is at least as large (the shell then scales down a little rather than up), else the largest.
pub fn tray_size_for(system_small_icon: u32) -> u32 {
    TRAY_SIZES
        .iter()
        .copied()
        .find(|s| *s >= system_small_icon)
        .unwrap_or(TRAY_SIZES[TRAY_SIZES.len() - 1])
}

/// The size the notification area wants right now (Windows asks per DPI); 32 elsewhere.
#[cfg(windows)]
pub fn tray_size() -> u32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};
    // SAFETY: `GetSystemMetrics` has no preconditions; it only reads a system setting.
    let metric = unsafe { GetSystemMetrics(SM_CXSMICON) };
    tray_size_for(u32::try_from(metric).unwrap_or(0))
}

/// The size the notification area wants right now (Windows asks per DPI); 32 elsewhere.
#[cfg(not(windows))]
pub fn tray_size() -> u32 {
    32
}

/// Straight RGBA8 pixels and the edge of a square image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixels {
    /// RGBA8, row by row.
    pub rgba: Vec<u8>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Decodes one of the embedded PNGs (RGBA8, written by `cargo xtask icons`). `None` only if the
/// asset is corrupt, which the tests below rule out.
fn decode(png: &[u8]) -> Option<Pixels> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .ok()?;
    let mut buffer = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buffer).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buffer.truncate(info.buffer_size());
    Some(Pixels {
        rgba: buffer,
        width: info.width,
        height: info.height,
    })
}

/// The tray icon for `state` at `size` (one of [`TRAY_SIZES`]; another value gets the nearest
/// larger shipped size).
pub fn tray_icon(state: TrayState, size: u32) -> Pixels {
    decode(tray_png(state, tray_size_for(size))).unwrap_or_else(|| blank(size))
}

/// The icon of the Gander, Add a device and notification windows.
pub fn window_icon() -> Pixels {
    decode(WINDOW_PNG).unwrap_or_else(|| blank(64))
}

fn blank(size: u32) -> Pixels {
    Pixels {
        rgba: vec![0; (size * size * 4) as usize],
        width: size,
        height: size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_has_every_size_and_each_is_a_real_drawing() {
        for state in TrayState::ALL {
            for size in TRAY_SIZES {
                let icon = decode(tray_png(state, size))
                    .unwrap_or_else(|| panic!("{state:?} {size}: not an RGBA8 PNG"));
                assert_eq!((icon.width, icon.height), (size, size), "{state:?}");
                assert_eq!(icon.rgba.len(), (size * size * 4) as usize);
                let opaque = icon.rgba.chunks_exact(4).filter(|p| p[3] == 255).count();
                assert!(opaque > 20, "{state:?} {size}: only {opaque} opaque pixels");
                // The corner stays transparent: the lens does not fill its square.
                assert_eq!(
                    icon.rgba[3], 0,
                    "{state:?} {size}: corner is not transparent"
                );
            }
        }
    }

    #[test]
    fn the_states_differ_at_every_size() {
        for size in TRAY_SIZES {
            for (i, a) in TrayState::ALL.iter().enumerate() {
                for b in &TrayState::ALL[i + 1..] {
                    assert_ne!(
                        tray_icon(*a, size),
                        tray_icon(*b, size),
                        "{a:?} and {b:?} at {size}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_window_icon_is_a_64_pixel_square_with_a_tile() {
        let icon = window_icon();
        assert_eq!((icon.width, icon.height), (64, 64));
        // The app icon is a rounded tile: opaque in the middle, rounded off at the corner.
        let at = |x: usize, y: usize| icon.rgba[(y * 64 + x) * 4 + 3];
        assert_eq!(at(32, 4), 255);
        assert_eq!(at(0, 0), 0);
    }

    #[test]
    fn the_shipped_size_follows_the_system_icon_size() {
        assert_eq!(tray_size_for(0), 16);
        assert_eq!(tray_size_for(16), 16);
        assert_eq!(tray_size_for(17), 20);
        assert_eq!(tray_size_for(20), 20);
        assert_eq!(tray_size_for(24), 24);
        assert_eq!(tray_size_for(30), 32);
        assert_eq!(tray_size_for(48), 32);
    }

    #[test]
    fn an_unshipped_size_gets_the_next_larger_drawing() {
        assert_eq!(tray_icon(TrayState::Idle, 18).width, 20);
    }
}
