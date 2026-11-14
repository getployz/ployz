//! Translating a Service's Healthcheck into what Docker's create body carries, and the
//! Healthcheck Docker reports a Container has back into a spec's.

use bollard::models::HealthConfig;
use ployz_core::{
    ConfiguredHealthcheck, HEALTHCHECK_DISABLE_SENTINEL, HealthcheckCommand, HealthcheckSpec,
};

use super::Error;
use crate::docker::RawContainerConfig;

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

pub(in crate::docker) fn effective_healthcheck(
    config: Option<&RawContainerConfig>,
) -> Option<HealthcheckSpec> {
    let healthcheck = config?.healthcheck.as_ref()?;
    let test = healthcheck.test.clone().unwrap_or_default();
    if test
        .first()
        .is_some_and(|command| command == HEALTHCHECK_DISABLE_SENTINEL)
    {
        return Some(HealthcheckSpec::Disabled);
    }
    Some(HealthcheckSpec::Configured(ConfiguredHealthcheck {
        test: HealthcheckCommand::parse(test).ok()?,
        interval_millis: nanos_to_millis(healthcheck.interval),
        timeout_millis: nanos_to_millis(healthcheck.timeout),
        start_period_millis: nanos_to_millis(healthcheck.start_period),
        start_interval_millis: nanos_to_millis(healthcheck.start_interval),
        retries: healthcheck
            .retries
            .filter(|retries| *retries > 0)
            .and_then(|retries| u32::try_from(retries).ok()),
        deadline_millis: None,
    }))
}

fn nanos_to_millis(nanos: Option<i64>) -> Option<u64> {
    nanos
        .filter(|nanos| *nanos > 0)
        .and_then(|nanos| u64::try_from(nanos).ok())
        .map(|nanos| nanos.div_ceil(1_000_000))
}
