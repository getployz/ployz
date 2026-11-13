//! The five container counters a Service's history is made of, named once.

use ployz_core::metrics::{MetricSample, ServicePoint};

pub(super) const COUNTERS: usize = 5;

/// One value for each container counter (cpu, rx, tx, io read, io write), in
/// `ServicePoint` order. A sample holds them as cumulative `u64`s, a point as
/// per-second `f64` rates, and the folds in between as `Delta`s or means.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Counters<T>(pub(super) [T; COUNTERS]);

impl<T> Counters<T> {
    pub(super) fn map<U>(self, f: impl FnMut(T) -> U) -> Counters<U> {
        Counters(self.0.map(f))
    }

    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.0.iter_mut()
    }
}

impl<T> IntoIterator for Counters<T> {
    type Item = T;
    type IntoIter = std::array::IntoIter<T, COUNTERS>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl Counters<Option<u64>> {
    pub(super) fn of_sample(sample: &MetricSample) -> Self {
        Self([
            sample.cpu_usec,
            sample.rx,
            sample.tx,
            sample.io_r,
            sample.io_w,
        ])
    }
}

impl Counters<Option<f64>> {
    pub(super) fn of_point(point: &ServicePoint) -> Self {
        Self([point.cpu_usec, point.rx, point.tx, point.io_r, point.io_w])
    }

    /// A point at `ts` holding these rates and the given memory.
    pub(super) fn into_point(
        self,
        ts: i64,
        mem_bytes: Option<u64>,
        mem_limit: Option<u64>,
    ) -> ServicePoint {
        let Self([cpu_usec, rx, tx, io_r, io_w]) = self;
        ServicePoint {
            ts,
            cpu_usec,
            mem_bytes,
            mem_limit,
            rx,
            tx,
            io_r,
            io_w,
        }
    }
}

/// Adds the point of another Service id that held the same name at the same
/// `ts` onto `sum`. A rate no id reported stays unknown, a reported one adds,
/// and the memory limit is unknown once any id's is.
pub(super) fn combine(sum: &mut ServicePoint, replica: ServicePoint) {
    let mut rates = Counters::of_point(sum);
    for (total, rate) in rates.iter_mut().zip(Counters::of_point(&replica)) {
        if let Some(rate) = rate {
            *total = Some(total.unwrap_or(0.0) + rate);
        }
    }
    let mem_bytes = match replica.mem_bytes {
        Some(mem) => Some(sum.mem_bytes.unwrap_or(0).saturating_add(mem)),
        None => sum.mem_bytes,
    };
    let mem_limit = sum
        .mem_limit
        .zip(replica.mem_limit)
        .map(|(total, limit)| total.saturating_add(limit));
    *sum = rates.into_point(sum.ts, mem_bytes, mem_limit);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(rates: [Option<f64>; COUNTERS], mem: Option<u64>, limit: Option<u64>) -> ServicePoint {
        Counters(rates).into_point(60, mem, limit)
    }

    #[test]
    fn combining_adds_reported_rates_and_keeps_unreported_ones_unknown() {
        let mut sum = point(
            [Some(1.0), None, Some(3.0), None, None],
            Some(10),
            Some(100),
        );
        combine(
            &mut sum,
            point([Some(2.0), None, None, Some(4.0), None], Some(5), Some(50)),
        );
        assert_eq!(
            Counters::of_point(&sum),
            Counters([Some(3.0), None, Some(3.0), Some(4.0), None])
        );
        assert_eq!((sum.mem_bytes, sum.mem_limit), (Some(15), Some(150)));
    }

    #[test]
    fn the_limit_is_unknown_once_any_ids_is() {
        let mut sum = point([None; COUNTERS], Some(1), Some(100));
        combine(&mut sum, point([None; COUNTERS], None, None));
        assert_eq!((sum.mem_bytes, sum.mem_limit), (Some(1), None));
        combine(&mut sum, point([None; COUNTERS], Some(2), Some(7)));
        assert_eq!((sum.mem_bytes, sum.mem_limit), (Some(3), None));
    }
}
