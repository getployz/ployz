use std::path::{Path, PathBuf};

use crate::mesh_state::invite::parse_and_verify_invite_token;
use ipnet::Ipv4Net;
use ployz_api::{
    DaemonPayload, DaemonRequest, DaemonResponse, InstallRuntimeTarget, InstallServiceMode,
    InstallSource, MachineAddOptions, MachineInstallOptions, MeshReadyPayload,
    MeshSelfRecordPayload,
};
use ployz_orchestrator::ipam::Ipam;
use ployz_orchestrator::mesh::tasks::PeerSyncCommand;
use ployz_sdk::DaemonClient;
use ployz_store_api::{InviteStore, MachineStore};
use ployz_types::model::{MachineId, MachineRecord};
use ployz_types::time::now_unix_secs;
use tokio::task::JoinSet;
use tokio::time::{Duration, Instant, sleep, timeout};

use crate::daemon::DaemonState;
use crate::daemon::ssh::{
    EphemeralSshIdentityFile, SshOptions, run_ssh, run_ssh_with_stdin, ssh_stdio_transport,
};

use super::operations::{
    MachineOperationArtifacts, MachineOperationKind, MachineOperationRecord,
    MachineOperationStatus, MachineOperationStore,
};
use super::render::render_machine_add_report;
use super::types::{
    MachineAddContext, MachineAddFailure, MachineAddReport, MachineAddStage, MachineAddTargetResult,
};

