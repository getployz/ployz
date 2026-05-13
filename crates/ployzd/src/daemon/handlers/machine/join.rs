mod bootstrap;
mod coordination;
pub(in crate::daemon::handlers::machine) mod remote;
pub(super) mod rollback;
mod target;

pub(in crate::daemon::handlers::machine) use self::coordination::release_reserved_subnet;

use ployz_api::{
    DaemonPayload, DaemonRequest, DaemonResponse, MachineAddOptions, MachineInstallOptions,
    MachineSelfTransition,
};
use ployz_model::{MachineId, MachineLifecycle, MachineMembership};
use ployz_node_api::NodeRequest;
use tokio::task::JoinSet;

use crate::daemon::DaemonState;
use crate::daemon::ssh::{EphemeralSshIdentityFile, SshOptions};

use self::bootstrap::bootstrap_remote_machine;
use self::coordination::BootstrapSubnetClaim;
use self::remote::{
    ExpectedMachineRecord, ExpectedSubnetState, log_nats_enable_rollback, nats_rpc_expect_ok,
    nats_self_record, remote_response_error, remote_rpc_expect_ok, wait_for_machine_record,
    wait_for_nats_ready,
};
use self::target::run_machine_add_target;
use super::operations::{MachineOperationArtifacts, MachineOperationKind, MachineOperationStatus};
use super::render::render_machine_add_report;
use super::types::{MachineAddContext, MachineAddFailure, MachineAddReport};
use ployz_nats::{NodeCommandSubject, RpcPolicy};
use std::time::Duration;

const INVITE_TTL_SECS: u64 = 600;
const MACHINE_TRANSITION_RPC_TIMEOUT: Duration = Duration::from_secs(120);

impl DaemonState {
    pub(crate) async fn handle_machine_init(
        &self,
        target: &str,
        network: &str,
        install: &MachineInstallOptions,
    ) -> DaemonResponse {
        if self.active.is_some() {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                "machine init requires no local running network; switch context or run `mesh down` first",
            );
        }

        let operation_store = self.machine_operation_store();
        let mut operation = match operation_store.begin(
            MachineOperationKind::Init,
            Some(network.to_string()),
            vec![target.to_string()],
            "bootstrapping",
            MachineOperationArtifacts::default(),
        ) {
            Ok(operation) => operation,
            Err(err) => return self.err("MACHINE_OPERATION_START_FAILED", err),
        };

        if let Err(err) = bootstrap_remote_machine(target, install, &SshOptions::default()).await {
            let _ = operation_store.update_status(
                &mut operation,
                MachineOperationStatus::Failed,
                Some(err.clone()),
            );
            return self.err("SSH_BOOTSTRAP_FAILED", err);
        }

        if let Err(err) = operation_store.update_stage(&mut operation, "remote-init") {
            tracing::warn!(error = %err, operation_id = %operation.id, "machine init: failed to persist operation stage");
        }

        if let Err(err) = remote_rpc_expect_ok(
            target,
            DaemonRequest::MeshInit {
                network: network.to_string(),
            },
            &SshOptions::default(),
        )
        .await
        {
            let _ = operation_store.update_status(
                &mut operation,
                MachineOperationStatus::Failed,
                Some(err.clone()),
            );
            return self.err("REMOTE_INIT_FAILED", err);
        }

