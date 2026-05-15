use crate::daemon::DaemonState;
use crate::daemon::ssh::EphemeralSshIdentityFile;
use ployz_api::{DaemonPayload, DaemonResponse, MachineAddOptions};
use ployz_model::MachineId;
use ployz_node_runtime::MACHINE_TRANSITION_RPC_POLICY;
use tokio::task::JoinSet;

use super::super::operations::{MachineOperationArtifacts, MachineOperationKind};
use super::super::render::render_machine_add_report;
use super::super::types;
use super::super::types::{MachineAddContext, MachineAddFailure, MachineAddReport};
use super::coordination::release_reserved_subnet;
use super::target::run_machine_add_target;

const INVITE_TTL_SECS: u64 = 600;

impl DaemonState {
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
                        Ok(client) => Some(client.with_policy(ployz_nats::RpcPolicy {
                            timeout: MACHINE_TRANSITION_RPC_POLICY.timeout,
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
                    report.push(types::MachineAddTargetResult::Failed {
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
                    report.push(types::MachineAddTargetResult::Failed {
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
                types::MachineAddStage::Preflight.to_string(),
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
                    report.push(types::MachineAddTargetResult::Failed {
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
                Err(err) => report.push(types::MachineAddTargetResult::Failed {
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
}