const INVITE_TTL_SECS: u64 = 600;
const REMOTE_READY_TIMEOUT: Duration = Duration::from_secs(30);
const REMOTE_READY_POLL_INTERVAL: Duration = Duration::from_secs(1);
const REMOTE_READY_RPC_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_CLEANUP_RPC_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_STATUS_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployz\" status >/dev/null";
const REMOTE_PLOYZ_VERSION_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployz\" --version";
const REMOTE_RPC_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployz\" rpc-stdio";

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
                let Some(peer_sync_tx) = active.mesh.peer_sync_sender() else {
                    return self.err("PEER_SYNC_UNAVAILABLE", "peer sync task is not running");
                };
                (
                    active.config.clone(),
                    MachineAddContext {
                        network_name: active.config.name.0.clone(),
                        store: active.store.clone(),
                        peer_sync_tx,
                        ssh_options,
                        install: options.install.clone().unwrap_or_default(),
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

        let warnings = match self.degraded_mesh_warnings().await {
            Ok(warnings) => warnings,
            Err(err) => return self.err("LIST_FAILED", err),
        };
        tracing::info!(
            warning_count = warnings.len(),
            "machine add degraded-mesh check complete"
        );

        let allocated_subnets = match self.allocate_machine_subnets(targets.len()).await {
            Ok(subnets) => subnets,
            Err(err) => return self.err("SUBNET_EXHAUSTION", err),
        };
        tracing::info!(
            allocated_count = allocated_subnets.len(),
            "machine add subnet allocation complete"
        );

        let operation_store = self.machine_operation_store();
        let mut report = MachineAddReport::with_warnings(warnings);
        let mut tasks = JoinSet::new();

        for (target, allocated_subnet) in targets.iter().cloned().zip(allocated_subnets) {
            tracing::info!(%target, %allocated_subnet, "machine add issuing invite token");
            let token = match self
                .do_issue_invite_token(&running, INVITE_TTL_SECS, allocated_subnet)
                .await
            {
                Ok(token) => token,
                Err(err) => {
                    report.push(MachineAddTargetResult::Failed {
                        target,
                        failure: MachineAddFailure::Preflight {
                            reason: format!(
                                "failed to issue invite token for subnet {allocated_subnet}: {err}"
                            ),
                        },
                    });
                    continue;
                }
            };
            tracing::info!(%target, "machine add invite token issued");
            let invite = match parse_and_verify_invite_token(&token) {
                Ok(invite) => invite,
                Err(err) => {
                    report.push(MachineAddTargetResult::Failed {
                        target,
                        failure: MachineAddFailure::Preflight {
                            reason: format!(
                                "issued invite token could not be re-read for finalization: {err}"
                            ),
                        },
                    });
                    continue;
                }
            };

            let operation = match operation_store.begin(
                MachineOperationKind::Add,
                Some(context.network_name.clone()),
                vec![target.clone()],
                MachineAddStage::Preflight.to_string(),
                MachineOperationArtifacts {
                    invite_id: Some(invite.invite_id.clone()),
                    allocated_subnet: Some(allocated_subnet.to_string()),
                    uses_operation_identity: options.ssh_identity_private_key.is_some(),
                    ..MachineOperationArtifacts::default()
                },
            ) {
                Ok(operation) => operation,
                Err(err) => {
                    report.push(MachineAddTargetResult::Failed {
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
                    allocated_subnet,
                    token,
                    invite.invite_id,
                )
                .await
            });
        }

        while let Some(join_result) = tasks.join_next().await {
            match join_result {
                Ok(outcome) => report.push(outcome),
                Err(err) => report.push(MachineAddTargetResult::Failed {
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

    pub(crate) async fn allocate_machine_subnets(
        &self,
        count: usize,
    ) -> Result<Vec<Ipv4Net>, String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no running network".to_string())?;
        let machines = active
            .store
            .list_machines()
            .await
            .map_err(|err| format!("failed to list machines for subnet allocation: {err}"))?;

        let cluster: Ipv4Net = self
            .cluster_cidr
            .parse()
            .map_err(|err| format!("invalid cluster CIDR '{}': {err}", self.cluster_cidr))?;
        let allocated = machines.iter().filter_map(|machine| machine.subnet);
        let mut ipam = Ipam::with_allocated(cluster, self.subnet_prefix_len, allocated);
        let mut subnets = Vec::with_capacity(count);

        for _ in 0..count {
            let Some(subnet) = ipam.allocate() else {
                return Err("no available subnets".into());
            };
            subnets.push(subnet);
        }

        Ok(subnets)
    }
}

async fn run_machine_add_target(
    context: MachineAddContext,
    operation_store: MachineOperationStore,
    mut operation: MachineOperationRecord,
    target: String,
    allocated_subnet: Ipv4Net,
    token: String,
    invite_id: String,
) -> MachineAddTargetResult {
    let mut stage;
    let mut joiner_id = None;

    tracing::info!(%target, "machine add target: bootstrap starting");
    if let Err(err) =
        bootstrap_remote_machine(&target, &context.install, &context.ssh_options).await
    {
        let _ = operation_store.update_status(
            &mut operation,
            MachineOperationStatus::Failed,
            Some(err.clone()),
        );
        return MachineAddTargetResult::Failed {
            target,
            failure: MachineAddFailure::Preflight { reason: err },
        };
    }
    stage = MachineAddStage::Bootstrapped;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, "machine add target: bootstrap complete");

    tracing::info!(%target, "machine add target: remote join starting");
    match remote_rpc_expect_ok(
        &target,
        DaemonRequest::MeshJoin { token },
        &context.ssh_options,
    )
    .await
    {
        Ok(()) => {}
        Err(err) if err.contains("already exists") || err.contains("already running") => {
            tracing::info!(target, "remote already joined, continuing to self-record");
        }
        Err(err) => {
            let _ = operation_store.update_status(
                &mut operation,
                MachineOperationStatus::Failed,
                Some(err.clone()),
            );
            return MachineAddTargetResult::Failed {
                target,
                failure: MachineAddFailure::Join { reason: err },
            };
        }
    }
    stage = MachineAddStage::Joined;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, "machine add target: remote join complete");

    tracing::info!(%target, "machine add target: self-record starting");
    let record = match remote_self_record(&target, &context.ssh_options).await {
        Ok(record) => record,
        Err(err) => {
            let _ = rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref()).await;
            let _ = operation_store.update_status(
                &mut operation,
                MachineOperationStatus::Failed,
                Some(err.clone()),
            );
            return MachineAddTargetResult::Failed {
                target,
                failure: MachineAddFailure::SelfRecord { reason: err },
            };
        }
    };
    stage = MachineAddStage::SelfRecorded;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, "machine add target: self-record complete");

    if record.subnet != Some(allocated_subnet) {
        let actual_subnet = record
            .subnet
            .map(|subnet| subnet.to_string())
            .unwrap_or_else(|| "—".into());
        let _ = rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref()).await;
        let reason = format!(
            "joiner self-record subnet '{actual_subnet}' did not match allocated subnet '{allocated_subnet}'"
        );
        let _ = operation_store.update_status(
            &mut operation,
            MachineOperationStatus::Failed,
            Some(reason.clone()),
        );
        return MachineAddTargetResult::Failed {
            target,
            failure: MachineAddFailure::SelfRecord { reason },
        };
    }

    let machine_id = record.id.clone();
    operation.artifacts.machine_id = Some(machine_id.clone());
    let _ = operation_store.save(&operation);
    joiner_id = Some(machine_id.clone());
    tracing::info!(%target, joiner_id = %machine_id, "machine add target: transient peer install starting");
    if let Err(err) = upsert_transient_peer(&context.peer_sync_tx, record).await {
        let _ = rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref()).await;
        let _ = operation_store.update_status(
            &mut operation,
            MachineOperationStatus::Failed,
            Some(err.clone()),
        );
        return MachineAddTargetResult::Failed {
            target,
            failure: MachineAddFailure::Preflight { reason: err },
        };
    }
    stage = MachineAddStage::TransientPeerInstalled;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, joiner_id = %machine_id, "machine add target: transient peer installed");

    tracing::info!(%target, joiner_id = %machine_id, "machine add target: waiting for remote ready");
    if let Err(err) = wait_for_remote_ready(&target, &context.ssh_options).await {
        tracing::warn!(
            %target,
            joiner_id = %machine_id,
            error = %err,
            "machine add target: remote ready failed"
        );
        let _ = rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref()).await;
        let _ = operation_store.update_status(
            &mut operation,
            MachineOperationStatus::Failed,
            Some(err.clone()),
        );
        return MachineAddTargetResult::Failed {
            target,
            failure: MachineAddFailure::Ready { reason: err },
        };
    }
    stage = MachineAddStage::Ready;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, joiner_id = %machine_id, "machine add target: remote ready");

    tracing::info!(
        %target,
        joiner_id = %machine_id,
        invite_id,
        "machine add target: finalizing invite"
    );
    if let Err(err) = context
        .store
        .consume_invite(&invite_id, now_unix_secs())
        .await
    {
        tracing::warn!(
            %target,
            joiner_id = %machine_id,
            invite_id,
            error = %err,
            "machine add target: invite finalization failed"
        );
    } else {
        tracing::info!(
            %target,
            joiner_id = %machine_id,
            invite_id,
            "machine add target: invite finalized"
        );
    }

    let _ = operation_store.update_stage(&mut operation, MachineAddStage::Finalized.to_string());
    let _ = operation_store.update_status(&mut operation, MachineOperationStatus::Succeeded, None);
    tracing::info!(
        %target,
        joiner_id = %machine_id,
        "machine add target: awaiting self-publication"
    );
    MachineAddTargetResult::AwaitingSelfPublication {
        target,
        joiner_id: machine_id,
    }
}

