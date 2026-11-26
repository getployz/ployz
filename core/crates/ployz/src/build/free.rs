//! What a Free Build needs of the Servers before it starts: every Server its Service may
//! run on able to run its image by the Free receipt, and a Builder that runs it.

use std::{borrow::Cow, collections::BTreeSet};

use ployz_core::{
    BUILD_FREE_CAPABILITY, DeployIntent, FREE_RECEIPT_CAPABILITY, MachineObservation, RpcError,
    ServiceName,
};

use crate::connect::ConnectError;

/// Refuses when a Server one of the `free` Services of `intent` may run on can't run a
/// Free Build's image by its receipt, naming each such Server.
///
/// # Errors
/// Returns `unsupported`, with each Server named, when a Server is behind.
pub(crate) fn free_destinations(
    free: &[ServiceName],
    intent: &DeployIntent,
    machines: &[MachineObservation],
) -> Result<(), ConnectError> {
    let behind: BTreeSet<String> = intent
        .target
        .iter()
        .filter(|spec| free.contains(&spec.name))
        .flat_map(|spec| super::platforms::placeable(spec, &intent.namespace, machines))
        .filter(|machine| !advertises(machine, FREE_RECEIPT_CAPABILITY))
        .map(|machine| machine.machine.name.to_string())
        .collect();
    if behind.is_empty() {
        return Ok(());
    }
    Err(refusal(&behind, "run", free))
}

/// The `builders` that run a Free Build, when any of `free` builds. Refuses naming every
/// Builder when none does.
///
/// # Errors
/// Returns `unsupported`, with each Builder named, when none runs a Free Build.
pub(crate) fn free_builders<'a>(
    free: &[ServiceName],
    builders: Cow<'a, [MachineObservation]>,
) -> Result<Cow<'a, [MachineObservation]>, ConnectError> {
    if free.is_empty() {
        return Ok(builders);
    }
    let able: Vec<_> = builders
        .iter()
        .filter(|machine| advertises(machine, BUILD_FREE_CAPABILITY))
        .cloned()
        .collect();
    if !able.is_empty() {
        return Ok(Cow::Owned(able));
    }
    let behind = builders
        .iter()
        .map(|machine| machine.machine.name.to_string())
        .collect();
    Err(refusal(&behind, "build", free))
}

fn advertises(machine: &MachineObservation, capability: &str) -> bool {
    machine.machine.runtime.capabilities.contains(capability)
}

/// "web-1", "web-1 and web-2", or "web-1, web-2 and web-3".
fn listed<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let names: Vec<_> = names.collect();
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// A refusal that names each Server in `behind`, which can't yet `act` (build or run) the
/// `free` Services, and asks to upgrade it.
fn refusal(behind: &BTreeSet<String>, act: &str, free: &[ServiceName]) -> ConnectError {
    let servers = listed(behind.iter().map(String::as_str));
    let services = listed(free.iter().map(ServiceName::as_str));
    let message = format!(
        "{servers} can't {act} {services}, which builds without approval. Upgrade {servers}, \
         then deploy again."
    );
    ConnectError::Remote(RpcError::unsupported(message.clone()).with_details(
        serde_json::json!({ "rejections": { "upgrade needed": behind.len() }, "reasons": [message] }),
    ))
}

#[cfg(test)]
#[path = "free_tests.rs"]
mod tests;
