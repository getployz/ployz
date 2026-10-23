//! Keeps a spec that takes IPv6 off a Machine whose daemon would silently ignore it.
//! Delete once no supported release lacks the capability.

use ployz_core::{
    BRIDGE_IPV6_CAPABILITY, ContainerWant, MachineTarget, ResolvedServiceSpec, RpcError,
    RpcErrorCode, RpcRequestBody, RunTarget, Start, WantedCopy, machine_matches_target,
};

use crate::cluster::Client;

/// Refuses a new Container whose spec takes IPv6 unless its target advertises
/// that it gives it.
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
    admit_spec(client, target, &new.resolved_spec).await
}

/// [`admit`] for a Volume Move's destination copy, which `SetVolumeCopy` creates.
pub(crate) async fn admit_switch(
    client: &Client,
    target: &MachineTarget,
    body: &RpcRequestBody,
) -> Result<(), RpcError> {
    let RpcRequestBody::SetVolumeCopy(request) = body else {
        return Ok(());
    };
    let WantedCopy::Writer {
        start: Some(Start::Create { resolved_spec, .. }),
        ..
    } = &request.want
    else {
        return Ok(());
    };
    admit_spec(client, target, resolved_spec).await
}

async fn admit_spec(
    client: &Client,
    target: &MachineTarget,
    spec: &ResolvedServiceSpec,
) -> Result<(), RpcError> {
    if !spec.container.ipv6 {
        return Ok(());
    }
    let machines = client.clone().machines().await.map_err(RpcError::from)?;
    let message = match machines
        .iter()
        .find(|observation| machine_matches_target(&observation.machine, target))
    {
        Some(observation)
            if observation
                .machine
                .runtime
                .capabilities
                .contains(BRIDGE_IPV6_CAPABILITY) =>
        {
            return Ok(());
        }
        Some(observation) => format!(
            "{} runs a ployzd that can't give a Service IPv6 on the ployz bridge; upgrade it before deploying a Service that takes IPv6",
            observation.machine.name
        ),
        None => format!(
            "{target} shows no evidence it can give a Service IPv6; refusing to deploy it there"
        ),
    };
    Err(RpcError {
        code: RpcErrorCode::Unsupported,
        message,
        details: serde_json::json!({ "capability": BRIDGE_IPV6_CAPABILITY }),
        cause: Vec::new(),
    })
}

#[cfg(test)]
#[path = "bridge_ipv6_compat_tests.rs"]
mod tests;