async fn upsert_transient_peer(
    peer_sync_tx: &tokio::sync::mpsc::Sender<PeerSyncCommand>,
    record: MachineRecord,
) -> Result<(), String> {
    peer_sync_tx
        .send(PeerSyncCommand::UpsertTransient(record))
        .await
        .map_err(|err| format!("failed to install founder-local transient peer: {err}"))
}

pub(super) async fn remove_transient_peer(
    peer_sync_tx: &tokio::sync::mpsc::Sender<PeerSyncCommand>,
    machine_id: &MachineId,
) -> Result<(), String> {
    peer_sync_tx
        .send(PeerSyncCommand::RemoveTransient(machine_id.clone()))
        .await
        .map_err(|err| format!("failed to clear founder-local transient peer: {err}"))
}

async fn rollback_machine_add_target(
    context: &MachineAddContext,
    target: &str,
    stage: MachineAddStage,
    joiner_id: Option<&MachineId>,
) -> Result<(), String> {
    let mut errors = Vec::new();
    if matches!(
        stage,
        MachineAddStage::TransientPeerInstalled
            | MachineAddStage::Ready
            | MachineAddStage::Finalized
    ) && let Some(joiner_id) = joiner_id
        && let Err(err) = remove_transient_peer(&context.peer_sync_tx, joiner_id).await
    {
        errors.push(err);
    }
    if matches!(
        stage,
        MachineAddStage::Joined
            | MachineAddStage::SelfRecorded
            | MachineAddStage::TransientPeerInstalled
            | MachineAddStage::Ready
            | MachineAddStage::Finalized
    ) && let Err(err) =
        best_effort_remote_cleanup(target, &context.network_name, &context.ssh_options).await
    {
        errors.push(err);
    }

    if errors.is_empty() {
        return Ok(());
    }
    Err(errors.join("; "))
}

