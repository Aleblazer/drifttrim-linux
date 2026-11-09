//! Pairs fresh MSB/LSB control changes without exposing coarse-only values.

pub const PAIR_TIMEOUT_MS: u64 = 100;
const SLOTS: usize = 16 * 32;

#[derive(Clone, Debug)]
pub struct CcDecoder {
    coarse: [u8; SLOTS],
    at: [u64; SLOTS],
    pending: [bool; SLOTS],
}

impl Default for CcDecoder {
    fn default() -> Self {
        Self {
            coarse: [0; SLOTS],
            at: [0; SLOTS],
            pending: [false; SLOTS],
        }
    }
}

impl CcDecoder {
    /// Feeds one control change. Returns `(axis, raw)` when a fine byte
    /// completes a coarse byte received within [`PAIR_TIMEOUT_MS`].
    pub fn accept14(
        &mut self,
        channel: u8,
        controller: u8,
        value: u8,
        now: u64,
    ) -> Option<(usize, i32)> {
        if channel > 15 || controller > 63 || value > 127 {
            return None;
        }
        let axis = channel as usize * 32 + controller as usize % 32;
        if controller < 32 {
            self.coarse[axis] = value;
            self.at[axis] = now;
            self.pending[axis] = true;
            return None;
        }
        let fresh =
            self.pending[axis] && now >= self.at[axis] && now - self.at[axis] <= PAIR_TIMEOUT_MS;
        self.pending[axis] = false;
        fresh.then(|| (axis, ((self.coarse[axis] as i32) << 7) | value as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coarse_then_fine_makes_a_value() {
        let mut d = CcDecoder::default();
        assert_eq!(d.accept14(0, 16, 0x67, 0), None);
        assert_eq!(d.accept14(0, 48, 0x10, 5), Some((16, (0x67 << 7) | 0x10)));
    }

    #[test]
    fn orphan_and_reused_fine_bytes_are_ignored() {
        let mut d = CcDecoder::default();
        assert_eq!(d.accept14(0, 48, 1, 0), None);
        d.accept14(0, 16, 2, 1);
        assert!(d.accept14(0, 48, 3, 2).is_some());
        assert_eq!(d.accept14(0, 48, 4, 3), None);
    }

    #[test]
    fn expired_pairs_are_dropped() {
        let mut d = CcDecoder::default();
        d.accept14(0, 16, 2, 0);
        assert_eq!(d.accept14(0, 48, 3, PAIR_TIMEOUT_MS + 1), None);
        d.accept14(0, 16, 2, 200);
        assert!(d.accept14(0, 48, 3, 200 + PAIR_TIMEOUT_MS).is_some());
    }

    #[test]
    fn channels_and_controls_pair_independently() {
        let mut d = CcDecoder::default();
        d.accept14(0, 16, 1, 0);
        d.accept14(1, 16, 2, 0);
        d.accept14(0, 17, 3, 0);
        assert_eq!(d.accept14(1, 48, 0, 1), Some((48, 2 << 7)));
        assert_eq!(d.accept14(0, 49, 0, 1), Some((17, 3 << 7)));
        assert_eq!(d.accept14(0, 48, 0, 1), Some((16, 1 << 7)));
    }

    #[test]
    fn out_of_range_input_is_rejected() {
        let mut d = CcDecoder::default();
        assert_eq!(d.accept14(16, 16, 0, 0), None);
        assert_eq!(d.accept14(0, 64, 0, 0), None);
        assert_eq!(d.accept14(0, 16, 128, 0), None);
    }

    #[test]
    fn all_32_default_firmware_controls_are_distinct() {
        let mut d = CcDecoder::default();
        let mut seen = std::collections::HashSet::new();
        for channel in 0..4u8 {
            for cc in 16..24u8 {
                d.accept14(channel, cc, 1, 0);
                let (axis, raw) = d.accept14(channel, cc + 32, 1, 0).unwrap();
                assert_eq!(raw, 129);
                assert!(seen.insert(axis));
            }
        }
        assert_eq!(seen.len(), 32);
    }
}
