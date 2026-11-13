//! Translating a Service's Healthcheck into what Docker's create body carries.

use bollard::models::HealthConfig;
use ployz_core::{HEALTHCHECK_DISABLE_SENTINEL, HealthcheckSpec};

use super::Error;

/// Docker's own health check turned off.
pub(super) fn disabled() -> HealthConfig {
    HealthConfig {
        test: Some(vec![HEALTHCHECK_DISABLE_SENTINEL.into()]),
        ..Default::default()
    }
}

pub(in crate::docker) fn docker_healthcheck(spec: &HealthcheckSpec) -> Result<HealthConfig, Error> {
    match spec {
        HealthcheckSpec::Disabled | HealthcheckSpec::Http(_) => Ok(disabled()),
        HealthcheckSpec::Configured(configured) => Ok(HealthConfig {
            test: Some(configured.test.as_slice().to_vec()),
            interval: millis_to_nanos(configured.interval_millis)?,
            timeout: millis_to_nanos(configured.timeout_millis)?,
            retries: configured.retries.map(i64::from),
            start_period: millis_to_nanos(configured.start_period_millis)?,
            start_interval: millis_to_nanos(configured.start_interval_millis)?,
        }),
    }
}

fn millis_to_nanos(value: Option<u64>) -> Result<Option<i64>, Error> {
    value
        .map(|value| {
            value
                .checked_mul(1_000_000)
                .and_then(|value| i64::try_from(value).ok())
                .ok_or(Error::DurationOverflow)
        })
        .transpose()
}
