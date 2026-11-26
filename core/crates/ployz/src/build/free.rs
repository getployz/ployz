//! What a Free Build needs of the Servers: a Builder that runs it, and Servers that run
//! its image by the Free receipt. [`takes_free_receipts`] is the one test of the latter.
//! Placement leaves out every Server that fails it, and the last step before an image
//! moves or a Container is made checks it again.

use std::{borrow::Cow, collections::BTreeSet};

use ployz_core::{
    BUILD_FREE_CAPABILITY, DeployIntent, FREE_RECEIPT_CAPABILITY, Machine, MachineObservation,
    MachineTarget, RpcError, ServiceName, machine_matches_target,
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
        .filter(|machine| !takes_free_receipts(&machine.machine))
        .map(|machine| machine.machine.name.to_string())
        .collect();
    if behind.is_empty() {
        return Ok(());
    }
    Err(refusal(&behind, "run", free, ", then deploy again"))
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
    Err(refusal(&behind, "build", free, ", then deploy again"))
}

/// Whether `machine` runs an image by its Free receipt.
pub(crate) fn takes_free_receipts(machine: &Machine) -> bool {
    machine
        .runtime
        .capabilities
        .contains(FREE_RECEIPT_CAPABILITY)
}

/// Refuses a creation of `service` that carries a Free receipt on `machine` when it doesn't
/// take one.
///
/// # Errors
/// Returns `unsupported`, naming `machine`, when it is behind.
pub(crate) fn runs_free_receipt(
    machine: &Machine,
    service: &ServiceName,
) -> Result<(), ConnectError> {
    if takes_free_receipts(machine) {
        return Ok(());
    }
    Err(refusal(
        &BTreeSet::from([machine.name.to_string()]),
        "run",
        std::slice::from_ref(service),
        " first",
    ))
}

/// Refuses a creation of `service` that carries a Free receipt before it reaches `target`,
/// when `target` doesn't take one: the last look after placement chose it. A target this
/// Client doesn't list is left to refuse for itself.
///
/// # Errors
/// Returns `unsupported`, naming `target`, when it is behind, or the Machine listing's
/// error.
pub(crate) async fn admit_free(
    client: &crate::connect::Client,
    target: &MachineTarget,
    service: &ServiceName,
) -> Result<(), RpcError> {
    let machines = client.clone().machines().await.map_err(RpcError::from)?;
    match machines.iter().find(|observation| {
        !observation.unarrived() && machine_matches_target(&observation.machine, target)
    }) {
        Some(observation) => {
            runs_free_receipt(&observation.machine, service).map_err(RpcError::from)
        }
        None => Ok(()),
    }
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
/// `free` Services, and asks to upgrade it, `then` what.
fn refusal(behind: &BTreeSet<String>, act: &str, free: &[ServiceName], then: &str) -> ConnectError {
    let servers = listed(behind.iter().map(String::as_str));
    let services = listed(free.iter().map(ServiceName::as_str));
    let message = format!(
        "{servers} can't {act} {services}, which builds without approval. Upgrade \
         {servers}{then}."
    );
    ConnectError::Remote(RpcError::unsupported(message.clone()).with_details(
        serde_json::json!({ "rejections": { "upgrade needed": behind.len() }, "reasons": [message] }),
    ))
}

#[cfg(test)]
#[path = "free_tests.rs"]
mod tests;
