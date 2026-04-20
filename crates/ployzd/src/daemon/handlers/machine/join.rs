use std::path::{Path, PathBuf};

use crate::coordination::fanout::{
    FanOutTarget, accepted_targets, fanout_abort, fanout_commit, fanout_prepare,
};
use crate::daemon::handlers::mesh;
use crate::mesh_state::invite::{InviteClaims, parse_and_verify_invite_token};
use ipnet::Ipv4Net;
use ployz_api::{
    CoordinationAbortRequest, CoordinationCommitPayload, CoordinationCommitRequest,
    CoordinationOperation, CoordinationPreparePayload, CoordinationPrepareRequest, DaemonPayload,
    DaemonRequest, DaemonResponse, MachineAddOptions, MachineInstallOptions, MembershipAttestation,
};
use ployz_orchestrator::ipam::Ipam;
use ployz_orchestrator::mesh::tasks::PeerSyncCommand;
use ployz_runtime_api::WireGuardDevice;
use ployz_sdk::{DaemonClient, TcpTransport};
use ployz_store_api::MachineStore;
use ployz_types::model::{MachineId, MachineRecord, Phase};
use ployz_types::time::now_unix_secs;
use std::net::{IpAddr, SocketAddr};
use tokio::task::JoinSet;
use tokio::time::{Duration, timeout};

use crate::daemon::DaemonState;
use crate::daemon::ssh::{
    EphemeralSshIdentityFile, SshOptions, run_ssh, run_ssh_with_stdin, ssh_stdio_transport,
};
use std::collections::HashSet;

use super::operations::{
    MachineOperationArtifacts, MachineOperationKind, MachineOperationRecord,
    MachineOperationStatus, MachineOperationStore,
};
use super::render::render_machine_add_report;
use super::types::{
    MachineAddContext, MachineAddFailure, MachineAddReport, MachineAddStage, MachineAddTargetResult,
};

