//! How often metrics are sampled and how long each tier is kept.
//!
//! The sampler, rate buckets, rollups, history and cleanup all read this one
//! value, so a change to a tier's age or width cannot reach one of them and
//! miss another.

use std::time::Duration;

use crate::observe::layout::NANOS_PER_SEC;

/// The cadence and retention of metric rows. Every field is whole seconds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Retention {
    /// The sampler's tick, which is also the narrowest bucket a raw read
    /// answers on.
    pub(crate) sample: Duration,
    /// The width of one rollup point.
    pub(crate) step: Duration,
    /// How long a raw hour is kept once it has closed.
    pub(crate) raw: Duration,
    /// How long a rollup is kept once its hour has closed.
    pub(crate) rollup: Duration,
}

pub(crate) const RETENTION: Retention = Retention {
    sample: Duration::from_secs(10),
    step: Duration::from_secs(5 * 60),
    raw: Duration::from_secs(24 * 60 * 60),
    rollup: Duration::from_secs(90 * 24 * 60 * 60),
};

pub(crate) const HOUR_NANOS: i64 = 3_600 * NANOS_PER_SEC;

const fn nanos(span: Duration) -> i64 {
    (span.as_secs().saturating_mul(NANOS_PER_SEC as u64)) as i64
}

impl Retention {
    pub(crate) const fn sample_nanos(&self) -> i64 {
        nanos(self.sample)
    }

    pub(crate) const fn step_nanos(&self) -> i64 {
        nanos(self.step)
    }

    /// The oldest age a range may start at and be answered from raw rows
    /// alone: the raw age less one hour. A raw hour is deleted once it ended
    /// `raw` ago, so the hour such a range starts in has at least an hour of
    /// life left, and a read that began in time is not cut short by cleanup.
    pub(crate) const fn raw_span_nanos(&self) -> i64 {
        nanos(self.raw) - HOUR_NANOS
    }
}

#[cfg(test)]
mod tests {
    use super::{HOUR_NANOS, RETENTION};

    const SEC: i64 = 1_000_000_000;

    #[test]
    fn the_retention_values_are_a_day_of_raw_and_ninety_days_of_rollups() {
        assert_eq!(RETENTION.raw.as_secs(), 24 * 3_600);
        assert_eq!(RETENTION.rollup.as_secs(), 90 * 24 * 3_600);
    }

    #[test]
    fn rows_are_sampled_every_ten_seconds_and_rolled_to_five_minute_points() {
        assert_eq!(RETENTION.sample_nanos(), 10 * SEC);
        assert_eq!(RETENTION.step_nanos(), 300 * SEC);
        assert_eq!(HOUR_NANOS % RETENTION.step_nanos(), 0);
    }

    #[test]
    fn a_raw_range_may_start_23_hours_back_one_hour_short_of_the_raw_age() {
        assert_eq!(RETENTION.raw_span_nanos(), 23 * HOUR_NANOS);
        assert_eq!(
            RETENTION.raw_span_nanos() + HOUR_NANOS,
            i64::try_from(RETENTION.raw.as_secs()).unwrap() * SEC
        );
    }
}
