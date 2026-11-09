//! Per-fader calibration: raw range plus a taper preset, turned into a
//! piecewise (raw -> percent) curve.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Taper {
    #[default]
    Linear,
    Audio,
    Straight,
    LinearReversed,
    AudioReversed,
}

/// One point on a curve: raw reading and the percentage it maps to.
pub type CurvePoint = (i32, i32);

// Inverse of the Bourns datasheet output-vs-travel curves
// (value-fraction % -> volume %), so physical travel maps roughly linearly.
const LINEAR_SHAPE: &[(f64, f64)] = &[
    (0., 0.),
    (1., 10.),
    (4., 20.),
    (10., 30.),
    (30., 40.),
    (50., 50.),
    (63., 60.),
    (78., 70.),
    (92., 80.),
    (98., 90.),
    (100., 100.),
];
const AUDIO_SHAPE: &[(f64, f64)] = &[
    (0., 0.),
    (1., 10.),
    (3., 20.),
    (6., 30.),
    (10., 40.),
    (15., 50.),
    (22., 60.),
    (38., 70.),
    (65., 80.),
    (90., 90.),
    (100., 100.),
];
const STRAIGHT_SHAPE: &[(f64, f64)] = &[(0., 0.), (100., 100.)];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Calibration {
    pub min: i32,
    pub max: i32,
    pub taper: Taper,
    /// Independent final direction toggle. Reversed tapers mirror the
    /// electrical compensation curve; this flips the mapped percentage.
    pub inverted: bool,
    /// Mute dead zone threshold in raw units. 0 means off.
    pub mute_raw: i32,
}

impl Default for Calibration {
    fn default() -> Self {
        Self {
            min: 4,
            max: 3215,
            taper: Taper::Linear,
            inverted: false,
            mute_raw: 0,
        }
    }
}

impl Calibration {
    pub fn reversed(&self) -> bool {
        matches!(self.taper, Taper::LinearReversed | Taper::AudioReversed)
    }

    pub fn build_curve(&self) -> Vec<CurvePoint> {
        let (mut lo, mut hi) = (self.min.min(self.max), self.min.max(self.max));
        if hi - lo < 2 {
            // Guard against a degenerate range.
            (lo, hi) = (0, 3250);
        }
        let shape = match self.taper {
            Taper::Audio | Taper::AudioReversed => AUDIO_SHAPE,
            Taper::Straight => STRAIGHT_SHAPE,
            _ => LINEAR_SHAPE,
        };
        let reversed = self.reversed();
        let mut out: Vec<CurvePoint> = (0..shape.len())
            .map(|i| {
                let (f, v) = if reversed {
                    shape[shape.len() - 1 - i]
                } else {
                    shape[i]
                };
                let raw_fraction = if reversed { 100.0 - f } else { f };
                let raw = lo as f64 + raw_fraction / 100.0 * (hi - lo) as f64;
                let pct = if reversed { 100.0 - v } else { v };
                (raw.round_ties_even() as i32, pct.round_ties_even() as i32)
            })
            .collect();
        // Keep raw values strictly increasing so interpolation stays defined.
        for i in 1..out.len() {
            if out[i].0 <= out[i - 1].0 {
                out[i].0 = out[i - 1].0 + 1;
            }
        }
        out
    }

    pub fn map_raw(&self, curve: &[CurvePoint], raw: i32) -> f64 {
        let mapped = eval(curve, raw);
        if self.inverted {
            100.0 - mapped
        } else {
            mapped
        }
    }

    pub fn raw_at_percentage(&self, curve: &[CurvePoint], percentage: f64) -> f64 {
        inv_eval(
            curve,
            if self.inverted {
                100.0 - percentage
            } else {
                percentage
            },
        )
    }

    fn upper_mute_threshold(&self) -> i32 {
        let (lo, hi) = (self.min.min(self.max), self.min.max(self.max));
        hi - (self.mute_raw - lo).max(0)
    }

    pub fn is_in_mute_zone(&self, raw: i32) -> bool {
        if self.mute_raw <= 0 {
            return false;
        }
        if self.inverted {
            raw > self.upper_mute_threshold()
        } else {
            raw < self.mute_raw
        }
    }

    pub fn is_outside_mute_zone(&self, raw: i32, exit_band: i32) -> bool {
        if self.mute_raw <= 0 {
            return true;
        }
        if self.inverted {
            raw < self.upper_mute_threshold() - exit_band
        } else {
            raw > self.mute_raw + exit_band
        }
    }
}