const INVITE_TTL_SECS: u64 = 600;
const RPC_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const REMOTE_CLEANUP_RPC_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_JOIN_VERIFY_TIMEOUT: Duration = Duration::from_secs(60);
const WAIT_NODE_PHASE_RPC_GRACE: Duration = Duration::from_secs(1);
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
                        peer_sync_tx,
                        ssh_options,
                        install: options.install.clone().unwrap_or_default(),
                        machine_store: active.store.machine(),
                        membership_store: active.store.membership(),
                        rpc_port: self.coordination_rpc_port,
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

        let allocated_subnets = match self
            .allocate_machine_subnets_with_coordination(targets)
            .await
        {
            Ok(subnets) => subnets,
            Err(err) => return self.err("SUBNET_EXHAUSTION", err),
        };
        tracing::info!(
            allocated_count = allocated_subnets.len(),
            "machine add subnet allocation complete"
        );

        let operation_store = self.machine_operation_store();
        let mut report = MachineAddReport::default();
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
                    invite,
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

    pub(crate) async fn handle_machine_admit(&self, id: &str) -> DaemonResponse {
        let Some(active) = self.active.as_ref() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };

        let machine_id = MachineId(id.to_string());
        let machine_store = active.store.machine();
        let record =
            match super::list::find_machine_record(machine_store.as_ref(), &machine_id).await {
                Ok(Some(record)) => record,
                Ok(None) => {
                    return self.err("MACHINE_NOT_FOUND", format!("machine '{id}' not found"));
                }
                Err(err) => {
                    return self.err("LIST_FAILED", format!("failed to read machines: {err}"));
                }
            };

        if record.admitted {
            return self.ok(format!("machine '{id}' already admitted"));
        }

        if machine_id == self.identity.machine_id {
            let phase = active.mesh.ready_status().await.phase;
            if phase != Phase::Running {
                return self.err(
                    "MACHINE_NOT_RUNNING",
                    format!("local machine '{id}' is in phase '{phase}'"),
                );
            }
        } else if let Err(err) =
            wait_for_remote_machine_phase(&record, self.coordination_rpc_port, Phase::Running).await
        {
            return self.err("MACHINE_NOT_RUNNING", err);
        }

        match admit_machine_membership(machine_store.as_ref(), &record).await {
            Ok(()) => self.ok(format!("machine '{id}' admitted")),
            Err(err) => self.err("ADMIT_FAILED", err),
        }
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
            .machine()
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

    pub(crate) async fn allocate_machine_subnets_with_coordination(
        &self,
        targets: &[String],
    ) -> Result<Vec<Ipv4Net>, String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no running network".to_string())?;
        let machines = active
            .store
            .machine()
            .list_machines()
            .await
            .map_err(|err| format!("failed to list machines for subnet allocation: {err}"))?;
        let configured_peer_keys: HashSet<_> = active
            .mesh
            .network
            .read_peers()
            .await
            .map_err(|err| format!("failed to read configured mesh peers for quorum: {err}"))?
            .into_iter()
            .map(|peer| peer.public_key)
            .collect();

        let cluster: Ipv4Net = self
            .cluster_cidr
            .parse()
            .map_err(|err| format!("invalid cluster CIDR '{}': {err}", self.cluster_cidr))?;
        let allocated = machines.iter().filter_map(|machine| machine.subnet);
        let seed = hash_machine_id(&self.identity.machine_id);
        let mut ipam =
            Ipam::with_allocated_and_seed(cluster, self.subnet_prefix_len, allocated, seed);
        let mut subnets = Vec::with_capacity(targets.len());

        // Target all participating peers — RPC reachability at call time determines
        // who is online. Offline peers (timeout / connection refused) are skipped;
        // only explicit denials abort the candidate and trigger a retry.
        let now = now_unix_secs();
        let self_id = &self.identity.machine_id;
        let local_authoritative_self = active.mesh.authoritative_self_record().await;
        let admitted_machines: Vec<&MachineRecord> = machines
            .iter()
            .filter(|machine| machine.admitted)
            .filter(|machine| {
                machine.id == *self_id || configured_peer_keys.contains(&machine.public_key)
            })
            .collect();
        let local_self_missing = local_authoritative_self.as_ref().is_some_and(|record| {
            record.admitted
                && admitted_machines
                    .iter()
                    .all(|machine| machine.id != *self_id)
        });
        let known_cluster_size = admitted_machines.len() + usize::from(local_self_missing);
        let configured_cluster_size = configured_peer_keys.len() + 1;
        if known_cluster_size < configured_cluster_size {
            return Err(format!(
                "cluster membership view incomplete for quorum: known_cluster_size={known_cluster_size} configured_cluster_size={configured_cluster_size}"
            ));
        }

        let peers: Vec<FanOutTarget> = admitted_machines
            .into_iter()
            .filter(|machine| &machine.id != self_id)
            .map(|m| FanOutTarget {
                machine_id: m.id.clone(),
                overlay_ip: m.overlay_ip,
            })
            .collect();
        let rpc_port = self.coordination_rpc_port;

        for target in targets {
            let mut selected = None;
            loop {
                let Some(candidate) = ipam.allocate() else {
                    break;
                };
                let nonce = format!(
                    "machine-add:{}:{target}:{}",
                    self.identity.machine_id.0, now
                );
                let owner_id = self.identity.machine_id.0.clone();

                // Local prepare first.
                let prepare = self
                    .handle_coordination_prepare(CoordinationPrepareRequest {
                        owner_id: owner_id.clone(),
                        nonce: nonce.clone(),
                        // TTL is the backstop for accepted-but-timed-out prepares; don't shorten
                        // below connect+request budget × 10 without revisiting retry behavior.
                        lease_ttl_secs: 30,
                        operation: CoordinationOperation::SubnetClaimPrepare {
                            machine_id: target.clone(),
                            subnet: candidate.to_string(),
                        },
                    })
                    .await;
                let Some(DaemonPayload::CoordinationPrepare(CoordinationPreparePayload {
                    accepted: local_accepted,
                    prepare_token,
                    ..
                })) = prepare.payload
                else {
                    continue;
                };
                if !prepare.ok || !local_accepted {
                    continue;
                }
                let Some(local_token) = prepare_token else {
                    continue;
                };

                // Fan-out prepare to all online peers.
                let cluster_size = peers.len() + 1; // +1 for self
                let fanout_result = fanout_prepare(
                    &peers,
                    rpc_port,
                    CoordinationPrepareRequest {
                        owner_id: owner_id.clone(),
                        nonce: nonce.clone(),
                        lease_ttl_secs: 30,
                        operation: CoordinationOperation::SubnetClaimPrepare {
                            machine_id: target.clone(),
                            subnet: candidate.to_string(),
                        },
                    },
                    Duration::from_secs(10),
                    cluster_size,
                )
                .await;

                if !fanout_result.denied.is_empty() || !fanout_result.offline.is_empty() {
                    tracing::debug!(
                        target = %target,
                        candidate = %candidate,
                        denied = ?fanout_result.denied,
                        offline = ?fanout_result.offline,
                        "machine add subnet prepare fanout returned non-accepting peers"
                    );
                }

                if !fanout_result.all_online_accepted || !fanout_result.quorum_met {
                    // Abort everywhere — either a peer denied or quorum was not reached.
                    let abort_op = CoordinationOperation::SubnetClaimAbort {
                        machine_id: target.clone(),
                        subnet: candidate.to_string(),
                    };
                    fanout_abort(
                        &accepted_targets(&fanout_result.accepted),
                        rpc_port,
                        CoordinationAbortRequest {
                            owner_id: owner_id.clone(),
                            nonce: nonce.clone(),
                            operation: abort_op.clone(),
                        },
                    )
                    .await;
                    self.handle_coordination_abort(CoordinationAbortRequest {
                        owner_id,
                        nonce,
                        operation: abort_op,
                    })
                    .await;
                    if !fanout_result.quorum_met {
                        return Err(format!(
                            "subnet allocation requires quorum for target '{target}'; denied=[{}] offline=[{}]",
                            format_machine_ids(&fanout_result.denied),
                            format_machine_ids(&fanout_result.offline),
                        ));
                    }
                    continue;
                }

                // Collect tokens from local + peers for the commit.
                let mut prepare_tokens = vec![local_token];
                for (_, payload) in &fanout_result.accepted {
                    if let Some(token) = &payload.prepare_token {
                        prepare_tokens.push(token.clone());
                    }
                }

                // Local commit.
                let commit = self
                    .handle_coordination_commit(CoordinationCommitRequest {
                        owner_id: owner_id.clone(),
                        nonce: nonce.clone(),
                        prepare_tokens: prepare_tokens.clone(),
                        operation: CoordinationOperation::SubnetClaimCommit {
                            machine_id: target.clone(),
                            subnet: candidate.to_string(),
                        },
                    })
                    .await;
                let Some(DaemonPayload::CoordinationCommit(CoordinationCommitPayload {
                    committed,
                    ..
                })) = commit.payload
                else {
                    fanout_abort(
                        &accepted_targets(&fanout_result.accepted),
                        rpc_port,
                        CoordinationAbortRequest {
                            owner_id,
                            nonce,
                            operation: CoordinationOperation::SubnetClaimAbort {
                                machine_id: target.clone(),
                                subnet: candidate.to_string(),
                            },
                        },
                    )
                    .await;
                    continue;
                };
                if !commit.ok || !committed {
                    fanout_abort(
                        &accepted_targets(&fanout_result.accepted),
                        rpc_port,
                        CoordinationAbortRequest {
                            owner_id,
                            nonce,
                            operation: CoordinationOperation::SubnetClaimAbort {
                                machine_id: target.clone(),
                                subnet: candidate.to_string(),
                            },
                        },
                    )
                    .await;
                    continue;
                }

                // Fan-out commit to all accepted peers.
                fanout_commit(
                    &accepted_targets(&fanout_result.accepted),
                    rpc_port,
                    CoordinationCommitRequest {
                        owner_id,
                        nonce,
                        prepare_tokens,
                        operation: CoordinationOperation::SubnetClaimCommit {
                            machine_id: target.clone(),
                            subnet: candidate.to_string(),
                        },
                    },
                    Duration::from_secs(10),
                )
                .await;

                selected = Some(candidate);
                break;
            }
            let Some(subnet) = selected else {
                return Err(format!(
                    "no available coordinated subnets for target '{target}'"
                ));
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
    invite: InviteClaims,
) -> MachineAddTargetResult {
    let mut stage;
    let mut joiner_id = None;
    let invite_id = invite.invite_id;
    let challenge_nonce = invite.challenge_nonce;

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
    let (record, attestation) =
        match remote_join_and_read_self_record(&target, token, &context.ssh_options).await {
            Ok(result) => result,
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
        };
    stage = MachineAddStage::Joined;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, "machine add target: remote join complete");
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
    if let Err(err) = upsert_transient_peer(&context.peer_sync_tx, record.clone()).await {
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

    tracing::info!(
        %target,
        joiner_id = %machine_id,
        invite_id,
        "machine add target: registering pending membership"
    );
    let registered_record = match commit_machine_add_membership(
        &context,
        &invite_id,
        &challenge_nonce,
        &record,
        &attestation,
    )
    .await
    {
        Ok(record) => record,
        Err(err) => {
            tracing::warn!(
                %target,
                joiner_id = %machine_id,
                invite_id,
                error = %err,
                "machine add target: pending membership registration failed"
            );
            let _ = rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref()).await;
            let _ = operation_store.update_status(
                &mut operation,
                MachineOperationStatus::Failed,
                Some(err.clone()),
            );
            return MachineAddTargetResult::Failed {
                target,
                failure: MachineAddFailure::Admit { reason: err },
            };
        }
    };
    stage = MachineAddStage::Registered;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(
        %target,
        joiner_id = %machine_id,
        invite_id,
        "machine add target: pending membership registered"
    );

    if let Err(err) =
        verify_machine_add_target_phase(&context, &target, &registered_record, Phase::Running).await
    {
        tracing::warn!(
            %target,
            joiner_id = %machine_id,
            invite_id,
            error = %err,
            "machine add target: remote verify failed"
        );
        let _ = operation_store.update_status(
            &mut operation,
            MachineOperationStatus::Failed,
            Some(err.clone()),
        );
        return MachineAddTargetResult::Failed {
            target,
            failure: MachineAddFailure::Verify { reason: err },
        };
    }
    stage = MachineAddStage::Verified;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, joiner_id = %machine_id, "machine add target: remote verified");

    if let Err(err) =
        admit_machine_membership(context.machine_store.as_ref(), &registered_record).await
    {
        tracing::warn!(
            %target,
            joiner_id = %machine_id,
            invite_id,
            error = %err,
            "machine add target: admission write failed"
        );
        let _ = operation_store.update_status(
            &mut operation,
            MachineOperationStatus::Failed,
            Some(err.clone()),
        );
        return MachineAddTargetResult::Failed {
            target,
            failure: MachineAddFailure::Admit { reason: err },
        };
    }
    stage = MachineAddStage::Admitted;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, joiner_id = %machine_id, "machine add target: machine admitted");

    let _ = operation_store.update_stage(&mut operation, MachineAddStage::Finalized.to_string());
    let _ = operation_store.update_status(&mut operation, MachineOperationStatus::Succeeded, None);
    tracing::info!(
        %target,
        joiner_id = %machine_id,
        invite_id,
        "machine add target: membership finalized"
    );
    MachineAddTargetResult::Added {
        target,
        joiner_id: machine_id,
    }
}

