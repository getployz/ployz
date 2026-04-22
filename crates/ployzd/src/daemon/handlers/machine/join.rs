use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use ipnet::Ipv4Net;
use ployz_api::{
    CoordOp, DaemonPayload, DaemonRequest, DaemonResponse, InstallRuntimeTarget,
    InstallServiceMode, InstallSource, MachineAddOptions, MachineInstallOptions,
    MeshBootstrapRequest, MeshReadyPayload, MeshSelfRecordPayload, ResourceKey as ApiResourceKey,
};
use ployz_orchestrator::coordination::{
    PrepareVote, Reservation, ReservationId, ResourceKey, Vote, quorum_prepare,
};
use ployz_orchestrator::ipam::pick_candidate_subnet;
use ployz_orchestrator::machine_liveness::machine_is_fresh;
use ployz_orchestrator::mesh::tasks::PeerSyncCommand;
use ployz_sdk::Transport;
use ployz_store_api::{InviteStore, MachineStore};
use ployz_types::model::{
    JOIN_RESPONSE_PREFIX, JoinResponse, MachineId, MachineRecord, MachineStatus, OverlayIp,
    Participation,
};
use ployz_types::time::now_unix_secs;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
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
const PEER_RPC_TIMEOUT: Duration = Duration::from_secs(3);
const REMOTE_STATUS_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployz\" status >/dev/null";
const REMOTE_PLOYZ_VERSION_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployz\" --version";
const REMOTE_RPC_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployz\" rpc-stdio";
const SUBNET_RESERVATION_TTL_SECS: u64 = 30;
const MAX_SUBNET_ATTEMPTS: usize = 64;

#[derive(Debug, Clone)]
pub(super) struct BootstrapSubnetClaim {
    reservation: Reservation,
    subnet: Ipv4Net,
    quorum_peers: Vec<CoordinationPeer>,
    peer_rpc_port: u16,
}