        let _ = operation_store.update_stage(&mut operation, "complete");
        let _ =
            operation_store.update_status(&mut operation, MachineOperationStatus::Succeeded, None);
        self.ok(format!(
            "remote founder initialized\n  target:  {target}\n  network: {network}"
        ))
    }

    pub(crate) async fn handle_machine_add(
        &self,
        targets: &[String],
        options: &MachineAddOptions,
    ) -> DaemonResponse {
        tracing::info!(target_count = targets.len(), "machine add requested");
        if targets.is_empty() {
            return self.err(
                "INVALID_ARGUMENT",
                "machine add requires at least one target",
            );
        }

        let identity_file = match options.ssh_identity_private_key.as_deref() {
            Some(private_key) => match EphemeralSshIdentityFile::write(private_key) {
                Ok(identity_file) => Some(identity_file),
                Err(err) => {
                    return self.err("INVALID_IDENTITY", err);
                }
            },
            None => None,
        };
        let ssh_options = identity_file
            .as_ref()
            .map(EphemeralSshIdentityFile::ssh_options)
            .unwrap_or_default();
        let (running, context) = match self.active.as_ref() {
            Some(active) => {
                let nats_rpc = if self.runtime_is_memory_test() {
                    None
                } else {
                    match self.nats_node_rpc_client().await {
                        Ok(client) => Some(client.with_policy(RpcPolicy {
                            timeout: MACHINE_TRANSITION_RPC_TIMEOUT,
                        })),
                        Err(error) => return self.err("NATS_RPC_UNAVAILABLE", error),
                    }
                };
                (
                    active.config.clone(),
                    MachineAddContext {
                        network_name: active.config.name.0.clone(),
                        network_dir: self.network_dir(&active.config.name.0),
                        network_id: active.config.id.clone(),
                        local_machine_id: self.identity.machine_id.clone(),
                        cluster_cidr: active.config.cluster_cidr.clone(),
                        store: active.mesh.store.clone(),
                        nats_rpc,
                        ssh_options,
                        install: options.install.clone().unwrap_or_default(),
                        remote_ready_wait_policy: {
                            #[cfg(test)]
                            {
                                self.machine_add_remote_ready_wait_policy
                            }
                            #[cfg(not(test))]
                            {
                                None
                            }
                        },
                    },
                )
            }
            None => {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "machine add requires a running network on this daemon",
                );
            }
        };

        let operation_store = self.machine_operation_store();
        let mut report = MachineAddReport::with_warnings(Vec::new());
        let mut tasks = JoinSet::new();

        for target in targets.iter().cloned() {
            tracing::info!(%target, "machine add issuing invite token");
            let (_token, invite) = match self.do_issue_invite_token(&running, INVITE_TTL_SECS).await
            {
                Ok(value) => value,
                Err(err) => {
                    report.push(super::types::MachineAddTargetResult::Failed {
                        target,
                        failure: MachineAddFailure::Preflight {
                            reason: format!("failed to issue invite token: {err}"),
                        },
                    });
                    continue;
                }
            };
            tracing::info!(%target, "machine add invite token issued");
            let target_machine_id = MachineId::new(target.clone());
            let subnet_claim = match self.reserve_machine_subnet(&target_machine_id).await {
                Ok(claim) => claim,
                Err(err) => {
                    report.push(super::types::MachineAddTargetResult::Failed {
                        target,
                        failure: MachineAddFailure::Preflight {
                            reason: format!("failed to reserve subnet: {err}"),
                        },
                    });
                    continue;
                }
            };

            let operation = match operation_store.begin(
                MachineOperationKind::Add,
                Some(context.network_name.clone()),
                vec![target.clone()],
                super::types::MachineAddStage::Preflight.to_string(),
                MachineOperationArtifacts {
                    invite_id: Some(invite.invite_id.clone()),
                    allocated_subnet: Some(subnet_claim.subnet().to_string()),
                    uses_operation_identity: options.ssh_identity_private_key.is_some(),
                    ..MachineOperationArtifacts::default()
                },
            ) {
                Ok(operation) => operation,
                Err(err) => {
                    if let Err(release_err) = release_reserved_subnet(subnet_claim).await {
                        tracing::warn!(
                            target = %target,
                            error = %release_err,
                            "machine add: failed to release reserved subnet after operation start failure"
                        );
                    }
                    report.push(super::types::MachineAddTargetResult::Failed {
                        target,
                        failure: MachineAddFailure::Preflight { reason: err },
                    });
                    continue;
                }
            };

            let task_context = context.clone();
            let task_operation_store = operation_store.clone();
            tasks.spawn(async move {
                run_machine_add_target(
                    task_context,
                    task_operation_store,
                    operation,
                    target,
                    invite.invite_id,
                    subnet_claim,
                )
                .await
            });
        }

        while let Some(join_result) = tasks.join_next().await {
            match join_result {
                Ok(outcome) => report.push(outcome),
                Err(err) => report.push(super::types::MachineAddTargetResult::Failed {
                    target: "task".into(),
                    failure: MachineAddFailure::Preflight {
                        reason: format!("task join failure: {err}"),
                    },
                }),
            }
        }

        let payload = report.payload();
        let message = render_machine_add_report(&report);
        if report.has_failures() {
            return self.err_with_payload(
                "MACHINE_ADD_FAILED",
                message,
                Some(DaemonPayload::MachineAdd(payload)),
            );
        }

        self.ok_with_payload(message, Some(DaemonPayload::MachineAdd(payload)))
    }

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
            (match super::list::find_machine_record(&active.mesh.store, &machine_id).await {
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
            Ok(client) => client.with_policy(RpcPolicy {
                timeout: MACHINE_TRANSITION_RPC_TIMEOUT,
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
                Ok(()) => match self::coordination::assert_subnet_unique(
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
        if let Err(err) = nats_rpc_expect_ok(
            nats_client,
            NodeCommandSubject::machine_transition_self(&record.id),
            NodeRequest::MachineTransitionSelf {
                transition: MachineSelfTransition::Activate { assigned_subnet },
            },
        )
        .await
        {
            let _ = release_reserved_subnet(subnet_claim).await;
            return self.err("REMOTE_ACTIVATE_FAILED", err);
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
            (match super::list::find_machine_record(&active.mesh.store, &machine_id).await {
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
            Ok(client) => client.with_policy(RpcPolicy {
                timeout: MACHINE_TRANSITION_RPC_TIMEOUT,
            }),
            Err(error) => return self.err("NATS_RPC_UNAVAILABLE", error),
        };
        let transition = nats_client
            .request(
                NodeCommandSubject::machine_transition_self(&record.id),
                &NodeRequest::MachineTransitionSelf {
                    transition: MachineSelfTransition::Drain,
                },
            )
            .await;
        match transition {
            Ok(response) if response.is_ok() => {}
            Ok(response) => {
                return self.err("REMOTE_DRAIN_FAILED", remote_response_error(&response));
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
                            format!("{err}; observed machine record did not confirm draining: {record_err}"),
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
            (match super::list::find_machine_record(&active.mesh.store, &machine_id).await {
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
            Ok(client) => client.with_policy(RpcPolicy {
                timeout: MACHINE_TRANSITION_RPC_TIMEOUT,
            }),
            Err(error) => return self.err("NATS_RPC_UNAVAILABLE", error),
        };
        let transition = nats_client
            .request(
                NodeCommandSubject::machine_transition_self(&record.id),
                &NodeRequest::MachineTransitionSelf {
                    transition: MachineSelfTransition::Standby { force },
                },
            )
            .await;
        match transition {
            Ok(response) if response.is_ok() => {}
            Ok(response) => {
                return self.err("REMOTE_STANDBY_FAILED", remote_response_error(&response));
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
                            format!("{err}; observed machine record did not confirm standby: {record_err}"),
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
