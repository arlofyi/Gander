// SPDX-License-Identifier: Apache-2.0
//! Encoder set point from the bandwidth estimate, with the battery cap.

/// How the estimate becomes the encoder's bitrate.
#[derive(Debug, Clone, Copy)]
pub struct RatePolicy {
    /// Set point = estimate x headroom. The webrtc-rs pacer sends at exactly the estimate, so an
    /// encoder at the estimate builds a sender queue (WP-43: 0.85).
    pub headroom: f64,
    /// Floor, bit/s.
    pub min_bps: u32,
    /// Ceiling, bit/s.
    pub max_bps: u32,
    /// Ceiling while the host runs on battery, bit/s.
    pub battery_cap_bps: u32,
}

impl Default for RatePolicy {
    fn default() -> Self {
        Self {
            headroom: 0.85,
            min_bps: 250_000,
            max_bps: 8_000_000,
            battery_cap_bps: 2_500_000,
        }
    }
}

impl RatePolicy {
    /// The ceiling now: the policy maximum, the phone's `max_kbps` and the battery cap.
    pub fn ceiling(&self, on_battery: bool, client_max_bps: Option<u32>) -> u32 {
        let mut c = self.max_bps;
        if let Some(m) = client_max_bps {
            c = c.min(m);
        }
        if on_battery {
            c = c.min(self.battery_cap_bps);
        }
        c.max(self.min_bps)
    }

    /// The set point for `estimate_bps`.
    pub fn setpoint(
        &self,
        estimate_bps: f64,
        on_battery: bool,
        client_max_bps: Option<u32>,
    ) -> u32 {
        let ceiling = self.ceiling(on_battery, client_max_bps);
        let want = estimate_bps * self.headroom;
        if want.is_nan() {
            return self.min_bps;
        }
        (want as u64).clamp(u64::from(self.min_bps), u64::from(ceiling)) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headroom_floor_ceiling_and_battery() {
        let p = RatePolicy::default();
        assert_eq!(p.setpoint(4_000_000.0, false, None), 3_400_000);
        assert_eq!(p.setpoint(20_000_000.0, false, None), 8_000_000);
        assert_eq!(p.setpoint(10_000.0, false, None), 250_000);
        assert_eq!(p.setpoint(8_000_000.0, true, None), 2_500_000);
        assert_eq!(p.setpoint(8_000_000.0, false, Some(1_000_000)), 1_000_000);
        assert_eq!(p.setpoint(f64::NAN, false, None), 250_000);
    }
}