async fn commit_machine_add_membership(
    context: &MachineAddContext,
    invite_id: &str,
    challenge_nonce: &str,
    record: &MachineRecord,
    attestation: &MembershipAttestation,
) -> Result<MachineRecord, String> {
    if let Err(reason) =
        mesh::verify_membership_attestation(record, invite_id, challenge_nonce, attestation)
    {
        return Err(format!("membership attestation rejected: {reason}"));
    }

    let now = now_unix_secs();
    let mut registered = record.clone();
    registered.admitted = false;
    if registered.created_at == 0 {
        registered.created_at = now;
    }
    registered.updated_at = now;
    context
        .membership_store
        .commit_membership(&registered, invite_id, now)
        .await
        .map_err(|error| format!("failed to commit membership: {error}"))?;
    Ok(registered)
}

async fn admit_machine_membership(
    machine_store: &dyn MachineStore,
    record: &MachineRecord,
) -> Result<(), String> {
    let mut admitted = match super::list::find_machine_record(machine_store, &record.id).await {
        Ok(Some(current)) => current,
        Ok(None) => record.clone(),
        Err(err) => {
            return Err(format!(
                "failed to load machine '{}' before admit: {err}",
                record.id
            ));
        }
    };
    admitted.admitted = true;
    admitted.updated_at = now_unix_secs();
    machine_store
        .upsert_self_machine(&admitted)
        .await
        .map_err(|error| format!("failed to admit machine '{}': {error}", admitted.id))
}

