//! Bounded history of our own recent volume writes.
//!
//! Volume change notifications arrive asynchronously and can land after a
//! newer write, so a single expected value cannot tell our own echoes apart
//! from a change made in another mixer.

const CAPACITY: usize = 64;
pub const VOLUME_EPSILON: f32 = 0.0049;

#[derive(Clone, Debug, Default)]
pub struct ExpectedWrites {
    writes: Vec<(f32, u64)>,
}

impl ExpectedWrites {
    pub fn add(&mut self, volume: f32, until: u64, now: u64) {
        self.remove_expired(now);
        if self.writes.len() == CAPACITY {
            self.writes.remove(0);
        }
        self.writes.push((volume, until));
    }

    /// Returns true and forgets the write if `actual` matches a recent one.
    pub fn consume(&mut self, actual: f32, now: u64) -> bool {
        self.remove_expired(now);
        match self
            .writes
            .iter()
            .position(|(v, _)| (v - actual).abs() < VOLUME_EPSILON)
        {
            Some(index) => {
                self.writes.remove(index);
                true
            }
            None => false,
        }
    }

    fn remove_expired(&mut self, now: u64) {
        self.writes.retain(|(_, until)| now <= *until);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_is_consumed_once() {
        let mut w = ExpectedWrites::default();
        w.add(0.5, 1000, 0);
        assert!(w.consume(0.502, 10));
        assert!(!w.consume(0.5, 20));
    }

    #[test]
    fn out_of_order_echoes_still_match() {
        let mut w = ExpectedWrites::default();
        w.add(0.1, 1000, 0);
        w.add(0.2, 1000, 0);
        assert!(w.consume(0.2, 5));
        assert!(w.consume(0.1, 6));
    }

    #[test]
    fn expired_writes_look_external() {
        let mut w = ExpectedWrites::default();
        w.add(0.5, 100, 0);
        assert!(!w.consume(0.5, 101));
    }

    #[test]
    fn capacity_drops_the_oldest() {
        let mut w = ExpectedWrites::default();
        for i in 0..=CAPACITY {
            w.add(i as f32, 1000, 0);
        }
        assert!(!w.consume(0.0, 1));
        assert!(w.consume(CAPACITY as f32, 1));
    }
}
