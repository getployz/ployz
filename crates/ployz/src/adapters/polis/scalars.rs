//! Shared SQL scalar codecs for Ployz Corrosion adapters.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

const NANOS_PER_SECOND: i64 = 1_000_000_000;

pub(in crate::adapters::polis) fn unix_time_parts(
    value: SystemTime,
) -> Result<(i64, i64), polis::StoreError> {
    let duration = value
        .duration_since(UNIX_EPOCH)
        .map_err(|_| polis::StoreError::MalformedPayload)?;
    Ok((
        i64_from_u64(duration.as_secs())?,
        i64::from(duration.subsec_nanos()),
    ))
}

pub(in crate::adapters::polis) fn system_time_from_unix_parts(
    secs: i64,
    nanos: i64,
) -> Result<SystemTime, polis::StoreError> {
    if secs < 0 || !(0..NANOS_PER_SECOND).contains(&nanos) {
        return Err(polis::StoreError::MalformedPayload);
    }
    Ok(UNIX_EPOCH + Duration::new(secs as u64, nanos as u32))
}

pub(in crate::adapters::polis) fn i64_from_u64(value: u64) -> Result<i64, polis::StoreError> {
    i64::try_from(value).map_err(|_| polis::StoreError::MalformedPayload)
}

pub(in crate::adapters::polis) fn u64_from_i64(value: i64) -> Result<u64, polis::StoreError> {
    u64::try_from(value).map_err(|_| polis::StoreError::MalformedPayload)
}