async fn wait_for_remote_machine_phase(
    record: &MachineRecord,
    rpc_port: u16,
    phase: Phase,
) -> Result<(), String> {
    let timeout_ms = u64::try_from(REMOTE_JOIN_VERIFY_TIMEOUT.as_millis())
        .map_err(|error| format!("convert join verify timeout: {error}"))?;
    let client = remote_overlay_client(
        record,
        rpc_port,
        REMOTE_JOIN_VERIFY_TIMEOUT + WAIT_NODE_PHASE_RPC_GRACE,
    );
    client
        .wait_node_phase(phase, timeout_ms)
        .await
        .map(|_| ())
        .map_err(|error| {
            format!(
                "wait for machine '{}' to reach phase '{}': {error}",
                record.id, phase
            )
        })
}

async fn verify_machine_add_target_phase(
    context: &MachineAddContext,
    #[cfg(test)] target: &str,
    #[cfg(not(test))] _target: &str,
    record: &MachineRecord,
    phase: Phase,
) -> Result<(), String> {
    #[cfg(test)]
    if context.rpc_port == 0 {
        let timeout_ms = u64::try_from(REMOTE_JOIN_VERIFY_TIMEOUT.as_millis())
            .map_err(|error| format!("convert join verify timeout: {error}"))?;
        let response = remote_rpc(
            target,
            DaemonRequest::WaitNodePhase { phase, timeout_ms },
            &context.ssh_options,
        )
        .await?;
        if !response.ok {
            return Err(remote_response_error(&response));
        }
        return match response.payload {
            Some(DaemonPayload::NodeStatus(_)) => Ok(()),
            _ => Err(String::from(
                "remote wait-node-phase test probe did not include node-status payload",
            )),
        };
    }

    wait_for_remote_machine_phase(record, context.rpc_port, phase).await
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
        MachineAddStage::TransientPeerInstalled | MachineAddStage::Finalized
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

async fn remote_join_and_read_self_record(
    target: &str,
    token: String,
    ssh_options: &SshOptions,
) -> Result<(MachineRecord, MembershipAttestation), String> {
    let response = remote_rpc(target, DaemonRequest::MeshJoin { token }, ssh_options).await?;
    if !response.ok {
        return Err(remote_response_error(&response));
    }
    let Some(DaemonPayload::MeshJoin(payload)) = response.payload else {
        return Err("remote join did not include mesh-join payload".into());
    };
    Ok((payload.record, payload.attestation))
}

fn remote_overlay_client(
    record: &MachineRecord,
    rpc_port: u16,
    request_timeout: Duration,
) -> DaemonClient<TcpTransport> {
    let addr = SocketAddr::new(IpAddr::V6(record.overlay_ip.0), rpc_port);
    DaemonClient::new(TcpTransport::new(addr).with_timeouts(RPC_CONNECT_TIMEOUT, request_timeout))
}

fn format_machine_ids(machine_ids: &[MachineId]) -> String {
    let mut ids: Vec<String> = machine_ids.iter().map(ToString::to_string).collect();
    ids.sort();
    ids.join(", ")
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
    // In test environments the test binary lives in target/debug/deps/ while
    // the actual ployz binary is one level up at target/debug/.
    let grandparent_candidate = current_exe
        .parent()
        .and_then(|p| p.parent())
        .map(|gp| gp.join("ployz"))
        .unwrap_or_else(|| PathBuf::from("ployz"));
    let candidates = [
        current_exe.with_file_name("ployz"),
        current_exe
            .parent()
            .map(|parent| parent.join("ployz"))
            .unwrap_or_else(|| PathBuf::from("ployz")),
        grandparent_candidate,
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
    if let Some(runtime_target) = &install.runtime_target {
        args.push("--runtime".into());
        args.push(shell_quote(runtime_target));
    }
    if let Some(service_mode) = &install.service_mode {
        args.push("--service-mode".into());
        args.push(shell_quote(service_mode));
    }
    if let Some(source) = &install.source {
        args.push("--source".into());
        args.push(shell_quote(source));
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

/// Derive a deterministic seed from a machine_id for IPAM offset.
///
/// Different coordinators get different seeds so that concurrent IPAM
/// allocations from the same snapshot start scanning from different
/// positions, reducing coordination collisions.
pub(crate) fn hash_machine_id(machine_id: &MachineId) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    machine_id.0.hash(&mut hasher);
    hasher.finish()
}
