use serde::{Deserialize, Serialize};

use crate::ValueError;

/// CPU nanounits in Docker's nonnegative signed integer range.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct CpuNanos(i64);

impl CpuNanos {
    /// Convert CPUs to nanounits, truncating fractional nanounits toward zero.
    pub fn from_cpus(cpus: f64) -> Result<Self, ValueError> {
        let nanos = cpus * 1e9;
        // i64::MAX rounds up to 2^63 as f64, so that boundary must be excluded.
        if !cpus.is_finite() || cpus < 0.0 || nanos >= 9_223_372_036_854_775_808.0 {
            return Err(ValueError::new(
                "CPUs",
                cpus.to_string(),
                "a nonnegative finite quantity whose nanounits are below 2^63",
            ));
        }
        Self::try_from(nanos as i64)
    }

    #[must_use]
    pub fn get(self) -> i64 {
        self.0
    }
}

impl TryFrom<i64> for CpuNanos {
    type Error = ValueError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        if value < 0 {
            return Err(ValueError::new(
                "CPU nanounits",
                value.to_string(),
                "a nonnegative integer",
            ));
        }
        Ok(Self(value))
    }
}

impl From<CpuNanos> for i64 {
    fn from(value: CpuNanos) -> Self {
        value.get()
    }
}

/// Bytes in Docker's nonnegative signed integer range, including zero and i64::MAX.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct ByteQuantity(i64);

impl ByteQuantity {
    #[must_use]
    pub fn get(self) -> i64 {
        self.0
    }
}

impl TryFrom<i64> for ByteQuantity {
    type Error = ValueError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        if value < 0 {
            return Err(ValueError::new(
                "bytes",
                value.to_string(),
                "a nonnegative integer",
            ));
        }
        Ok(Self(value))
    }
}

impl From<ByteQuantity> for i64 {
    fn from(value: ByteQuantity) -> Self {
        value.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_cpu_conversion_rejects_overflow_without_saturation() {
        for invalid in [
            -1.0,
            -f64::MIN_POSITIVE,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            1e20,
            f64::MAX,
            9_223_372_036.854_776,
        ] {
            assert!(CpuNanos::from_cpus(invalid).is_err(), "{invalid}");
        }
        for (cpus, nanos) in [
            (0.0, 0),
            (0.125, 125_000_000),
            (1.5, 1_500_000_000),
            (0.000_000_001_9, 1),
            (9_223_372_036.854_774, 9_223_372_036_854_774_784),
        ] {
            assert_eq!(CpuNanos::from_cpus(cpus).unwrap().get(), nanos);
        }
        assert!(CpuNanos::try_from(-1).is_err());
        assert!(ByteQuantity::try_from(-1).is_err());
        assert_eq!(CpuNanos::try_from(i64::MAX).unwrap().get(), i64::MAX);
        assert_eq!(ByteQuantity::try_from(i64::MAX).unwrap().get(), i64::MAX);
    }
}