async fn wait_for_remote_ready(target: &str, ssh_options: &SshOptions) -> Result<(), String> {
    let deadline = Instant::now() + REMOTE_READY_TIMEOUT;
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;
        let last_error = match timeout(
            REMOTE_READY_RPC_TIMEOUT,
            remote_mesh_ready(target, ssh_options),
        )
        .await
        {
            Ok(Ok(payload)) => {
                let response_message = format!(
                    "ready={}, phase={}, store_healthy={}, sync_connected={}, heartbeat_started={}",
                    payload.ready,
                    payload.phase,
                    payload.store_healthy,
                    payload.sync_connected,
                    payload.heartbeat_started
                );
                if remote_join_ready(&payload) {
                    tracing::debug!(%target, attempt, "remote mesh ready confirmed");
                    return Ok(());
                }
                tracing::debug!(%target, attempt, ?payload, "remote mesh not ready yet");
                format!("mesh reported not ready yet: {response_message}")
            },
            Ok(Err(err)) => {
                tracing::debug!(%target, attempt, error = %err, "remote readiness rpc failed");
                err
            }
            Err(_) => {
                let err = format!(
                    "rpc readiness probe exceeded {:?}",
                    REMOTE_READY_RPC_TIMEOUT
                );
                tracing::debug!(%target, attempt, error = %err, "remote readiness rpc timed out");
                err
            }
        };

        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for remote mesh readiness after {:?}: {last_error}",
                REMOTE_READY_TIMEOUT,
            ));
        }

        sleep(REMOTE_READY_POLL_INTERVAL).await;
    }
}

async fn remote_self_record(
    target: &str,
    ssh_options: &SshOptions,
) -> Result<MachineRecord, String> {
    let transport = ssh_stdio_transport(target, REMOTE_RPC_COMMAND, ssh_options);
    let client = DaemonClient::new(transport);
    client
        .mesh_self_record()
        .await
        .map(|MeshSelfRecordPayload { record, .. }| record)
        .map_err(|error| {
            format!(
                "remote rpc via '{}' failed: {error}",
                client.transport().command_display()
            )
        })
}

fn remote_join_ready(payload: &MeshReadyPayload) -> bool {
    payload.ready
        || (payload.phase == "running" && payload.store_healthy && payload.heartbeat_started)
}

async fn remote_rpc(
    target: &str,
    request: DaemonRequest,
    ssh_options: &SshOptions,
) -> Result<DaemonResponse, String> {
    let transport = ssh_stdio_transport(target, REMOTE_RPC_COMMAND, ssh_options);
    let client = DaemonClient::new(transport);
    client.request(request).await.map_err(|err| {
        format!(
            "remote rpc via '{}' failed: {err}",
            client.transport().command_display()
        )
    })
}

async fn remote_mesh_ready(
    target: &str,
    ssh_options: &SshOptions,
) -> Result<MeshReadyPayload, String> {
    let transport = ssh_stdio_transport(target, REMOTE_RPC_COMMAND, ssh_options);
    let client = DaemonClient::new(transport);
    client.mesh_ready().await.map_err(|error| {
        format!(
            "remote rpc via '{}' failed: {error}",
            client.transport().command_display()
        )
    })
}

async fn remote_rpc_expect_ok(
    target: &str,
    request: DaemonRequest,
    ssh_options: &SshOptions,
) -> Result<(), String> {
    let response = remote_rpc(target, request, ssh_options).await?;
    if response.ok {
        return Ok(());
    }
    Err(remote_response_error(&response))
}

fn remote_response_error(response: &DaemonResponse) -> String {
    format!(
        "remote daemon error [{}]: {}",
        response.code, response.message
    )
}

pub(super) async fn best_effort_remote_cleanup(
    target: &str,
    network_name: &str,
    ssh_options: &SshOptions,
) -> Result<(), String> {
    tracing::debug!(%target, %network_name, "machine add cleanup: mesh down starting");
    let down_error = match timeout(
        REMOTE_CLEANUP_RPC_TIMEOUT,
        remote_rpc(target, DaemonRequest::MeshDown, ssh_options),
    )
    .await
    {
        Ok(Ok(response)) if response.ok => None,
        Ok(Ok(response)) => Some(remote_response_error(&response)),
        Ok(Err(err)) => Some(err),
        Err(_) => Some(format!(
            "mesh down rpc exceeded {:?}",
            REMOTE_CLEANUP_RPC_TIMEOUT
        )),
    };
    tracing::debug!(
        %target,
        %network_name,
        had_error = down_error.is_some(),
        "machine add cleanup: mesh down complete"
    );
    tracing::debug!(%target, %network_name, "machine add cleanup: mesh destroy starting");
    let destroy_error = match timeout(
        REMOTE_CLEANUP_RPC_TIMEOUT,
        remote_rpc(
            target,
            DaemonRequest::MeshDestroy {
                network: network_name.to_string(),
            },
            ssh_options,
        ),
    )
    .await
    {
        Ok(Ok(response)) if response.ok => None,
        Ok(Ok(response)) => Some(remote_response_error(&response)),
        Ok(Err(err)) => Some(err),
        Err(_) => Some(format!(
            "mesh destroy rpc exceeded {:?}",
            REMOTE_CLEANUP_RPC_TIMEOUT
        )),
    };
    tracing::debug!(
        %target,
        %network_name,
        had_error = destroy_error.is_some(),
        "machine add cleanup: mesh destroy complete"
    );

    let mut errors = Vec::new();
    if let Some(err) = down_error {
        errors.push(format!("mesh down: {err}"));
    }
    if let Some(err) = destroy_error {
        errors.push(format!("mesh destroy: {err}"));
    }

    if errors.is_empty() {
        return Ok(());
    }

    Err(errors.join("; "))
}

