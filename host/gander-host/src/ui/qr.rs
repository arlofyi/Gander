// SPDX-License-Identifier: Apache-2.0
//! Rendering the pairing QR code.
//!
//! The matrix is built from [`QrPayload::to_uri`], which carries the window secret: it is passed
//! straight to the encoder and never stored, logged or shown as text. [`QrMatrix`] holds only
//! dark/light modules and redacts itself in `Debug` (the modules ARE the secret).

use std::fmt;

use gander_core::pairing::qr::QrPayload;
use qrcode::{Color, EcLevel, QrCode};

/// Quiet zone around the code, in modules (the QR standard asks for 4).
pub const QUIET_ZONE: usize = 4;

/// A rendered QR code: `width` x `width` modules, row-major, `true` = dark.
#[derive(Clone, PartialEq, Eq)]
pub struct QrMatrix {
    width: usize,
    dark: Vec<bool>,
}

impl fmt::Debug for QrMatrix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "QrMatrix({0}x{0}, <redacted>)", self.width)
    }
}

impl QrMatrix {
    /// Encodes the payload's URI (error correction level M). `None` if it does not fit a QR code,
    /// which cannot happen for a real payload.
    pub fn for_payload(payload: &QrPayload) -> Option<Self> {
        Self::encode(payload.to_uri().as_bytes())
    }

    fn encode(data: &[u8]) -> Option<Self> {
        let code = QrCode::with_error_correction_level(data, EcLevel::M).ok()?;
        Some(Self {
            width: code.width(),
            dark: code
                .to_colors()
                .into_iter()
                .map(|c| c == Color::Dark)
                .collect(),
        })
    }

    /// Modules per side, without the quiet zone.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Whether the module at (`x`, `y`) is dark; false outside the code (the quiet zone).
    pub fn is_dark(&self, x: usize, y: usize) -> bool {
        x < self.width && y < self.width && self.dark[y * self.width + x]
    }

    /// Pixels per side when drawn with `scale` pixels per module and the quiet zone.
    pub fn image_side(&self, scale: usize) -> usize {
        (self.width + 2 * QUIET_ZONE) * scale
    }

    /// The code as an egui image: black modules on white, `scale` pixels per module, with a quiet
    /// zone. White background on purpose, whatever the theme: phone cameras need the contrast.
    pub fn to_image(&self, scale: usize) -> egui::ColorImage {
        let side = self.image_side(scale);
        let mut pixels = vec![egui::Color32::WHITE; side * side];
        for y in 0..self.width {
            for x in 0..self.width {
                if !self.is_dark(x, y) {
                    continue;
                }
                for dy in 0..scale {
                    let row = (y + QUIET_ZONE) * scale + dy;
                    let start = row * side + (x + QUIET_ZONE) * scale;
                    pixels[start..start + scale].fill(egui::Color32::BLACK);
                }
            }
        }
        egui::ColorImage::new([side, side], pixels)
    }
}

#[cfg(test)]
mod tests {
    use gander_core::protocol::{HostId, PairingSecret};

    use super::*;

    fn payload() -> QrPayload {
        QrPayload {
            host_id: HostId::from("host_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG"),
            addresses: vec!["100.101.102.103".parse().unwrap()],
            port: 42637,
            host_name: "studio-pc".into(),
            secret: PairingSecret::new("s3cr3t-s3cr3t-s3cr3t-s3cr3t-s3cr3t-s3cr3t"),
            magic_dns: Some("studio-pc.tail1234.ts.net".into()),
        }
    }

    #[test]
    fn a_payload_renders_to_a_square_matrix_with_finder_patterns() {
        let m = QrMatrix::for_payload(&payload()).unwrap();
        assert!(m.width() >= 21, "version 1 is 21 modules");
        let w = m.width();
        // The three finder patterns have a dark 7x7 outline in their corners.
        for (cx, cy) in [(0, 0), (w - 7, 0), (0, w - 7)] {
            for i in 0..7 {
                assert!(m.is_dark(cx + i, cy), "top edge");
                assert!(m.is_dark(cx + i, cy + 6), "bottom edge");
                assert!(m.is_dark(cx, cy + i), "left edge");
                assert!(m.is_dark(cx + 6, cy + i), "right edge");
            }
        }
        assert!(!m.is_dark(w, 0), "outside is the quiet zone");
    }

    #[test]
    fn the_image_has_a_white_quiet_zone_and_the_requested_scale() {
        let m = QrMatrix::for_payload(&payload()).unwrap();
        let img = m.to_image(3);
        let side = m.image_side(3);
        assert_eq!(img.size, [side, side]);
        let px = |x: usize, y: usize| img.pixels[y * side + x];
        assert_eq!(px(0, 0), egui::Color32::WHITE);
        assert_eq!(
            px(QUIET_ZONE * 3 - 1, QUIET_ZONE * 3 - 1),
            egui::Color32::WHITE
        );
        // The top-left module of the first finder pattern is dark and 3x3 pixels wide.
        let o = QUIET_ZONE * 3;
        assert_eq!(px(o, o), egui::Color32::BLACK);
        assert_eq!(px(o + 2, o + 2), egui::Color32::BLACK);
    }

    #[test]
    fn debug_never_shows_modules_and_the_uri_is_not_kept() {
        let m = QrMatrix::for_payload(&payload()).unwrap();
        let shown = format!("{m:?}");
        assert!(shown.contains("redacted"), "{shown}");
        assert!(!shown.contains("true"));
        assert!(!shown.contains("s3cr3t"));
    }

    #[test]
    fn different_secrets_give_different_codes() {
        let a = QrMatrix::for_payload(&payload()).unwrap();
        let mut other = payload();
        other.secret = PairingSecret::new("another-secret-another-secret-another-secret");
        let b = QrMatrix::for_payload(&other).unwrap();
        assert_ne!(a, b);
    }
}
