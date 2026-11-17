//! The Environment's default region: the one Setting no node holds.

use std::collections::BTreeSet;

use ployz_core::{Hint, Region, RpcError};
use serde_json::{Value, json};

use super::SettingPath;
use crate::error;
use crate::scope::Environment;

/// Refuse `region` when no Server is in it, by `regions`, the regions the Servers are in.
///
/// # Errors
/// Returns `invalid_argument` naming the regions there are, or how to give a Server one.
pub fn refuse_unknown(region: &Region, regions: &BTreeSet<Region>) -> Result<(), RpcError> {
    if regions.contains(region) {
        return Ok(());
    }
    let named = regions.iter().map(Region::as_str).collect::<Vec<_>>();
    if named.is_empty() {
        return Err(
            error::invalid("No Server has a region yet.", json!({ "region": region })).with_hint(
                Hint::Next(format!("ployz server set NAME --label-add region={region}")),
            ),
        );
    }
    Err(error::invalid(
        format!("No Server is in {region}. Regions: {}.", named.join(", ")),
        json!({ "region": region, "regions": named }),
    ))
}

/// Set `environment`'s default region to `value`, or clear it with none, refusing a
/// region none of `regions` is. Whether Working State changed.
pub(super) fn set(
    environment: &mut Environment,
    value: Option<&Value>,
    regions: Option<&BTreeSet<Region>>,
) -> Result<bool, RpcError> {
    let region = value.map(parse).transpose()?;
    if let (Some(region), Some(regions)) = (&region, regions) {
        refuse_unknown(region, regions)?;
    }
    let changed = environment.working.region != region;
    environment.working.region = region;
    Ok(changed)
}

fn parse(value: &Value) -> Result<Region, RpcError> {
    value
        .as_str()
        .and_then(|region| Region::parse(region).ok())
        .ok_or_else(|| {
            error::invalid(
                format!(
                    "{}: expected a region, the value of a Server's region label",
                    SettingPath::REGION
                ),
                json!({ "example": "fsn" }),
            )
        })
}
