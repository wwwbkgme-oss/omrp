use serde::{Deserialize, Serialize};

/// Deterministic time. NO relation to wall clock.
/// This is the ONLY time type used in reducers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SequencedInstant {
    pub seq: u64,
    pub logical_time: u64,
}

impl SequencedInstant {
    pub const EPOCH: Self = Self { seq: 0, logical_time: 0 };
}

/// Single source of time in the system.
/// DIESER Clock erzeugt ALLE SequencedInstants.
pub struct Clock {
    seq: u64,
}

impl Clock {
    pub fn new() -> Self {
        Self { seq: 0 }
    }

    pub fn tick(&mut self) -> SequencedInstant {
        self.seq += 1;
        SequencedInstant {
            seq: self.seq,
            logical_time: self.seq,
        }
    }

    pub fn current_seq(&self) -> u64 {
        self.seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clock_monotonic() {
        let mut clock = Clock::new();
        let t1 = clock.tick();
        let t2 = clock.tick();
        let t3 = clock.tick();
        assert!(t1 < t2);
        assert!(t2 < t3);
        assert_eq!(t1.seq, 1);
        assert_eq!(t2.seq, 2);
        assert_eq!(t3.seq, 3);
    }
}
