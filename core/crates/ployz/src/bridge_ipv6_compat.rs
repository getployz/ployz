//! Keeps a spec that takes IPv6 off a Machine whose daemon would silently ignore it.
//! Delete once no supported release lacks the capability.

use ployz_core::{
    BRIDGE_IPV6_CAPABILITY, ContainerWant, MachineTarget, RpcError, RunTarget,
    machine_matches_target,
};

use crate::cluster::Client;
use crate::sealed_compat::unsupported;

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
    if !new.resolved_spec.container.ipv6 {
        return Ok(());
    }
    let machines = client.clone().machines().await.map_err(RpcError::from)?;
    match machines
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
            Ok(())
        }
        Some(observation) => Err(unsupported(format!(
            "{} runs a ployzd that can't give a Service IPv6 on the ployz bridge; upgrade it before deploying a Service that takes IPv6",
            observation.machine.name
        ))),
        None => Err(unsupported(format!(
            "{target} shows no evidence it can give a Service IPv6; refusing to deploy it there"
        ))),
    }
}

#[cfg(test)]
#[path = "bridge_ipv6_compat_tests.rs"]
mod tests;
