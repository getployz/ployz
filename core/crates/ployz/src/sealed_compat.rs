//! Keeps Sealed values off a Cluster until every Machine can keep them sealed.
//! Delete once no supported release lacks these capabilities.

use std::borrow::Cow;

use ployz_core::{
    BUILD_SEALED_CAPABILITY, ContainerWant, ENV_SEALED_CAPABILITY, EnvValue,
    MANAGEMENT_SCOPE_CAPABILITY, MachineObservation, MachineTarget, RpcError, RpcErrorCode,
    RunTarget, config::BuildMethod, machine_matches_target,
};

use crate::{cluster::Client, connect::ConnectError};

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

/// The Machines a Build may run on. One that reads Sealed values, built and
/// named by `reads`, runs only where the daemon opens them; without one, every
/// visible Machine stays.
pub(crate) fn builders<'a>(
    visible: &'a [MachineObservation],
    reads: Option<(BuildMethod, Vec<&str>)>,
) -> Result<Cow<'a, [MachineObservation]>, ConnectError> {
    let Some((method, read)) = reads else {
        return Ok(Cow::Borrowed(visible));
    };
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
        let names = names.join(", ");
        match method {
            // One Server that opens them is enough: the Build runs there.
            BuildMethod::Dockerfile => {
                let it = if read.len() == 1 { "it" } else { "them" };
                format!(
                    "{names} cannot open sealed values, and this build reads {}. \
                     Upgrade a Server to 0.3.5 or later to build there, or stop reading {it} in the build.",
                    read.join(", ")
                )
            }
            // A Dockerfile reading none builds anywhere; Railpack can't read none.
            BuildMethod::Railpack => format!(
                "{names} cannot open sealed values. Railpack gives the build every variable, \
                 sealed ones included, so upgrade a Server to build there."
            ),
        }
    };
    let mut error = unsupported(reason.clone());
    error.details = serde_json::json!({
        "rejections": {"cannot open sealed values": behind.len()},
        "reasons": [reason],
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