#[derive(Debug, Clone)]
struct CoordinationPeer {
    machine_id: MachineId,
    overlay_ip: OverlayIp,
}

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
                        network_id: active.config.id.clone(),
                        cluster_cidr: active.config.cluster_cidr.clone(),
                        store: active.mesh.store.clone(),
                        reservations: self.reservations.clone(),
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

        let operation_store = self.machine_operation_store();
        let mut report = MachineAddReport::with_warnings(warnings);
        let mut tasks = JoinSet::new();

        for target in targets.iter().cloned() {
            tracing::info!(%target, "machine add issuing invite token");
            let (_token, invite) = match self.do_issue_invite_token(&running, INVITE_TTL_SECS).await
            {
                Ok(value) => value,
                Err(err) => {
                    report.push(MachineAddTargetResult::Failed {
                        target,
                        failure: MachineAddFailure::Preflight {
                            reason: format!("failed to issue invite token: {err}"),
                        },
                    });
                    continue;
                }
            };
            tracing::info!(%target, "machine add invite token issued");
            let target_machine_id = MachineId(target.clone());
            let subnet_claim = match self.reserve_machine_subnet(&target_machine_id).await {
                Ok(claim) => claim,
                Err(err) => {
                    report.push(MachineAddTargetResult::Failed {
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
                MachineAddStage::Preflight.to_string(),
                MachineOperationArtifacts {
                    invite_id: Some(invite.invite_id.clone()),
                    allocated_subnet: Some(subnet_claim.subnet.to_string()),
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
                    invite.invite_id,
                    subnet_claim,
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

    pub(crate) async fn handle_machine_enable(&self, target: &str) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "machine enable requires a running network",
                );
            }
        };
        let machine_id = MachineId(target.to_string());
        let Some(record) =
            (match super::list::find_machine_record(&active.mesh.store, &machine_id).await {
                Ok(record) => record,
                Err(err) => return self.err("LIST_FAILED", err),
            })
        else {
            return self.err("MACHINE_NOT_FOUND", format!("machine '{target}' not found"));
        };
        if record.participation != Participation::Disabled {
            return self.err(
                "MACHINE_NOT_DISABLED",
                format!("machine '{target}' is not disabled"),
            );
        }
        let peer_rpc_port = match self.peer_control_port() {
            Ok(port) => port,
            Err(error) => return self.err("CONTROL_TRANSPORT_FAILED", error.to_string()),
        };

        let context = MachineAddContext {
            network_name: active.config.name.0.clone(),
            network_id: active.config.id.clone(),
            cluster_cidr: active.config.cluster_cidr.clone(),
            store: active.mesh.store.clone(),
            reservations: self.reservations.clone(),
            peer_sync_tx: {
                let Some(peer_sync_tx) = active.mesh.peer_sync_sender() else {
                    return self.err("PEER_SYNC_UNAVAILABLE", "peer sync task is not running");
                };
                peer_sync_tx
            },
            ssh_options: SshOptions::default(),
            install: MachineInstallOptions::default(),
        };

        let subnet_claim = match self.reserve_machine_subnet(&machine_id).await {
            Ok(claim) => claim,
            Err(err) => return self.err("SUBNET_RESERVATION_FAILED", err),
        };

        if let Err(err) = self
            .set_local_participation_override(Some(Participation::Enabled))
            .await
        {
            let _ = release_reserved_subnet(&context, &subnet_claim).await;
            return self.err(
                "PARTICIPATION_OVERRIDE_FAILED",
                format!("failed to hold founder participation: {err}"),
            );
        }

        let result = self
            .handle_machine_enable_remote(
                &machine_id,
                &record,
                peer_rpc_port,
                &context,
                &subnet_claim,
            )
            .await;

        let _ = self.set_local_participation_override(None).await;
        result
    }

    async fn handle_machine_enable_remote(
        &self,
        machine_id: &MachineId,
        record: &MachineRecord,
        peer_rpc_port: u16,
        context: &MachineAddContext,
        subnet_claim: &BootstrapSubnetClaim,
    ) -> DaemonResponse {
        if let Err(err) = overlay_rpc_expect_ok(
            record.overlay_ip,
            peer_rpc_port,
            DaemonRequest::MeshPromote {
                assigned_subnet: subnet_claim.subnet,
            },
        )
        .await
        {
            let _ = release_reserved_subnet(context, subnet_claim).await;
            return self.err("REMOTE_ENABLE_FAILED", err);
        }

        let remote_record = match overlay_self_record(record, peer_rpc_port).await {
            Ok(record) => record,
            Err(err) => {
                let _ = release_reserved_subnet(context, subnet_claim).await;
                return self.err("SELF_RECORD_FAILED", err);
            }
        };
        if remote_record.id != *machine_id {
            let _ = release_reserved_subnet(context, subnet_claim).await;
            return self.err(
                "MACHINE_ID_MISMATCH",
                format!(
                    "remote machine id '{}' did not match enable target '{}'",
                    remote_record.id, machine_id
                ),
            );
        }

        if let Err(err) = upsert_transient_peer(&context.peer_sync_tx, remote_record).await {
            let _ = release_reserved_subnet(context, subnet_claim).await;
            return self.err("PEER_SYNC_UNAVAILABLE", err);
        }

        if let Err(err) = wait_for_overlay_ready(record, peer_rpc_port).await {
            let _ = release_reserved_subnet(context, subnet_claim).await;
            return self.err("REMOTE_READY_FAILED", err);
        }
        if let Err(err) = overlay_rpc_expect_ok(
            record.overlay_ip,
            peer_rpc_port,
            DaemonRequest::MeshSetParticipation {
                participation: Participation::Enabled,
            },
        )
        .await
        {
            let _ = release_reserved_subnet(context, subnet_claim).await;
            return self.err("REMOTE_ENABLE_FAILED", err);
        }

        let _ = release_reserved_subnet(context, subnet_claim).await;
        self.ok(format!(
            "machine enabled\n  machine: {}\n  subnet:  {}",
            machine_id, subnet_claim.subnet
        ))
    }

    pub(crate) async fn handle_machine_drain(&self, target: &str) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "machine drain requires a running network",
                );
            }
        };
        let machine_id = MachineId(target.to_string());
        let Some(record) =
            (match super::list::find_machine_record(&active.mesh.store, &machine_id).await {
                Ok(record) => record,
                Err(err) => return self.err("LIST_FAILED", err),
            })
        else {
            return self.err("MACHINE_NOT_FOUND", format!("machine '{target}' not found"));
        };
        if record.participation == Participation::Draining {
            return self.ok(format!("machine '{}' already draining", machine_id));
        }
        let peer_rpc_port = match self.peer_control_port() {
            Ok(port) => port,
            Err(error) => return self.err("CONTROL_TRANSPORT_FAILED", error.to_string()),
        };
        if let Err(err) = overlay_rpc_expect_ok(
            record.overlay_ip,
            peer_rpc_port,
            DaemonRequest::MeshSetParticipation {
                participation: Participation::Draining,
            },
        )
        .await
        {
            return self.err("REMOTE_DRAIN_FAILED", err);
        }
        self.ok(format!("machine '{}' draining", machine_id))
    }

    pub(crate) async fn handle_machine_disable(&self, target: &str, force: bool) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "machine disable requires a running network",
                );
            }
        };
        let machine_id = MachineId(target.to_string());
        let Some(record) =
            (match super::list::find_machine_record(&active.mesh.store, &machine_id).await {
                Ok(record) => record,
                Err(err) => return self.err("LIST_FAILED", err),
            })
        else {
            return self.err("MACHINE_NOT_FOUND", format!("machine '{target}' not found"));
        };
        if record.participation == Participation::Disabled && record.subnet.is_none() {
            return self.ok(format!("machine '{}' already disabled", machine_id));
        }
        let peer_rpc_port = match self.peer_control_port() {
            Ok(port) => port,
            Err(error) => return self.err("CONTROL_TRANSPORT_FAILED", error.to_string()),
        };
        if let Err(err) = self
            .set_local_participation_override(Some(Participation::Enabled))
            .await
        {
            return self.err(
                "PARTICIPATION_OVERRIDE_FAILED",
                format!("failed to hold founder participation: {err}"),
            );
        }
        if let Err(err) = overlay_rpc_expect_ok(
            record.overlay_ip,
            peer_rpc_port,
            DaemonRequest::MeshStandby { force },
        )
        .await
        {
            let _ = self.set_local_participation_override(None).await;
            return self.err("REMOTE_DISABLE_FAILED", err);
        }

        let _ = self.set_local_participation_override(None).await;
        self.ok(format!("machine '{}' disabled", machine_id))
    }

    pub(super) async fn reserve_machine_subnet(
        &self,
        owner: &MachineId,
    ) -> Result<BootstrapSubnetClaim, String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no running network".to_string())?;
        let peer_rpc_port = self
            .peer_control_port()
            .map_err(|error| error.to_string())?;
        let cluster: Ipv4Net = self
            .cluster_cidr
            .parse()
            .map_err(|err| format!("invalid cluster CIDR '{}': {err}", self.cluster_cidr))?;
        let now = now_unix_secs();
        let machines = active
            .mesh
            .store
            .list_machines()
            .await
            .map_err(|err| format!("list machines for subnet reservation: {err}"))?;
        let bias_seed = machine_bias_seed(owner);

        let mut taken = machines
            .iter()
            .filter_map(|machine| machine.subnet)
            .collect::<HashSet<_>>();
        taken.extend(self.reservations.active_subnets(now).await);
        let quorum_peers = coordination_peers(&machines, &self.identity.machine_id, now);

        for _ in 0..MAX_SUBNET_ATTEMPTS {
            let Some(candidate) =
                pick_candidate_subnet(cluster, self.subnet_prefix_len, &taken, bias_seed)
            else {
                return Err("no available subnets".into());
            };
            let reservation = Reservation {
                id: ReservationId::random(),
                key: ResourceKey::Subnet(candidate),
                owner: owner.clone(),
                nonce: ReservationId::random().0,
                expires_at: now.saturating_add(SUBNET_RESERVATION_TTL_SECS),
            };
            let committed_taken = machines
                .iter()
                .any(|machine| machine.subnet == Some(candidate));
            let local_vote = self
                .reservations
                .prepare(reservation.clone(), committed_taken, now)
                .await;
            match local_vote {
                Vote::Allow => {}
                Vote::Deny(_) => {
                    taken.insert(candidate);
                    continue;
                }
            }

            // Subnet claims use quorum only for the pending reservation step. Durability still
            // comes from the eventual MachineRecord self-publication on the target machine.
            let decision = quorum_prepare(&quorum_peers, PrepareVote::Allow, |peer| {
                let reservation = reservation.clone();
                async move { remote_coord_prepare(&peer, &reservation, peer_rpc_port).await }
            })
            .await;
            if decision.allowed {
                return Ok(BootstrapSubnetClaim {
                    reservation,
                    subnet: candidate,
                    quorum_peers,
                    peer_rpc_port,
                });
            }
            let _ = self
                .reservations
                .release(&reservation.key, &reservation.nonce, now_unix_secs())
                .await;
            if !decision.retry_could_succeed() {
                return Err(format!(
                    "failed to reach quorum for subnet reservation ({}/{})",
                    decision.votes_for, decision.votes_total
                ));
            }
            taken.insert(candidate);
        }

        Err("no available subnets".into())
    }
}

