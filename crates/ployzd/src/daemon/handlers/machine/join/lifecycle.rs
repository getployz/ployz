use crate::daemon::DaemonState;
use crate::daemon::node_rpc::NatsMachineLifecycleRpcTransport;
use ployz_api::{DaemonResponse, MachineSelfTransition};
use ployz_model::{MachineId, MachineLifecycle, MachineMembership};
use ployz_node_runtime::{
    MACHINE_TRANSITION_RPC_POLICY, MachineLifecycleNodeClient, NodeRpcError, NodeRpcErrorKind,
};

use super::coordination::{BootstrapSubnetClaim, release_reserved_subnet};
use super::remote::{
    ExpectedMachineRecord, ExpectedSubnetState, log_nats_enable_rollback, nats_self_record,
    wait_for_machine_record, wait_for_nats_ready,
};

impl DaemonState {
    pub(crate) async fn handle_machine_activate(&self, target: &str) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "machine activate requires a running network",
                );
            }
        };
        let machine_id = match MachineId::try_new(target) {
            Ok(machine_id) => machine_id,
            Err(error) => return self.err("MACHINE_INVALID_TARGET", error),
        };
        let Some(record) =
            (match super::super::list::find_machine_record(&active.mesh.store, &machine_id).await {
                Ok(record) => record,
                Err(err) => return self.err("LIST_FAILED", err),
            })
        else {
            return self.err("MACHINE_NOT_FOUND", format!("machine '{target}' not found"));
        };
        if record.lifecycle != MachineLifecycle::Standby {
            return self.err(
                "INVALID_TRANSITION",
                format!("machine '{target}' is not standby"),
            );
        }
        let nats_client = match self.nats_node_rpc_client().await {
            Ok(client) => client.with_policy(ployz_nats::RpcPolicy {
                timeout: MACHINE_TRANSITION_RPC_POLICY.timeout,
            }),
            Err(error) => return self.err("NATS_RPC_UNAVAILABLE", error),
        };

        let subnet_claim = match self.reserve_machine_subnet(&machine_id).await {
            Ok(claim) => claim,
            Err(err) => return self.err("SUBNET_RESERVATION_FAILED", err),
        };
        let assigned_subnet = subnet_claim.subnet();

        let result = self
            .handle_machine_activate_remote(&machine_id, &record, &nats_client, subnet_claim)
            .await;

        if result.is_ok() {
            match wait_for_machine_record(
                &active.mesh.store,
                &machine_id,
                ExpectedMachineRecord::new(MachineLifecycle::Active, ExpectedSubnetState::Present),
            )
            .await
            {
                Ok(()) => match super::coordination::assert_subnet_unique(
                    &active.mesh.store,
                    &machine_id,
                    assigned_subnet,
                )
                .await
                {
                    Ok(()) => result,
                    Err(err) => self.err("SUBNET_UNIQUENESS_FAILED", err),
                },
                Err(err) => self.err("MACHINE_ENABLE_SYNC_FAILED", err),
            }
        } else {
            result
        }
    }

    async fn handle_machine_activate_remote(
        &self,
        machine_id: &MachineId,
        record: &MachineMembership,
        nats_client: &ployz_nats::NatsNodeRpcClient,
        subnet_claim: BootstrapSubnetClaim,
    ) -> DaemonResponse {
        let assigned_subnet = subnet_claim.subnet();
        if let Err(err) = request_remote_machine_transition(
            nats_client,
            &record.id,
            MachineSelfTransition::Activate { assigned_subnet },
        )
        .await
        {
            let _ = release_reserved_subnet(subnet_claim).await;
            return self.err("REMOTE_ACTIVATE_FAILED", remote_node_rpc_error(&err));
        }

        let remote_record = match nats_self_record(nats_client, record).await {
            Ok(record) => record,
            Err(err) => {
                log_nats_enable_rollback(nats_client, record, &err).await;
                let _ = release_reserved_subnet(subnet_claim).await;
                return self.err("SELF_RECORD_FAILED", err);
            }
        };
        if remote_record.id != *machine_id {
            let mismatch = format!(
                "remote machine id '{}' did not match enable target '{}'",
                remote_record.id, machine_id
            );
            log_nats_enable_rollback(nats_client, record, &mismatch).await;
            let _ = release_reserved_subnet(subnet_claim).await;
            return self.err("MACHINE_ID_MISMATCH", mismatch);
        }

        if let Err(err) = wait_for_nats_ready(nats_client, record).await {
            log_nats_enable_rollback(nats_client, record, &err).await;
            let _ = release_reserved_subnet(subnet_claim).await;
            return self.err("REMOTE_READY_FAILED", err);
        }

        let _ = release_reserved_subnet(subnet_claim).await;
        self.ok(format!(
            "machine activated\n  machine: {}\n  subnet:  {}",
            machine_id, assigned_subnet
        ))
    }

    pub(crate) async fn handle_machine_drain(&mut self, target: &str) -> DaemonResponse {
        let machine_id = match MachineId::try_new(target) {
            Ok(machine_id) => machine_id,
            Err(error) => return self.err("MACHINE_DRAIN_INVALID_TARGET", error),
        };
        if machine_id == self.identity.machine_id {
            return self
                .handle_machine_transition_self(MachineSelfTransition::Drain)
                .await;
        }
        self.handle_remote_machine_drain(target).await
    }

    pub(crate) async fn handle_remote_machine_drain(&self, target: &str) -> DaemonResponse {
        let machine_id = match MachineId::try_new(target) {
            Ok(machine_id) => machine_id,
            Err(error) => return self.err("MACHINE_DRAIN_INVALID_TARGET", error),
        };
        if machine_id == self.identity.machine_id {
            return self.err(
                "LOCAL_DRAIN_REQUIRES_EXCLUSIVE_LANE",
                "local machine drain must run on the exclusive lane",
            );
        }
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "machine drain requires a running network",
                );
            }
        };
        let Some(record) =
            (match super::super::list::find_machine_record(&active.mesh.store, &machine_id).await {
                Ok(record) => record,
                Err(err) => return self.err("LIST_FAILED", err),
            })
        else {
            return self.err("MACHINE_NOT_FOUND", format!("machine '{target}' not found"));
        };
        if record.lifecycle == MachineLifecycle::Draining {
            return self.ok(format!("machine '{}' already draining", machine_id));
        }
        let nats_client = match self.nats_node_rpc_client().await {
            Ok(client) => client.with_policy(ployz_nats::RpcPolicy {
                timeout: MACHINE_TRANSITION_RPC_POLICY.timeout,
            }),
            Err(error) => return self.err("NATS_RPC_UNAVAILABLE", error),
        };
        let transition = request_remote_machine_transition(
            &nats_client,
            &record.id,
            MachineSelfTransition::Drain,
        )
        .await;
        match transition {
            Ok(()) => {}
            Err(err) if err.kind == NodeRpcErrorKind::Remote => {
                return self.err("REMOTE_DRAIN_FAILED", remote_node_rpc_error(&err));
            }
            Err(err) => {
                let record_wait = wait_for_machine_record(
                    &active.mesh.store,
                    &machine_id,
                    ExpectedMachineRecord::new(
                        MachineLifecycle::Draining,
                        ExpectedSubnetState::Present,
                    ),
                )
                .await;
                match record_wait {
                    Ok(()) => {
                        tracing::warn!(
                            machine = %machine_id,
                            error = %err,
                            "machine drain NATS reply failed after observed machine record reached expected value"
                        );
                        return self.ok(format!("machine '{}' draining", machine_id));
                    }
                    Err(record_err) => {
                        return self.err(
                            "REMOTE_DRAIN_FAILED",
                            format!(
                                "{}; observed machine record did not confirm draining: {record_err}",
                                remote_node_rpc_error(&err)
                            ),
                        );
                    }
                }
            }
        }
        match wait_for_machine_record(
            &active.mesh.store,
            &machine_id,
            ExpectedMachineRecord::new(MachineLifecycle::Draining, ExpectedSubnetState::Present),
        )
        .await
        {
            Ok(()) => self.ok(format!("machine '{}' draining", machine_id)),
            Err(err) => self.err("MACHINE_DRAIN_SYNC_FAILED", err),
        }
    }

    pub(crate) async fn handle_machine_standby(
        &mut self,
        target: &str,
        force: bool,
    ) -> DaemonResponse {
        let machine_id = match MachineId::try_new(target) {
            Ok(machine_id) => machine_id,
            Err(error) => return self.err("MACHINE_STANDBY_INVALID_TARGET", error),
        };
        if machine_id == self.identity.machine_id {
            return self
                .handle_machine_transition_self(MachineSelfTransition::Standby { force })
                .await;
        }
        self.handle_remote_machine_standby(target, force).await
    }

    pub(crate) async fn handle_remote_machine_standby(
        &self,
        target: &str,
        force: bool,
    ) -> DaemonResponse {
        let machine_id = match MachineId::try_new(target) {
            Ok(machine_id) => machine_id,
            Err(error) => return self.err("MACHINE_STANDBY_INVALID_TARGET", error),
        };
        if machine_id == self.identity.machine_id {
            return self.err(
                "LOCAL_STANDBY_REQUIRES_EXCLUSIVE_LANE",
                "local machine standby must run on the exclusive lane",
            );
        }
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "machine standby requires a running network",
                );
            }
        };
        let Some(record) =
            (match super::super::list::find_machine_record(&active.mesh.store, &machine_id).await {
                Ok(record) => record,
                Err(err) => return self.err("LIST_FAILED", err),
            })
        else {
            return self.err("MACHINE_NOT_FOUND", format!("machine '{target}' not found"));
        };
        if record.lifecycle == MachineLifecycle::Standby && record.subnet.is_none() {
            return self.ok(format!("machine '{}' already standby", machine_id));
        }
        let nats_client = match self.nats_node_rpc_client().await {
            Ok(client) => client.with_policy(ployz_nats::RpcPolicy {
                timeout: MACHINE_TRANSITION_RPC_POLICY.timeout,
            }),
            Err(error) => return self.err("NATS_RPC_UNAVAILABLE", error),
        };
        let transition = request_remote_machine_transition(
            &nats_client,
            &record.id,
            MachineSelfTransition::Standby { force },
        )
        .await;
        match transition {
            Ok(()) => {}
            Err(err) if err.kind == NodeRpcErrorKind::Remote => {
                return self.err("REMOTE_STANDBY_FAILED", remote_node_rpc_error(&err));
            }
            Err(err) => {
                let record_wait = wait_for_machine_record(
                    &active.mesh.store,
                    &machine_id,
                    ExpectedMachineRecord::new(
                        MachineLifecycle::Standby,
                        ExpectedSubnetState::Absent,
                    ),
                )
                .await;
                match record_wait {
                    Ok(()) => {
                        tracing::warn!(
                            machine = %machine_id,
                            error = %err,
                            "machine standby NATS reply failed after observed machine record reached expected value"
                        );
                        return self.ok(format!("machine '{}' standby", machine_id));
                    }
                    Err(record_err) => {
                        return self.err(
                            "REMOTE_STANDBY_FAILED",
                            format!(
                                "{}; observed machine record did not confirm standby: {record_err}",
                                remote_node_rpc_error(&err)
                            ),
                        );
                    }
                }
            }
        }

        match wait_for_machine_record(
            &active.mesh.store,
            &machine_id,
            ExpectedMachineRecord::new(MachineLifecycle::Standby, ExpectedSubnetState::Absent),
        )
        .await
        {
            Ok(()) => self.ok(format!("machine '{}' standby", machine_id)),
            Err(err) => self.err("MACHINE_STANDBY_SYNC_FAILED", err),
        }
    }
}

async fn request_remote_machine_transition(
    client: &ployz_nats::NatsNodeRpcClient,
    machine_id: &MachineId,
    transition: MachineSelfTransition,
) -> Result<(), NodeRpcError> {
    MachineLifecycleNodeClient::new(NatsMachineLifecycleRpcTransport::new(client.clone()))
        .transition_self(machine_id, transition)
        .await
}

fn remote_node_rpc_error(error: &NodeRpcError) -> String {
    if error.kind == NodeRpcErrorKind::Remote {
        return format!("remote daemon error [{}]: {}", error.code, error.message);
    }
    error.to_string()
}
