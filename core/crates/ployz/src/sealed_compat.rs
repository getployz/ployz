//! Keeps Sealed values off a Cluster until every Machine can keep them sealed.
//! Delete once no supported release lacks these capabilities.

use std::borrow::Cow;

use ployz_core::{
    BUILD_SEALED_CAPABILITY, ContainerWant, ENV_SEALED_CAPABILITY, EnvValue,
    MANAGEMENT_SCOPE_CAPABILITY, MachineObservation, MachineTarget, RpcError, RpcErrorCode,
    RunTarget, machine_matches_target,
};

use crate::{build::SEALED_BUILDS_RUN_ON_SERVER, cluster::Client, connect::ConnectError};

const SEALED_READY: [&str; 2] = [MANAGEMENT_SCOPE_CAPABILITY, ENV_SEALED_CAPABILITY];

/// Refuses a want that carries a Sealed value while any Machine lacks the
/// capabilities that keep it sealed, or the target shows none at all.
pub(crate) async fn admit(
    client: &Client,
    target: &MachineTarget,
    want: &ContainerWant,
) -> Result<(), RpcError> {
    let ContainerWant::Running {
        container: RunTarget::New(new),
        ..
    } = want
    else {
        return Ok(());
    };
    let spec = &new.resolved_spec;
    let hook = spec
        .pre_deploy
        .iter()
        .flat_map(|hook| hook.environment.values());
    if !spec
        .container
        .environment
        .values()
        .chain(hook)
        .any(EnvValue::is_sealed)
    {
        return Ok(());
    }
    let machines = client.clone().machines().await.map_err(RpcError::from)?;
    if !machines
        .iter()
        .any(|observation| machine_matches_target(&observation.machine, target))
    {
        return Err(unsupported(format!(
            "{target} shows no evidence it can open Sealed values; refusing to send them"
        )));
    }
    let behind: Vec<_> = machines
        .iter()
        .filter(|observation| {
            let capabilities = &observation.machine.runtime.capabilities;
            !SEALED_READY.iter().all(|name| capabilities.contains(*name))
        })
        .map(|observation| observation.machine.name.to_string())
        .collect();
    if behind.is_empty() {
        return Ok(());
    }
    Err(unsupported(format!(
        "{} cannot open Sealed values; upgrade every Machine before deploying them",
        behind.join(", ")
    )))
}

/// The Machines a Build may run on. One that reads a Sealed value runs only
/// where the daemon opens them; without one, every visible Machine stays.
pub(crate) fn builders(
    visible: &[MachineObservation],
    sealed: bool,
) -> Result<Cow<'_, [MachineObservation]>, ConnectError> {
    if !sealed {
        return Ok(Cow::Borrowed(visible));
    }
    let (able, behind): (Vec<_>, Vec<_>) = visible.iter().cloned().partition(|observation| {
        observation
            .machine
            .runtime
            .capabilities
            .contains(BUILD_SEALED_CAPABILITY)
    });
    if !able.is_empty() {
        return Ok(Cow::Owned(able));
    }
    let reason = if behind.is_empty() {
        "No Server is enrolled.".to_owned()
    } else {
        let names: Vec<_> = behind
            .iter()
            .map(|observation| observation.machine.name.to_string())
            .collect();
        format!(
            "{} cannot open sealed values; upgrade every Server first.",
            names.join(", ")
        )
    };
    let mut error = unsupported(format!("{SEALED_BUILDS_RUN_ON_SERVER} {reason}"));
    error.details = serde_json::json!({
        "rejections": {"cannot open sealed values": behind.len()},
        "reasons": [reason],
        "lead": SEALED_BUILDS_RUN_ON_SERVER,
    });
    Err(ConnectError::Remote(error))
}

fn unsupported(message: String) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unsupported,
        message,
        details: serde_json::Value::Null,
        cause: Vec::new(),
    }
}

#[cfg(test)]
#[path = "sealed_compat_tests.rs"]
pub(crate) mod tests;