/// Piecewise-linear lookup that clamps to the end points.
pub fn eval(curve: &[CurvePoint], v: i32) -> f64 {
    let Some(&(first_v, first_p)) = curve.first() else {
        return 0.0;
    };
    if v <= first_v {
        return first_p as f64;
    }
    for pair in curve.windows(2) {
        let ((v0, p0), (v1, p1)) = (pair[0], pair[1]);
        if v <= v1 {
            return p0 as f64 + (v - v0) as f64 / (v1 - v0) as f64 * (p1 - p0) as f64;
        }
    }
    curve[curve.len() - 1].1 as f64
}

/// Inverse of [`eval`]: the raw value whose percentage is `pct`.
pub fn inv_eval(curve: &[CurvePoint], pct: f64) -> f64 {
    let Some(&(first_v, first_p)) = curve.first() else {
        return 0.0;
    };
    let last = curve[curve.len() - 1];
    let ascending = last.1 >= first_p;
    let at_or_before_first = if ascending {
        pct <= first_p as f64
    } else {
        pct >= first_p as f64
    };
    if at_or_before_first {
        return first_v as f64;
    }
    for pair in curve.windows(2) {
        let ((v0, p0), (v1, p1)) = (pair[0], pair[1]);
        let reached = if ascending {
            pct <= p1 as f64
        } else {
            pct >= p1 as f64
        };
        if reached {
            return if p1 == p0 {
                v1 as f64
            } else {
                v0 as f64 + (pct - p0 as f64) / (p1 - p0) as f64 * (v1 - v0) as f64
            };
        }
    }
    last.0 as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cal(taper: Taper) -> Calibration {
        Calibration {
            min: 0,
            max: 1000,
            taper,
            ..Default::default()
        }
    }

    #[test]
    fn straight_is_proportional_and_clamped() {
        let c = cal(Taper::Straight);
        let curve = c.build_curve();
        assert_eq!(c.map_raw(&curve, 500), 50.0);
        assert_eq!(c.map_raw(&curve, -50), 0.0);
        assert_eq!(c.map_raw(&curve, 5000), 100.0);
    }

    #[test]
    fn linear_taper_uses_datasheet_points() {
        let c = cal(Taper::Linear);
        let curve = c.build_curve();
        assert_eq!(c.map_raw(&curve, 100), 30.0);
        assert_eq!(c.map_raw(&curve, 780), 70.0);
    }

    #[test]
    fn reversed_taper_still_rises_low_to_high() {
        let c = cal(Taper::AudioReversed);
        let curve = c.build_curve();
        assert_eq!(curve[0].1, 0);
        assert_eq!(curve[curve.len() - 1].1, 100);
        assert!(
            curve
                .windows(2)
                .all(|p| p[0].0 < p[1].0 && p[0].1 <= p[1].1)
        );
    }

    #[test]
    fn inverted_flips_the_final_percentage() {
        let c = Calibration {
            inverted: true,
            ..cal(Taper::Straight)
        };
        let curve = c.build_curve();
        assert_eq!(c.map_raw(&curve, 0), 100.0);
        assert_eq!(c.map_raw(&curve, 1000), 0.0);
    }

    #[test]
    fn degenerate_range_falls_back_to_full_scale() {
        let c = Calibration {
            min: 7,
            max: 8,
            taper: Taper::Straight,
            ..Default::default()
        };
        assert_eq!(c.build_curve(), vec![(0, 0), (3250, 100)]);
    }

    #[test]
    fn inverse_round_trips() {
        let c = cal(Taper::Audio);
        let curve = c.build_curve();
        for pct in [0.0, 10.0, 33.0, 50.0, 87.0, 100.0] {
            let raw = c.raw_at_percentage(&curve, pct);
            assert!(
                (c.map_raw(&curve, raw.round() as i32) - pct).abs() < 1.5,
                "{pct}"
            );
        }
    }

    #[test]
    fn mute_zone_follows_the_zero_endpoint() {
        let normal = Calibration {
            mute_raw: 50,
            ..cal(Taper::Straight)
        };
        assert!(normal.is_in_mute_zone(49));
        assert!(!normal.is_in_mute_zone(50));
        assert!(!normal.is_outside_mute_zone(60, 15));
        assert!(normal.is_outside_mute_zone(66, 15));

        let inverted = Calibration {
            inverted: true,
            ..normal
        };
        assert!(inverted.is_in_mute_zone(951));
        assert!(!inverted.is_in_mute_zone(50));
        assert!(inverted.is_outside_mute_zone(934, 15));
    }

    #[test]
    fn mute_zone_disabled_at_zero() {
        let c = cal(Taper::Straight);
        assert!(!c.is_in_mute_zone(0));
        assert!(c.is_outside_mute_zone(0, 15));
    }
}
