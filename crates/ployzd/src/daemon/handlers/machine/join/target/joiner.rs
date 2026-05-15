use ipnet::Ipv4Net;
use ployz_api::MachineSelfTransition;
use ployz_model::MachineMembership;
#[cfg(test)]
use ployz_store_api::MachineMembershipStore;

use super::super::super::types::MachineAddContext;
use super::super::remote::{
    nats_self_record, nats_transition_self, remote_self_record, wait_for_nats_command_responder,
    wait_for_nats_ready, wait_for_remote_ready,
};

pub(super) async fn wait_for_joiner_command_responder(
    context: &MachineAddContext,
    machine: &MachineMembership,
) -> Result<(), String> {
    if let Some(client) = &context.nats_rpc {
        return wait_for_nats_command_responder(client, machine).await;
    }

    #[cfg(test)]
    {
        let _ = machine;
        Ok(())
    }

    #[cfg(not(test))]
    {
        let _ = machine;
        Err("NATS RPC client unavailable".into())
    }
}

pub(super) async fn joiner_self_record(
    context: &MachineAddContext,
    target: &str,
    machine: &MachineMembership,
) -> Result<MachineMembership, String> {
    if let Some(client) = &context.nats_rpc {
        return nats_self_record(client, machine).await;
    }
    remote_self_record(target, &context.ssh_options).await
}

pub(super) async fn wait_for_joiner_ready(
    context: &MachineAddContext,
    target: &str,
    machine: &MachineMembership,
) -> Result<(), String> {
    if let Some(client) = &context.nats_rpc {
        return wait_for_nats_ready(client, machine).await;
    }
    wait_for_remote_ready(
        target,
        &context.ssh_options,
        context.remote_ready_wait_policy,
    )
    .await
}

pub(super) async fn activate_joiner_lifecycle(
    context: &MachineAddContext,
    record: &MachineMembership,
    assigned_subnet: Ipv4Net,
) -> Result<(), String> {
    if let Some(client) = &context.nats_rpc {
        return nats_transition_self(
            client,
            record,
            MachineSelfTransition::Activate { assigned_subnet },
        )
        .await;
    }

    #[cfg(test)]
    {
        let mut active_record = record.clone();
        active_record
            .apply_lifecycle_transition(ployz_model::MachineLifecycleTransition {
                goal: ployz_model::MachineLifecycleGoal::Activate { assigned_subnet },
                evidence: ployz_model::MachineTransitionEvidence::BootstrapActivation {
                    operation_id: None,
                },
                at_unix_secs: ployz_time::now_unix_secs(),
            })
            .map_err(|err| format!("activate joined machine: {err}"))?;
        return context
            .store
            .upsert_self_machine(&active_record)
            .await
            .map_err(|err| format!("persist active self-record: {err}"));
    }

    #[cfg(not(test))]
    {
        Err("NATS RPC client unavailable".into())
    }
}
