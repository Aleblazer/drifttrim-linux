//! Small pure rules: soft takeover, mute toggling and output failover.

/// True once a physical fader has reached or crossed the pickup target.
pub fn has_reached(previous: Option<i32>, current: i32, target: i32, tolerance: i32) -> bool {
    if (current - target).abs() <= tolerance {
        return true;
    }
    previous.is_some_and(|last| (last - target) * (current - target) <= 0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MuteState {
    pub muted: bool,
    pub target: i32,
    pub pre_mute: i32,
}

pub fn toggle_mute(muted: bool, target: i32, pre_mute: i32) -> MuteState {
    if muted {
        let restored = pre_mute.clamp(0, 100);
        MuteState {
            muted: false,
            target: restored,
            pre_mute: restored,
        }
    } else {
        MuteState {
            muted: true,
            target: 0,
            pre_mute: target.clamp(0, 100),
        }
    }
}

/// Picks the override if present, otherwise the highest-ranked present output.
pub fn resolve_output<'a>(
    ranked: impl IntoIterator<Item = &'a str>,
    is_present: impl Fn(&str) -> bool,
    override_id: Option<&'a str>,
) -> Option<&'a str> {
    if let Some(id) = override_id.filter(|id| is_present(id)) {
        return Some(id);
    }
    ranked.into_iter().find(|id| is_present(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pickup_within_tolerance() {
        assert!(has_reached(None, 49, 50, 2));
        assert!(!has_reached(None, 47, 50, 2));
    }

    #[test]
    fn pickup_on_crossing_in_either_direction() {
        assert!(has_reached(Some(40), 60, 50, 2));
        assert!(has_reached(Some(60), 40, 50, 2));
        assert!(!has_reached(Some(30), 40, 50, 2));
    }

    #[test]
    fn mute_round_trip_restores_level() {
        let muted = toggle_mute(false, 72, 50);
        assert_eq!(
            muted,
            MuteState {
                muted: true,
                target: 0,
                pre_mute: 72
            }
        );
        let restored = toggle_mute(true, muted.target, muted.pre_mute);
        assert_eq!(
            restored,
            MuteState {
                muted: false,
                target: 72,
                pre_mute: 72
            }
        );
    }

    #[test]
    fn output_failover_order() {
        let present = |id: &str| id == "b" || id == "c";
        assert_eq!(resolve_output(["a", "b", "c"], present, None), Some("b"));
        assert_eq!(resolve_output(["b"], present, Some("c")), Some("c"));
        assert_eq!(resolve_output(["c"], present, Some("gone")), Some("c"));
        assert_eq!(resolve_output(["a"], present, None), None);
    }
}
