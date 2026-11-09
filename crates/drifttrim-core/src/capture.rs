//! Judges a recorded calibration sweep.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Analysis {
    pub message: String,
    pub good: bool,
    pub suggested_mute_raw: Option<i32>,
    pub span: i32,
    pub coverage_percent: i32,
    pub prior_travel_percent: i32,
}

const BIN_COUNT: i64 = 12;

fn scaled(base: f64, full_scale: i32) -> i32 {
    (base * full_scale as f64 / 3250.0).round_ties_even() as i32
}

fn rejected(message: &str, span: i32) -> Analysis {
    Analysis {
        message: message.to_owned(),
        good: false,
        suggested_mute_raw: None,
        span,
        coverage_percent: 0,
        prior_travel_percent: 0,
    }
}

pub fn analyze(samples: &[i32], prior_span: i32, raw_full_scale: i32) -> Analysis {
    if samples.len() < 8 {
        return rejected("Too few samples — sweep the full travel more slowly", 0);
    }
    let min = *samples.iter().min().unwrap();
    let max = *samples.iter().max().unwrap();
    let span = max - min;
    if span < scaled(100.0, raw_full_scale).max(4) {
        return rejected(
            "Travel is too narrow — make sure both endpoints are reached",
            span,
        );
    }

    let mut bins = [false; BIN_COUNT as usize];
    for &sample in samples {
        let bin = ((sample - min) as i64 * BIN_COUNT / (span as i64 + 1)).clamp(0, BIN_COUNT - 1);
        bins[bin as usize] = true;
    }
    let filled = bins.iter().filter(|b| **b).count() as f64;
    let coverage = (filled * 100.0 / BIN_COUNT as f64).round_ties_even() as i32;
    let travel = if prior_span > 0 {
        ((span as f64 * 100.0 / prior_span as f64).round_ties_even() as i32).clamp(0, 999)
    } else {
        100
    };

    let tail = &samples[samples.len() - samples.len().min(8)..];
    let tail_min = *tail.iter().min().unwrap();
    let tail_max = *tail.iter().max().unwrap();
    let tail_spread = tail_max - tail_min;
    let tail_average = tail.iter().map(|&s| s as f64).sum::<f64>() / tail.len() as f64;
    let ended_low = tail_average <= min as f64 + span as f64 * 0.10;
    let stable_limit = (scaled(4.0, raw_full_scale).max(1) as f64).max(span as f64 * 0.01);
    let stable_endpoint = tail_spread as f64 <= stable_limit;
    let mute_cap = scaled(300.0, raw_full_scale).max(1);
    let suggested_mute_raw = (ended_low && stable_endpoint && min < mute_cap)
        .then(|| mute_cap.min(min + scaled(10.0, raw_full_scale).max(1).max(tail_spread * 3)));

    let good = travel >= 85 && coverage >= 67;
    let mut message = if travel < 85 {
        format!("Limited capture · {travel}% of prior travel — sweep both endpoints again")
    } else if coverage < 67 {
        format!("Sparse capture · {coverage}% coverage — move more slowly through the middle")
    } else {
        format!("Good capture · {travel}% travel · {coverage}% coverage")
    };
    if let Some(mute) = suggested_mute_raw {
        message.push_str(&format!(" · suggested mute below {mute}"));
    }

    Analysis {
        message,
        good,
        suggested_mute_raw,
        span,
        coverage_percent: coverage,
        prior_travel_percent: travel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_few_samples_is_rejected() {
        let a = analyze(&[1, 2, 3], 0, 3250);
        assert!(!a.good);
        assert_eq!(a.span, 0);
    }

    #[test]
    fn narrow_travel_is_rejected() {
        let a = analyze(&[500; 20], 0, 3250);
        assert!(!a.good);
        assert!(a.message.contains("too narrow"));
    }

    #[test]
    fn full_sweep_ending_low_suggests_a_mute_point() {
        let mut samples: Vec<i32> = (0..=32).rev().map(|i| 10 + i * 100).collect();
        samples.extend([10; 8]);
        let a = analyze(&samples, 0, 3250);
        assert!(a.good, "{}", a.message);
        assert_eq!(a.coverage_percent, 100);
        assert_eq!(a.suggested_mute_raw, Some(20));
    }

    #[test]
    fn short_sweep_against_prior_travel_is_limited() {
        let samples: Vec<i32> = (0..=20).map(|i| i * 100).collect();
        let a = analyze(&samples, 3200, 3250);
        assert!(!a.good);
        // Exactly 62.5%, and ties round to even to match the Windows app.
        assert_eq!(a.prior_travel_percent, 62);
    }

    #[test]
    fn sparse_middle_is_reported() {
        let mut samples = vec![0; 10];
        samples.extend([3200; 10]);
        let a = analyze(&samples, 0, 3250);
        assert!(!a.good);
        assert!(a.message.starts_with("Sparse capture"));
    }

    #[test]
    fn thresholds_scale_to_14_bit_sources() {
        let samples: Vec<i32> = (0..40).map(|i| i * 400).collect();
        let a = analyze(&samples, 0, 16383);
        assert!(a.good, "{}", a.message);
    }
}