async fn bootstrap_remote_machine(
    target: &str,
    install: &MachineInstallOptions,
    ssh_options: &SshOptions,
) -> Result<(), String> {
    let local_version = local_ployz_version()?;
    if let Ok(remote_version) = run_ssh(target, REMOTE_PLOYZ_VERSION_COMMAND, ssh_options).await {
        if remote_version.trim() == local_version.trim() {
            tracing::info!(
                %target,
                version = remote_version.trim(),
                "machine add bootstrap: remote ployz version already matches, skipping install"
            );
            return run_ssh(target, REMOTE_STATUS_COMMAND, ssh_options)
                .await
                .map(|_| ());
        }
        tracing::info!(
            %target,
            local_version = local_version.trim(),
            remote_version = remote_version.trim(),
            "machine add bootstrap: remote ployz version mismatch, reinstalling"
        );
    } else {
        tracing::info!(%target, "machine add bootstrap: remote ployz missing, installing");
    }

    let installer_path = crate::install::find_installer_script()?;
    let installer = std::fs::read(&installer_path)
        .map_err(|error| format!("read installer '{}': {error}", installer_path.display()))?;
    let remote_command = format!("bash -s -- {}", install_script_args(install));
    run_ssh_with_stdin(target, &remote_command, &installer, ssh_options).await?;
    run_ssh(target, REMOTE_STATUS_COMMAND, ssh_options)
        .await
        .map(|_| ())
}

fn local_ployz_version() -> Result<String, String> {
    let ployz_path = local_ployz_path()?;
    let output = std::process::Command::new(&ployz_path)
        .arg("--version")
        .output()
        .map_err(|error| format!("run '{}' --version: {error}", ployz_path.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "'{}' --version failed (status: {}){}",
            ployz_path.display(),
            output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".into()),
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn local_ployz_path() -> Result<PathBuf, String> {
    let current_exe =
        std::env::current_exe().map_err(|error| format!("current_exe failed: {error}"))?;
    let candidates = [
        current_exe.with_file_name("ployz"),
        current_exe
            .parent()
            .map(|parent| parent.join("ployz"))
            .unwrap_or_else(|| PathBuf::from("ployz")),
        PathBuf::from("/usr/local/bin/ployz"),
        PathBuf::from("/usr/bin/ployz"),
    ];
    for candidate in candidates {
        if Path::new(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err("ployz binary not found next to current daemon".into())
}

fn install_script_args(install: &MachineInstallOptions) -> String {
    let mut args = vec!["install".to_string()];
    if let Some(runtime_target) = install.runtime_target {
        args.push("--runtime".into());
        args.push(
            match runtime_target {
                InstallRuntimeTarget::Docker => "docker",
                InstallRuntimeTarget::Host => "host",
            }
            .into(),
        );
    }
    if let Some(service_mode) = install.service_mode {
        args.push("--service-mode".into());
        args.push(
            match service_mode {
                InstallServiceMode::User => "user",
                InstallServiceMode::System => "system",
            }
            .into(),
        );
    }
    if let Some(source) = &install.source {
        args.push("--source".into());
        args.push(
            match source {
                InstallSource::Release => "release",
                InstallSource::Git => "git",
            }
            .into(),
        );
    }
    if let Some(version) = &install.version {
        args.push("--version".into());
        args.push(shell_quote(version));
    }
    if let Some(git_url) = &install.git_url {
        args.push("--git-url".into());
        args.push(shell_quote(git_url));
    }
    if let Some(git_ref) = &install.git_ref {
        args.push("--git-ref".into());
        args.push(shell_quote(git_ref));
    }

    args.join(" ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
