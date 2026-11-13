//! The capped doubling delay the daemon's retrying loops share.

use std::time::Duration;

/// A delay that starts at `floor` and doubles on each take until it reaches `cap`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Doubling {
    floor: Duration,
    cap: Duration,
    next: Duration,
}

impl Doubling {
    pub(crate) fn between(floor: Duration, cap: Duration) -> Self {
        Self {
            floor,
            cap,
            next: floor,
        }
    }

    /// The current delay; the next take is double, up to the cap.
    pub(crate) fn take(&mut self) -> Duration {
        let delay = self.next;
        self.next = (delay * 2).min(self.cap);
        delay
    }

    /// Half to all of the current delay, so retrying peers do not move together. Advances
    /// the same way as [`Self::take`].
    pub(crate) fn take_jittered(&mut self) -> Duration {
        let half = self.take() / 2;
        half + half.mul_f64(fastrand::f64())
    }

    pub(crate) fn reset(&mut self) {
        self.next = self.floor;
    }
}

#[cfg(test)]
mod tests {
    use super::Doubling;
    use std::time::Duration;

    const fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn take_doubles_to_the_cap_and_reset_returns_to_the_floor() {
        let mut backoff = Doubling::between(secs(1), secs(10));
        let taken: Vec<_> = (0..6).map(|_| backoff.take()).collect();
        assert_eq!(
            taken,
            [secs(1), secs(2), secs(4), secs(8), secs(10), secs(10)]
        );
        backoff.reset();
        assert_eq!(backoff.take(), secs(1));
    }

    #[test]
    fn take_jittered_stays_within_half_to_all_of_each_step() {
        let mut backoff = Doubling::between(secs(1), secs(10));
        for step in [1, 2, 4, 8, 10, 10] {
            let wait = backoff.take_jittered();
            assert!(
                wait >= secs(step) / 2 && wait <= secs(step),
                "step {step} waited {wait:?}"
            );
        }
    }
}
