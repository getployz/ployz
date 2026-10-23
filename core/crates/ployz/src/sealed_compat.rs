//! Keeps Sealed values off a Cluster until every Machine can keep them sealed.
//! Delete once no supported release lacks these capabilities.

use ployz_core::{
    ContainerWant, ENV_SEALED_CAPABILITY, EnvValue, MANAGEMENT_SCOPE_CAPABILITY, MachineTarget,
    RpcError, RpcErrorCode, RunTarget, machine_matches_target,
};

use crate::cluster::Client;

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

pub(crate) fn unsupported(message: String) -> RpcError {
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