async fn run_machine_add_target(
    context: MachineAddContext,
    operation_store: MachineOperationStore,
    mut operation: MachineOperationRecord,
    target: String,
    invite_id: String,
    subnet_claim: BootstrapSubnetClaim,
) -> MachineAddTargetResult {
    let mut stage;
    let mut joiner_id = None;

    tracing::info!(%target, "machine add target: bootstrap starting");
    if let Err(err) =
        bootstrap_remote_machine(&target, &context.install, &context.ssh_options).await
    {
        let _ = release_reserved_subnet(&context, &subnet_claim).await;
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
        DaemonRequest::MeshBootstrap {
            request: match build_mesh_bootstrap_request(
                &context,
                subnet_claim.subnet,
                Some(target.clone()),
            )
            .await
            {
                Ok(request) => request,
                Err(err) => {
                    let _ = release_reserved_subnet(&context, &subnet_claim).await;
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
            },
        },
        &context.ssh_options,
    )
    .await
    {
        Ok(()) => {}
        Err(err) => {
            let _ = release_reserved_subnet(&context, &subnet_claim).await;
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
    let mut record = match remote_self_record(&target, &context.ssh_options).await {
        Ok(record) => record,
        Err(err) => {
            let _ = release_reserved_subnet(&context, &subnet_claim).await;
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
    record.control_target = Some(target.clone());
    stage = MachineAddStage::SelfRecorded;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, "machine add target: self-record complete");

    let machine_id = record.id.clone();
    if let Err(err) = persist_machine_control_target(&context, &machine_id, &target).await {
        let _ = release_reserved_subnet(&context, &subnet_claim).await;
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
    operation.artifacts.machine_id = Some(machine_id.clone());
    let _ = operation_store.save(&operation);
    joiner_id = Some(machine_id.clone());
    if let Err(err) = consume_invite(&context, &invite_id, &machine_id).await {
        let _ = release_reserved_subnet(&context, &subnet_claim).await;
        let _ = rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref()).await;
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
    tracing::info!(%target, joiner_id = %machine_id, "machine add target: transient peer install starting");
    if let Err(err) = upsert_transient_peer(&context.peer_sync_tx, record).await {
        let _ = release_reserved_subnet(&context, &subnet_claim).await;
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
        let _ = release_reserved_subnet(&context, &subnet_claim).await;
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
    let _ = release_reserved_subnet(&context, &subnet_claim).await;

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

async fn build_mesh_bootstrap_request(
    context: &MachineAddContext,
    assigned_subnet: Ipv4Net,
    self_control_target: Option<String>,
) -> Result<MeshBootstrapRequest, String> {
    let bootstrap_peers = context
        .store
        .list_machines()
        .await
        .map_err(|err| format!("list machines for bootstrap: {err}"))?
        .into_iter()
        .filter(|machine| !machine.endpoints.is_empty())
        .collect::<Vec<_>>();

    Ok(MeshBootstrapRequest {
        network_id: context.network_id.clone(),
        network_name: context.network_name.clone(),
        cluster_cidr: context.cluster_cidr.clone(),
        assigned_subnet,
        self_control_target,
        bootstrap_peers,
    })
}

async fn release_reserved_subnet(
    context: &MachineAddContext,
    subnet_claim: &BootstrapSubnetClaim,
) -> Result<(), String> {
    let now = now_unix_secs();
    let local_vote = context
        .reservations
        .release(
            &subnet_claim.reservation.key,
            &subnet_claim.reservation.nonce,
            now,
        )
        .await;
    if let Vote::Deny(conflict) = local_vote {
        tracing::warn!(
            ?conflict,
            subnet = %subnet_claim.subnet,
            "subnet reservation release denied locally"
        );
    }
    for peer in &subnet_claim.quorum_peers {
        if let Err(err) =
            remote_coord_release(peer, &subnet_claim.reservation, subnet_claim.peer_rpc_port).await
        {
            tracing::warn!(
                peer = %peer.machine_id,
                subnet = %subnet_claim.subnet,
                error = %err,
                "subnet reservation release fanout failed"
            );
        }
    }
    Ok(())
}

async fn consume_invite(
    context: &MachineAddContext,
    invite_id: &str,
    machine_id: &MachineId,
) -> Result<(), String> {
    context
        .store
        .redeem_invite(invite_id, machine_id, now_unix_secs())
        .await
        .map(|_| ())
        .map_err(|err| format!("consume invite: {err}"))
}

fn machine_bias_seed(machine_id: &MachineId) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    machine_id.hash(&mut hasher);
    hasher.finish()
}

fn coordination_peers(
    machines: &[MachineRecord],
    self_id: &MachineId,
    now: u64,
) -> Vec<CoordinationPeer> {
    machines
        .iter()
        .filter(|machine| machine.id != *self_id)
        .filter(|machine| machine.status != MachineStatus::Down)
        .filter(|machine| machine_is_fresh(machine, now))
        .filter(|machine| match machine.participation {
            Participation::Disabled => false,
            Participation::Enabled | Participation::Draining => true,
        })
        .filter_map(|machine| {
            Some(CoordinationPeer {
                machine_id: machine.id.clone(),
                overlay_ip: machine.overlay_ip,
            })
        })
        .collect()
}

async fn remote_coord_prepare(
    peer: &CoordinationPeer,
    reservation: &Reservation,
    peer_rpc_port: u16,
) -> PrepareVote {
    match overlay_rpc(
        peer.overlay_ip,
        peer_rpc_port,
        DaemonRequest::Coord {
            op: CoordOp::Prepare {
                id: reservation.id.0.clone(),
                key: api_resource_key(&reservation.key),
                owner: reservation.owner.clone(),
                nonce: reservation.nonce.clone(),
                ttl_secs: reservation.expires_at.saturating_sub(now_unix_secs()),
            },
        },
    )
    .await
    {
        Ok(response) if response.ok => PrepareVote::Allow,
        Ok(response) => {
            tracing::warn!(
                peer = %peer.machine_id,
                code = %response.code,
                message = %response.message,
                "subnet reservation prepare denied by peer"
            );
            classify_remote_prepare_denial(&response.message)
        }
        Err(err) => {
            tracing::warn!(peer = %peer.machine_id, error = %err, "subnet reservation prepare rpc failed");
            PrepareVote::TerminalDeny
        }
    }
}

fn classify_remote_prepare_denial(message: &str) -> PrepareVote {
    if message.contains("HeldBy") || message.contains("AlreadyCommitted") {
        return PrepareVote::RetryableDeny;
    }
    PrepareVote::TerminalDeny
}

async fn remote_coord_release(
    peer: &CoordinationPeer,
    reservation: &Reservation,
    peer_rpc_port: u16,
) -> Result<(), String> {
    let response = overlay_rpc(
        peer.overlay_ip,
        peer_rpc_port,
        DaemonRequest::Coord {
            op: CoordOp::Release {
                id: reservation.id.0.clone(),
                key: api_resource_key(&reservation.key),
                nonce: reservation.nonce.clone(),
            },
        },
    )
    .await?;
    if response.ok {
        return Ok(());
    }
    Err(remote_response_error(&response))
}

async fn persist_machine_control_target(
    context: &MachineAddContext,
    machine_id: &MachineId,
    control_target: &str,
) -> Result<(), String> {
    let Some(mut record) = super::list::find_machine_record(&context.store, machine_id).await?
    else {
        tracing::info!(
            machine_id = %machine_id,
            control_target,
            "machine record not visible in store yet; deferring control target persistence"
        );
        return Ok(());
    };
    record.control_target = Some(control_target.to_string());
    context
        .store
        .upsert_self_machine(&record)
        .await
        .map_err(|err| format!("persist control target: {err}"))
}

fn api_resource_key(key: &ResourceKey) -> ApiResourceKey {
    match key {
        ResourceKey::Subnet(subnet) => ApiResourceKey::Subnet(*subnet),
        ResourceKey::DeployNamespace(namespace) => {
            ApiResourceKey::DeployNamespace(namespace.clone())
        }
    }
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
            remote_rpc(
                target,
                DaemonRequest::MeshReady { json: false },
                ssh_options,
            ),
        )
        .await
        {
            Ok(Ok(response)) => match mesh_ready_payload(&response) {
                Ok(payload) => {
                    if remote_join_ready(&payload) {
                        tracing::debug!(%target, attempt, "remote mesh ready confirmed");
                        return Ok(());
                    }
                    tracing::debug!(%target, attempt, ?payload, "remote mesh not ready yet");
                    format!("mesh reported not ready yet: {}", response.message)
                }
                Err(err) => {
                    tracing::debug!(%target, attempt, error = %err, "remote readiness payload parse failed");
                    err
                }
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
    let response = remote_rpc(target, DaemonRequest::MeshSelfRecord, ssh_options).await?;
    if !response.ok {
        return Err(remote_response_error(&response));
    }
    match response.payload {
        Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload { record, .. })) => Ok(record),
        Some(payload) => Err(format!("unexpected self-record payload: {payload:?}")),
        None => decode_joiner_record(&response.message),
    }
}

fn mesh_ready_payload(response: &DaemonResponse) -> Result<MeshReadyPayload, String> {
    match &response.payload {
        Some(DaemonPayload::MeshReady(payload)) => Ok(payload.clone()),
        Some(payload) => Err(format!("unexpected readiness payload: {payload:?}")),
        None => parse_remote_ready_payload(&response.message),
    }
}

fn parse_remote_ready_payload(output: &str) -> Result<MeshReadyPayload, String> {
    if let Ok(payload) = serde_json::from_str::<MeshReadyPayload>(output) {
        return Ok(payload);
    }

    #[derive(serde::Deserialize)]
    struct RemoteReadyEnvelope {
        message: String,
    }

    let envelope = serde_json::from_str::<RemoteReadyEnvelope>(output)
        .map_err(|error| format!("failed to parse remote readiness envelope: {error}"))?;
    serde_json::from_str::<MeshReadyPayload>(&envelope.message)
        .map_err(|error| format!("failed to parse remote readiness message: {error}"))
}

fn remote_join_ready(payload: &MeshReadyPayload) -> bool {
    payload.ready
        || (payload.phase == "running" && payload.store_healthy && payload.heartbeat_started)
}

async fn overlay_rpc(
    overlay_ip: OverlayIp,
    peer_rpc_port: u16,
    request: DaemonRequest,
) -> Result<DaemonResponse, String> {
    let address = SocketAddr::new(IpAddr::V6(overlay_ip.0), peer_rpc_port);
    let stream = timeout(PEER_RPC_TIMEOUT, TcpStream::connect(address))
        .await
        .map_err(|_| {
            format!(
                "overlay rpc connect {address} timed out after {:?}",
                PEER_RPC_TIMEOUT
            )
        })?
        .map_err(|error| format!("overlay rpc connect {address}: {error}"))?;
    let (reader, mut writer) = stream.into_split();
    let mut line = serde_json::to_string(&request)
        .map_err(|error| format!("encode overlay rpc request: {error}"))?;
    line.push('\n');
    timeout(PEER_RPC_TIMEOUT, writer.write_all(line.as_bytes()))
        .await
        .map_err(|_| {
            format!(
                "overlay rpc write {address} timed out after {:?}",
                PEER_RPC_TIMEOUT
            )
        })?
        .map_err(|error| format!("overlay rpc write {address}: {error}"))?;
    timeout(PEER_RPC_TIMEOUT, writer.shutdown())
        .await
        .map_err(|_| {
            format!(
                "overlay rpc shutdown {address} timed out after {:?}",
                PEER_RPC_TIMEOUT
            )
        })?
        .map_err(|error| format!("overlay rpc shutdown {address}: {error}"))?;

    let mut response_line = String::new();
    let mut reader = BufReader::new(reader);
    timeout(PEER_RPC_TIMEOUT, reader.read_line(&mut response_line))
        .await
        .map_err(|_| {
            format!(
                "overlay rpc read {address} timed out after {:?}",
                PEER_RPC_TIMEOUT
            )
        })?
        .map_err(|error| format!("overlay rpc read {address}: {error}"))?;
    serde_json::from_str(&response_line)
        .map_err(|error| format!("decode overlay rpc response: {error}"))
}

async fn overlay_rpc_expect_ok(
    overlay_ip: OverlayIp,
    peer_rpc_port: u16,
    request: DaemonRequest,
) -> Result<(), String> {
    let response = overlay_rpc(overlay_ip, peer_rpc_port, request).await?;
    if response.ok {
        return Ok(());
    }
    Err(remote_response_error(&response))
}

async fn overlay_self_record(
    machine: &MachineRecord,
    peer_rpc_port: u16,
) -> Result<MachineRecord, String> {
    let response = overlay_rpc(
        machine.overlay_ip,
        peer_rpc_port,
        DaemonRequest::MeshSelfRecord,
    )
    .await?;
    if !response.ok {
        return Err(remote_response_error(&response));
    }
    match response.payload {
        Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload { record, .. })) => Ok(record),
        Some(payload) => Err(format!("unexpected self-record payload: {payload:?}")),
        None => decode_joiner_record(&response.message),
    }
}

async fn wait_for_overlay_ready(machine: &MachineRecord, peer_rpc_port: u16) -> Result<(), String> {
    let deadline = Instant::now() + REMOTE_READY_TIMEOUT;
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;
        let last_error = match timeout(
            REMOTE_READY_RPC_TIMEOUT,
            overlay_rpc(
                machine.overlay_ip,
                peer_rpc_port,
                DaemonRequest::MeshReady { json: false },
            ),
        )
        .await
        {
            Ok(Ok(response)) => match mesh_ready_payload(&response) {
                Ok(payload) => {
                    if remote_join_ready(&payload) {
                        tracing::debug!(machine = %machine.id, attempt, "overlay mesh ready confirmed");
                        return Ok(());
                    }
                    format!("mesh reported not ready yet: {}", response.message)
                }
                Err(err) => err,
            },
            Ok(Err(err)) => err,
            Err(_) => format!(
                "overlay readiness probe exceeded {:?}",
                REMOTE_READY_RPC_TIMEOUT
            ),
        };

        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for overlay mesh readiness after {:?}: {last_error}",
                REMOTE_READY_TIMEOUT,
            ));
        }

        sleep(REMOTE_READY_POLL_INTERVAL).await;
    }
}

async fn remote_rpc(
    target: &str,
    request: DaemonRequest,
    ssh_options: &SshOptions,
) -> Result<DaemonResponse, String> {
    let transport = ssh_stdio_transport(target, REMOTE_RPC_COMMAND, ssh_options);
    transport.request(request).await.map_err(|err| {
        format!(
            "remote rpc via '{}' failed: {err}",
            transport.command_display()
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

fn decode_joiner_record(output: &str) -> Result<MachineRecord, String> {
    let response_line = match output
        .lines()
        .find(|line| line.starts_with(JOIN_RESPONSE_PREFIX))
    {
        Some(line) => line,
        None => {
            return Err(format!(
                "self-record output missing {JOIN_RESPONSE_PREFIX} line\nhint: run `ployz mesh self-record` on the joiner and `ployz mesh accept <response>` on this machine"
            ));
        }
    };

    let join_response = JoinResponse::decode(response_line)
        .map_err(|err| format!("failed to decode join response: {err}"))?;
    Ok(join_response.into_seed_machine_record())
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
