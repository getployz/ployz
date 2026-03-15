use crate::config::Mode;
use crate::machine_liveness::{MachineLiveness, machine_liveness};
use crate::mesh::tasks::PeerSyncCommand;
use crate::model::{
    JOIN_RESPONSE_PREFIX, JoinResponse, MachineId, MachineRecord, MachineStatus, Participation,
};
use crate::network::ipam::Ipam;
use crate::node::invite::parse_and_verify_invite_token;
use crate::runtime::ContainerEngine;
use crate::runtime::labels::{LABEL_KIND, LABEL_MACHINE, LABEL_MANAGED};
use crate::store::InviteStore;
use crate::store::MachineStore;
use crate::store::StoreRuntimeControl;
use crate::store::driver::StoreDriver;
use chrono::DateTime;
use ipnet::Ipv4Net;
use ployz_sdk::transport::{
    DaemonResponse, InstallMode, InstallSource, MachineAddOptions, MachineInstallOptions,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{Duration, Instant, sleep, timeout};

use super::super::ssh::{EphemeralSshIdentityFile, SshOptions, run_ssh, run_ssh_with_stdin};
use super::super::{DaemonState, PendingSubnetHeal, SubnetHealAttempt};
use crate::install::find_installer_script;
use crate::store::network::NetworkConfig;
use crate::time::now_unix_secs;

const INVITE_TTL_SECS: u64 = 600;
const SUBNET_HEAL_COOLDOWN_SECS: u64 = 10;
const REMOTE_READY_TIMEOUT: Duration = Duration::from_secs(30);
const REMOTE_READY_POLL_INTERVAL: Duration = Duration::from_secs(1);
const REMOTE_READY_SSH_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_CLEANUP_SSH_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_STATUS_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployz\" status >/dev/null";
const REMOTE_MESH_INIT_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployz\" mesh init --name-stdin";
const REMOTE_MESH_JOIN_COMMAND: &str =
    "set -eu; \"$HOME/.local/bin/ployz\" mesh join --token-stdin";
const REMOTE_MESH_SELF_RECORD_COMMAND: &str =
    "set -eu; \"$HOME/.local/bin/ployz\" mesh self-record";
const REMOTE_MESH_READY_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployz\" mesh ready --json";
const REMOTE_MESH_DOWN_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployz\" mesh down";
const REMOTE_MESH_DESTROY_COMMAND: &str =
    "set -eu; \"$HOME/.local/bin/ployz\" mesh destroy --name-stdin";
const REMOTE_PLOYZ_VERSION_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployz\" --version";
const HEAL_WORKLOAD_STOP_GRACE: Duration = Duration::from_secs(10);
const SUBNET_HEAL_SETTLE_SECS: u64 = 10;

#[derive(Clone)]
struct MachineAddContext {
    network_name: String,
    store: StoreDriver,
    peer_sync_tx: mpsc::Sender<PeerSyncCommand>,
    ssh_options: SshOptions,
    install: MachineInstallOptions,
}

#[derive(Debug)]
enum MachineAddOutcome {
    AwaitingSelfPublication {
        target: String,
        joiner_id: MachineId,
    },
    FailedPreflight {
        target: String,
        reason: String,
    },
    FailedJoin {
        target: String,
        reason: String,
    },
    FailedSelfRecord {
        target: String,
        reason: String,
    },
    FailedReady {
        target: String,
        reason: String,
    },
}

#[derive(Debug, Default)]
struct MachineAddSummary {
    awaiting_self_publication: Vec<String>,
    failed_preflight: Vec<String>,
    failed_join: Vec<String>,
    failed_self_record: Vec<String>,
    failed_ready: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RemoteReadyPayload {
    ready: bool,
    #[serde(default)]
    phase: String,
    #[serde(default)]
    store_healthy: bool,
    #[serde(default)]
    heartbeat_started: bool,
}

#[derive(Debug, Deserialize)]
struct RemoteReadyEnvelope {
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalSubnetHealPlan {
    current_subnet: Ipv4Net,
    winner_machine_id: MachineId,
    target_subnet: Ipv4Net,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RestartableWorkload {
    container_name: String,
    was_running: bool,
}

fn parse_remote_ready_payload(output: &str) -> Result<RemoteReadyPayload, String> {
    if let Ok(payload) = serde_json::from_str::<RemoteReadyPayload>(output) {
        return Ok(payload);
    }

    let envelope = serde_json::from_str::<RemoteReadyEnvelope>(output)
        .map_err(|error| format!("failed to parse remote readiness envelope: {error}"))?;
    serde_json::from_str::<RemoteReadyPayload>(&envelope.message)
        .map_err(|error| format!("failed to parse remote readiness message: {error}"))
}

fn remote_join_ready(payload: &RemoteReadyPayload) -> bool {
    payload.ready
        || (payload.phase == "running" && payload.store_healthy && payload.heartbeat_started)
}

impl DaemonState {
    pub(crate) async fn handle_machine_list(&self) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let machines = match active.mesh.store.list_machines().await {
            Ok(machines) => machines,
            Err(err) => return self.err("LIST_FAILED", format!("failed to list machines: {err}")),
        };

        if machines.is_empty() {
            return self.ok("no machines");
        }

        let now = now_unix_secs();

        struct Row {
            id: String,
            status: &'static str,
            participation: &'static str,
            liveness: &'static str,
            overlay: String,
            subnet: String,
            heartbeat: String,
            created: String,
        }

        let rows: Vec<Row> = machines
            .iter()
            .map(|machine| Row {
                id: machine.id.0.clone(),
                status: format_status(machine),
                participation: format_participation(machine),
                liveness: format_liveness(machine, now),
                overlay: machine.overlay_ip.0.to_string(),
                subnet: machine
                    .subnet
                    .map(|subnet| subnet.to_string())
                    .unwrap_or_else(|| "—".into()),
                heartbeat: format_heartbeat(machine.last_heartbeat, now),
                created: format_timestamp(machine.created_at),
            })
            .collect();

        let w_id = rows
            .iter()
            .map(|row| row.id.len())
            .max()
            .unwrap_or(0)
            .max(2);
        let w_ov = rows
            .iter()
            .map(|row| row.overlay.len())
            .max()
            .unwrap_or(0)
            .max(10);
        let w_sub = rows
            .iter()
            .map(|row| row.subnet.len())
            .max()
            .unwrap_or(0)
            .max(6);
        let w_hb = rows
            .iter()
            .map(|row| row.heartbeat.len())
            .max()
            .unwrap_or(0)
            .max(9);
        let w_part = rows
            .iter()
            .map(|row| row.participation.len())
            .max()
            .unwrap_or(0)
            .max("PARTICIPATION".len());
        let w_live = rows
            .iter()
            .map(|row| row.liveness.len())
            .max()
            .unwrap_or(0)
            .max("LIVENESS".len());

        let mut lines = Vec::with_capacity(rows.len() + 1);
        lines.push(format!(
            "{:<w_id$}  {:<6}  {:<w_part$}  {:<w_live$}  {:<w_ov$}  {:<w_sub$}  {:<w_hb$}  {}",
            "ID",
            "STATUS",
            "PARTICIPATION",
            "LIVENESS",
            "OVERLAY IP",
            "SUBNET",
            "HEARTBEAT",
            "CREATED",
        ));
        for row in &rows {
            lines.push(format!(
                "{:<w_id$}  {:<6}  {:<w_part$}  {:<w_live$}  {:<w_ov$}  {:<w_sub$}  {:<w_hb$}  {}",
                row.id,
                row.status,
                row.participation,
                row.liveness,
                row.overlay,
                row.subnet,
                row.heartbeat,
                row.created,
            ));
        }
        self.ok(lines.join("\n"))
    }

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

        if let Err(err) = bootstrap_remote_machine(target, install, &SshOptions::default()).await {
            return self.err("SSH_BOOTSTRAP_FAILED", err);
        }

        if let Err(err) = run_ssh_with_stdin(
            target,
            REMOTE_MESH_INIT_COMMAND,
            network.as_bytes(),
            &SshOptions::default(),
        )
        .await
        {
            return self.err("REMOTE_INIT_FAILED", err);
        }

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
                        store: active.mesh.store.clone(),
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

        let mut summary = MachineAddSummary::default();
        let mut tasks = JoinSet::new();

        for (target, allocated_subnet) in targets.iter().cloned().zip(allocated_subnets) {
            tracing::info!(%target, %allocated_subnet, "machine add issuing invite token");
            let token = match self
                .do_issue_invite_token(&running, INVITE_TTL_SECS, allocated_subnet)
                .await
            {
                Ok(token) => token,
                Err(err) => {
                    summary.failed_preflight.push(format!(
                        "{target}: failed to issue invite token for subnet {allocated_subnet}: {err}"
                    ));
                    continue;
                }
            };
            tracing::info!(%target, "machine add invite token issued");
            let invite = match parse_and_verify_invite_token(&token) {
                Ok(invite) => invite,
                Err(err) => {
                    summary.failed_preflight.push(format!(
                        "{target}: issued invite token could not be re-read for finalization: {err}"
                    ));
                    continue;
                }
            };

            let task_context = context.clone();
            tasks.spawn(async move {
                run_machine_add_target(
                    task_context,
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
                Ok(outcome) => summary.push(outcome),
                Err(err) => summary
                    .failed_preflight
                    .push(format!("task join failure: {err}")),
            }
        }

        summary.into_response(self, &warnings)
    }

    pub(crate) async fn handle_machine_remove(&self, id: &str, force: bool) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let machine_id = MachineId(id.to_string());
        let record = match find_machine_record(&active.mesh.store, &machine_id).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                return self.err("MACHINE_NOT_FOUND", format!("machine '{id}' not found"));
            }
            Err(err) => {
                return self.err("LIST_FAILED", format!("failed to read machines: {err}"));
            }
        };

        if !force && record.participation != Participation::Disabled {
            return self.err(
                "MACHINE_NOT_DISABLED",
                format!(
                    "machine '{id}' must be disabled before removal (current participation: {})",
                    record.participation
                ),
            );
        }

        match active.mesh.store.delete_machine(&machine_id).await {
            Ok(()) => self.ok(format!("machine '{id}' removed")),
            Err(err) => self.err("DELETE_FAILED", format!("failed to remove machine: {err}")),
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
            .mesh
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

    pub async fn heal_local_subnet_conflict_if_needed(&mut self) {
        let Some(active) = self.active.as_ref() else {
            self.pending_subnet_heal = None;
            self.last_subnet_heal_attempt = None;
            return;
        };

        if active.mesh.phase() != crate::Phase::Running {
            return;
        }

        if !active.mesh.store.healthy().await {
            tracing::info!(
                machine_id = %self.identity.machine_id,
                "local subnet heal: store unhealthy, deferring"
            );
            return;
        }

        let machines = match active.mesh.store.list_machines().await {
            Ok(machines) => machines,
            Err(err) => {
                tracing::warn!(error = %err, "local subnet heal: failed to list machines");
                return;
            }
        };

        if !machines
            .iter()
            .any(|machine| machine.id == self.identity.machine_id)
        {
            tracing::warn!(
                machine_id = %self.identity.machine_id,
                "local subnet heal: local machine missing from store, deferring"
            );
            return;
        }

        let duplicate_groups = duplicate_subnet_groups(&machines);
        if duplicate_groups.is_empty() {
            self.pending_subnet_heal = None;
            self.last_subnet_heal_attempt = None;
            return;
        }

        tracing::warn!(
            machine_id = %self.identity.machine_id,
            duplicate_groups = ?duplicate_group_log_view(&duplicate_groups),
            "local subnet heal: duplicate subnet claims detected"
        );

        let current_conflict =
            match local_duplicate_subnet_conflict(&machines, &self.identity.machine_id) {
                Some(conflict) => conflict,
                None => {
                    self.pending_subnet_heal = None;
                    return;
                }
            };

        let now = now_unix_secs();
        let pending = match self.pending_subnet_heal {
            Some(pending) if pending.network_subnet == current_conflict.subnet => pending,
            _ => {
                let target_subnet = match allocate_replacement_subnet(
                    &machines,
                    &self.identity.machine_id,
                    &self.cluster_cidr,
                    self.subnet_prefix_len,
                ) {
                    Ok(target_subnet) => target_subnet,
                    Err(err) => {
                        tracing::warn!(error = %err, "local subnet heal: failed to plan subnet heal");
                        return;
                    }
                };
                let pending = PendingSubnetHeal {
                    network_subnet: current_conflict.subnet,
                    target_subnet,
                    planned_at: now,
                };
                self.pending_subnet_heal = Some(pending);
                tracing::info!(
                    machine_id = %self.identity.machine_id,
                    current_subnet = %pending.network_subnet,
                    target_subnet = %pending.target_subnet,
                    settle_secs = SUBNET_HEAL_SETTLE_SECS,
                    "local subnet heal: planned replacement subnet, waiting for settle window"
                );
                return;
            }
        };

        let target_winner =
            target_subnet_winner(&machines, &self.identity.machine_id, pending.target_subnet);
        if target_winner != self.identity.machine_id {
            let target_subnet = match allocate_replacement_subnet(
                &machines,
                &self.identity.machine_id,
                &self.cluster_cidr,
                self.subnet_prefix_len,
            ) {
                Ok(target_subnet) => target_subnet,
                Err(err) => {
                    tracing::warn!(error = %err, "local subnet heal: failed to replan subnet heal");
                    return;
                }
            };
            let pending = PendingSubnetHeal {
                network_subnet: current_conflict.subnet,
                target_subnet,
                planned_at: now,
            };
            self.pending_subnet_heal = Some(pending);
            tracing::info!(
                machine_id = %self.identity.machine_id,
                losing_machine_id = %target_winner,
                current_subnet = %pending.network_subnet,
                target_subnet = %pending.target_subnet,
                settle_secs = SUBNET_HEAL_SETTLE_SECS,
                "local subnet heal: lost target subnet tie-break, replanning"
            );
            return;
        }

        if now.saturating_sub(pending.planned_at) < SUBNET_HEAL_SETTLE_SECS {
            tracing::info!(
                machine_id = %self.identity.machine_id,
                current_subnet = %pending.network_subnet,
                target_subnet = %pending.target_subnet,
                settle_secs = SUBNET_HEAL_SETTLE_SECS,
                "local subnet heal: waiting for settle window"
            );
            return;
        }

        let plan = LocalSubnetHealPlan {
            current_subnet: current_conflict.subnet,
            winner_machine_id: current_conflict.winner_machine_id,
            target_subnet: pending.target_subnet,
        };

        match plan_local_subnet_heal(
            &machines,
            &self.identity.machine_id,
            &self.cluster_cidr,
            self.subnet_prefix_len,
        ) {
            Ok(Some(_)) => {}
            Ok(None) => {
                self.pending_subnet_heal = None;
                return;
            }
            Err(err) => {
                tracing::warn!(error = %err, "local subnet heal: failed to plan subnet heal");
                return;
            }
        }

        if let Some(last_attempt) = self.last_subnet_heal_attempt
            && last_attempt.network_subnet == plan.current_subnet
            && last_attempt.target_subnet == plan.target_subnet
            && now.saturating_sub(last_attempt.attempted_at) < SUBNET_HEAL_COOLDOWN_SECS
        {
            tracing::info!(
                machine_id = %self.identity.machine_id,
                current_subnet = %plan.current_subnet,
                target_subnet = %plan.target_subnet,
                "local subnet heal: skipping repeated attempt during cooldown"
            );
            return;
        }

        self.pending_subnet_heal = None;
        self.last_subnet_heal_attempt = Some(SubnetHealAttempt {
            network_subnet: plan.current_subnet,
            target_subnet: plan.target_subnet,
            attempted_at: now,
        });

        tracing::warn!(
            machine_id = %self.identity.machine_id,
            winner_machine_id = %plan.winner_machine_id,
            current_subnet = %plan.current_subnet,
            target_subnet = %plan.target_subnet,
            "local subnet heal: starting"
        );

        match self.apply_local_subnet_heal(&plan).await {
            Ok(()) => {
                tracing::info!(
                    machine_id = %self.identity.machine_id,
                    current_subnet = %plan.current_subnet,
                    target_subnet = %plan.target_subnet,
                    "local subnet heal: complete"
                );
            }
            Err(err) => {
                tracing::warn!(
                    machine_id = %self.identity.machine_id,
                    current_subnet = %plan.current_subnet,
                    target_subnet = %plan.target_subnet,
                    error = %err,
                    "local subnet heal: failed"
                );
            }
        }
    }

    async fn apply_local_subnet_heal(&mut self, plan: &LocalSubnetHealPlan) -> Result<(), String> {
        let network_name = self
            .active
            .as_ref()
            .map(|active| active.config.name.0.clone())
            .ok_or_else(|| "no running network".to_string())?;
        let config_path = NetworkConfig::path(&self.data_dir, &network_name);
        let mut config = NetworkConfig::load(&config_path)
            .map_err(|err| format!("load network config: {err}"))?;
        config.subnet = plan.target_subnet;
        config
            .save(&config_path)
            .map_err(|err| format!("save network config: {err}"))?;

        match self.mode {
            Mode::Memory => {
                self.apply_local_subnet_heal_in_memory_mode(&network_name)
                    .await
            }
            Mode::Docker | Mode::HostExec | Mode::HostService => {
                self.restart_active_mesh_for_subnet_heal(&network_name)
                    .await
            }
        }
    }

    async fn apply_local_subnet_heal_in_memory_mode(
        &mut self,
        network_name: &str,
    ) -> Result<(), String> {
        let config_path = NetworkConfig::path(&self.data_dir, network_name);
        let config = NetworkConfig::load(&config_path)
            .map_err(|err| format!("load network config: {err}"))?;
        let Some(active) = self.active.as_mut() else {
            return Err("no running network".into());
        };

        active.config = config.clone();
        let Some(record) = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.subnet = Some(config.subnet);
                record.updated_at = now_unix_secs();
            })
            .await
        else {
            return Err("local authoritative self record missing".into());
        };
        let _ = record;
        Ok(())
    }

    async fn restart_active_mesh_for_subnet_heal(
        &mut self,
        network_name: &str,
    ) -> Result<(), String> {
        let Some(active) = self.active.as_ref() else {
            return Err("no running network".into());
        };

        self.publish_local_machine_down_for_subnet_heal(&active.config.subnet)
            .await?;

        let workloads = self
            .stop_local_workloads_for_subnet_heal(network_name)
            .await?;

        self.restart_active_runtime_for_subnet_heal(network_name)
            .await?;

        self.start_local_workloads_after_subnet_heal(network_name, &workloads)
            .await
    }

    async fn publish_local_machine_down_for_subnet_heal(
        &self,
        current_subnet: &Ipv4Net,
    ) -> Result<(), String> {
        let Some(active) = self.active.as_ref() else {
            return Err("no running network".into());
        };

        let now = now_unix_secs();
        let Some(record) = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.subnet = Some(*current_subnet);
                record.status = MachineStatus::Down;
                record.participation = Participation::Disabled;
                record.last_heartbeat = now;
                record.updated_at = now;
            })
            .await
        else {
            return Err("local authoritative self record missing".into());
        };
        let _ = record;
        Ok(())
    }

    async fn stop_local_workloads_for_subnet_heal(
        &self,
        network_name: &str,
    ) -> Result<Vec<RestartableWorkload>, String> {
        match self.mode {
            Mode::Memory => Ok(Vec::new()),
            Mode::Docker | Mode::HostExec | Mode::HostService => {
                let engine = ContainerEngine::connect()
                    .await
                    .map_err(|err| format!("connect docker engine for subnet heal: {err}"))?;
                let bridge_name = format!("ployz-{network_name}");
                let observed = engine
                    .list_by_labels(&[
                        (LABEL_MANAGED, "true"),
                        (LABEL_KIND, "workload"),
                        (LABEL_MACHINE, &self.identity.machine_id.0),
                    ])
                    .await
                    .map_err(|err| format!("list local workloads for subnet heal: {err}"))?;

                let mut restartable = Vec::new();
                for container in observed {
                    if !container.networks.contains_key(&bridge_name) {
                        continue;
                    }

                    if container.running {
                        engine
                            .stop(&container.container_name, HEAL_WORKLOAD_STOP_GRACE)
                            .await
                            .map_err(|err| {
                                format!(
                                    "stop workload '{}' for subnet heal: {err}",
                                    container.container_name
                                )
                            })?;
                    }

                    let bridge = crate::network::docker_bridge::DockerBridgeNetwork::new(
                        network_name,
                        self.active
                            .as_ref()
                            .map(|active| active.config.subnet)
                            .ok_or_else(|| "no running network".to_string())?,
                    )
                    .await
                    .map_err(|err| format!("build bridge handle for subnet heal: {err}"))?;
                    bridge
                        .disconnect(&container.container_name, true)
                        .await
                        .map_err(|err| {
                            format!(
                                "disconnect workload '{}' from old bridge: {err}",
                                container.container_name
                            )
                        })?;

                    restartable.push(RestartableWorkload {
                        container_name: container.container_name,
                        was_running: container.running,
                    });
                }

                Ok(restartable)
            }
        }
    }

    async fn start_local_workloads_after_subnet_heal(
        &self,
        network_name: &str,
        workloads: &[RestartableWorkload],
    ) -> Result<(), String> {
        match self.mode {
            Mode::Memory => Ok(()),
            Mode::Docker | Mode::HostExec | Mode::HostService => {
                if workloads.is_empty() {
                    return Ok(());
                }

                let engine = ContainerEngine::connect()
                    .await
                    .map_err(|err| format!("connect docker engine after subnet heal: {err}"))?;
                let target_subnet = self
                    .active
                    .as_ref()
                    .map(|active| active.config.subnet)
                    .ok_or_else(|| "no running network".to_string())?;
                let bridge = crate::network::docker_bridge::DockerBridgeNetwork::new(
                    network_name,
                    target_subnet,
                )
                .await
                .map_err(|err| format!("build target bridge handle after subnet heal: {err}"))?;

                for workload in workloads {
                    bridge
                        .connect(&workload.container_name, None)
                        .await
                        .map_err(|err| {
                            format!(
                                "reconnect workload '{}' to healed bridge: {err}",
                                workload.container_name
                            )
                        })?;
                    if workload.was_running {
                        engine
                            .start(&workload.container_name)
                            .await
                            .map_err(|err| {
                                format!(
                                    "restart workload '{}' after subnet heal: {err}",
                                    workload.container_name
                                )
                            })?;
                    }
                }

                Ok(())
            }
        }
    }

    async fn degraded_mesh_warnings(&self) -> Result<Vec<String>, String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no running network".to_string())?;
        let machines = active
            .mesh
            .store
            .list_machines()
            .await
            .map_err(|err| format!("failed to list machines: {err}"))?;
        let now = now_unix_secs();

        Ok(machines
            .into_iter()
            .filter(|machine| machine.id != self.identity.machine_id)
            .filter(|machine| match machine.participation {
                Participation::Disabled => false,
                Participation::Enabled | Participation::Draining => true,
            })
            .filter(|machine| machine_liveness(machine, now) == MachineLiveness::Stale)
            .map(|machine| {
                let role = match machine.participation {
                    Participation::Disabled => "disabled",
                    Participation::Enabled => "enabled",
                    Participation::Draining => "draining",
                };
                let heartbeat = format_heartbeat(machine.last_heartbeat, now);
                format!(
                    "warning: {role} peer '{}' has a stale heartbeat ({heartbeat})",
                    machine.id
                )
            })
            .collect())
    }
}

impl MachineAddSummary {
    fn push(&mut self, outcome: MachineAddOutcome) {
        match outcome {
            MachineAddOutcome::AwaitingSelfPublication { target, joiner_id } => {
                self.awaiting_self_publication
                    .push(format!("{target} -> {}", joiner_id.0));
            }
            MachineAddOutcome::FailedPreflight { target, reason } => {
                self.failed_preflight.push(format!("{target}: {reason}"));
            }
            MachineAddOutcome::FailedJoin { target, reason } => {
                self.failed_join.push(format!("{target}: {reason}"));
            }
            MachineAddOutcome::FailedSelfRecord { target, reason } => {
                self.failed_self_record.push(format!("{target}: {reason}"));
            }
            MachineAddOutcome::FailedReady { target, reason } => {
                self.failed_ready.push(format!("{target}: {reason}"));
            }
        }
    }

    fn has_failures(&self) -> bool {
        !self.failed_preflight.is_empty()
            || !self.failed_join.is_empty()
            || !self.failed_self_record.is_empty()
            || !self.failed_ready.is_empty()
    }

    fn into_response(self, state: &DaemonState, warnings: &[String]) -> DaemonResponse {
        let mut lines = Vec::new();
        if !warnings.is_empty() {
            lines.extend(warnings.iter().cloned());
            lines.push(String::new());
        }

        lines.push("machine add summary".into());
        push_summary_section(
            &mut lines,
            "awaiting_self_publication",
            &self.awaiting_self_publication,
        );
        push_summary_section(&mut lines, "failed_preflight", &self.failed_preflight);
        push_summary_section(&mut lines, "failed_join", &self.failed_join);
        push_summary_section(&mut lines, "failed_self_record", &self.failed_self_record);
        push_summary_section(&mut lines, "failed_ready", &self.failed_ready);

        let message = lines.join("\n");
        if self.has_failures() {
            return DaemonResponse {
                ok: false,
                code: "MACHINE_ADD_FAILED".into(),
                message,
            };
        }

        state.ok(message)
    }
}

async fn run_machine_add_target(
    context: MachineAddContext,
    target: String,
    allocated_subnet: Ipv4Net,
    token: String,
    invite_id: String,
) -> MachineAddOutcome {
    tracing::info!(%target, "machine add target: bootstrap starting");
    if let Err(err) =
        bootstrap_remote_machine(&target, &context.install, &context.ssh_options).await
    {
        return MachineAddOutcome::FailedPreflight {
            target,
            reason: err,
        };
    }
    tracing::info!(%target, "machine add target: bootstrap complete");

    tracing::info!(%target, "machine add target: remote join starting");
    match run_ssh_with_stdin(
        &target,
        REMOTE_MESH_JOIN_COMMAND,
        token.as_bytes(),
        &context.ssh_options,
    )
    .await
    {
        Ok(_) => {}
        Err(err) if err.contains("already exists") || err.contains("already running") => {
            tracing::info!(target, "remote already joined, continuing to self-record");
        }
        Err(err) => {
            return MachineAddOutcome::FailedJoin {
                target,
                reason: err,
            };
        }
    }
    tracing::info!(%target, "machine add target: remote join complete");

    tracing::info!(%target, "machine add target: self-record starting");
    let self_record_output = match run_ssh(
        &target,
        REMOTE_MESH_SELF_RECORD_COMMAND,
        &context.ssh_options,
    )
    .await
    {
        Ok(output) => output,
        Err(err) => {
            let _ =
                best_effort_remote_cleanup(&target, &context.network_name, &context.ssh_options)
                    .await;
            return MachineAddOutcome::FailedSelfRecord {
                target,
                reason: format!(
                    "{err}\nhint: run `ployz mesh self-record` on the joiner and `ployz mesh accept <response>` on this machine"
                ),
            };
        }
    };
    tracing::info!(%target, "machine add target: self-record complete");

    let record = match decode_joiner_record(&self_record_output) {
        Ok(record) => record,
        Err(err) => {
            let _ =
                best_effort_remote_cleanup(&target, &context.network_name, &context.ssh_options)
                    .await;
            return MachineAddOutcome::FailedSelfRecord {
                target,
                reason: err,
            };
        }
    };
    if record.subnet != Some(allocated_subnet) {
        let actual_subnet = record
            .subnet
            .map(|subnet| subnet.to_string())
            .unwrap_or_else(|| "—".into());
        let _ =
            best_effort_remote_cleanup(&target, &context.network_name, &context.ssh_options).await;
        return MachineAddOutcome::FailedSelfRecord {
            target,
            reason: format!(
                "joiner self-record subnet '{actual_subnet}' did not match allocated subnet '{allocated_subnet}'"
            ),
        };
    }

    let joiner_id = record.id.clone();
    tracing::info!(%target, joiner_id = %joiner_id, "machine add target: transient peer install starting");
    if let Err(err) = upsert_transient_peer(&context.peer_sync_tx, record.clone()).await {
        let _ =
            best_effort_remote_cleanup(&target, &context.network_name, &context.ssh_options).await;
        return MachineAddOutcome::FailedPreflight {
            target,
            reason: err,
        };
    }
    tracing::info!(%target, joiner_id = %joiner_id, "machine add target: transient peer installed");

    tracing::info!(%target, joiner_id = %joiner_id, "machine add target: waiting for remote ready");
    if let Err(err) = wait_for_remote_ready(&target, &context.ssh_options).await {
        tracing::warn!(
            %target,
            joiner_id = %joiner_id,
            error = %err,
            "machine add target: remote ready failed"
        );
        if let Err(remove_err) = remove_transient_peer(&context.peer_sync_tx, &joiner_id).await {
            tracing::warn!(
                %target,
                joiner_id = %joiner_id,
                error = %remove_err,
                "machine add target: transient peer cleanup failed"
            );
        }
        if let Err(cleanup_err) =
            best_effort_remote_cleanup(&target, &context.network_name, &context.ssh_options).await
        {
            tracing::warn!(
                %target,
                joiner_id = %joiner_id,
                error = %cleanup_err,
                "machine add target: remote cleanup failed"
            );
        }
        return MachineAddOutcome::FailedReady {
            target,
            reason: err,
        };
    }
    tracing::info!(%target, joiner_id = %joiner_id, "machine add target: remote ready");

    tracing::info!(
        %target,
        joiner_id = %joiner_id,
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
            joiner_id = %joiner_id,
            invite_id,
            error = %err,
            "machine add target: invite finalization failed"
        );
    } else {
        tracing::info!(
            %target,
            joiner_id = %joiner_id,
            invite_id,
            "machine add target: invite finalized"
        );
    }

    tracing::info!(
        %target,
        joiner_id = %joiner_id,
        "machine add target: awaiting self-publication"
    );
    MachineAddOutcome::AwaitingSelfPublication { target, joiner_id }
}

async fn upsert_transient_peer(
    peer_sync_tx: &mpsc::Sender<PeerSyncCommand>,
    record: MachineRecord,
) -> Result<(), String> {
    peer_sync_tx
        .send(PeerSyncCommand::UpsertTransient(record))
        .await
        .map_err(|err| format!("failed to install founder-local transient peer: {err}"))
}

async fn remove_transient_peer(
    peer_sync_tx: &mpsc::Sender<PeerSyncCommand>,
    machine_id: &MachineId,
) -> Result<(), String> {
    peer_sync_tx
        .send(PeerSyncCommand::RemoveTransient(machine_id.clone()))
        .await
        .map_err(|err| format!("failed to clear founder-local transient peer: {err}"))
}

async fn wait_for_remote_ready(target: &str, ssh_options: &SshOptions) -> Result<(), String> {
    let deadline = Instant::now() + REMOTE_READY_TIMEOUT;
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;
        let last_error = match timeout(
            REMOTE_READY_SSH_TIMEOUT,
            run_ssh(target, REMOTE_MESH_READY_COMMAND, ssh_options),
        )
        .await
        {
            Ok(Ok(output)) => match parse_remote_ready_payload(&output) {
                Ok(payload) => {
                    if remote_join_ready(&payload) {
                        tracing::debug!(%target, attempt, payload = %output, "remote mesh ready confirmed");
                        return Ok(());
                    }
                    tracing::debug!(
                        %target,
                        attempt,
                        payload = %output,
                        "remote mesh not ready yet"
                    );
                    format!("mesh reported not ready yet: {output}")
                }
                Err(err) => {
                    tracing::debug!(
                        %target,
                        attempt,
                        payload = %output,
                        error = %err,
                        "remote readiness payload parse failed"
                    );
                    format!("failed to parse remote readiness payload '{output}': {err}")
                }
            },
            Ok(Err(err)) => {
                tracing::debug!(%target, attempt, error = %err, "remote readiness ssh failed");
                err
            }
            Err(_) => {
                let err = format!(
                    "ssh readiness probe exceeded {:?}",
                    REMOTE_READY_SSH_TIMEOUT
                );
                tracing::debug!(%target, attempt, error = %err, "remote readiness ssh timed out");
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

    let installer_path = find_installer_script()?;
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

async fn best_effort_remote_cleanup(
    target: &str,
    network_name: &str,
    ssh_options: &SshOptions,
) -> Result<(), String> {
    tracing::debug!(%target, %network_name, "machine add cleanup: mesh down starting");
    let down_error = match timeout(
        REMOTE_CLEANUP_SSH_TIMEOUT,
        run_ssh(target, REMOTE_MESH_DOWN_COMMAND, ssh_options),
    )
    .await
    {
        Ok(Ok(_)) => None,
        Ok(Err(err)) => Some(err),
        Err(_) => Some(format!(
            "mesh down ssh exceeded {:?}",
            REMOTE_CLEANUP_SSH_TIMEOUT
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
        REMOTE_CLEANUP_SSH_TIMEOUT,
        run_ssh_with_stdin(
            target,
            REMOTE_MESH_DESTROY_COMMAND,
            network_name.as_bytes(),
            ssh_options,
        ),
    )
    .await
    {
        Ok(Ok(_)) => None,
        Ok(Err(err)) => Some(err),
        Err(_) => Some(format!(
            "mesh destroy ssh exceeded {:?}",
            REMOTE_CLEANUP_SSH_TIMEOUT
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

async fn find_machine_record(
    store: &StoreDriver,
    machine_id: &MachineId,
) -> Result<Option<MachineRecord>, String> {
    let machines = store
        .list_machines()
        .await
        .map_err(|err| format!("{err}"))?;
    Ok(machines
        .into_iter()
        .find(|machine| machine.id == *machine_id))
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

fn duplicate_subnet_groups(machines: &[MachineRecord]) -> Vec<(Ipv4Net, Vec<MachineId>)> {
    let mut groups: BTreeMap<Ipv4Net, Vec<MachineId>> = BTreeMap::new();

    for machine in machines {
        let Some(subnet) = machine.subnet else {
            continue;
        };
        groups.entry(subnet).or_default().push(machine.id.clone());
    }

    groups
        .into_iter()
        .filter_map(|(subnet, mut machine_ids)| {
            if machine_ids.len() < 2 {
                return None;
            }
            machine_ids.sort_by(|left, right| left.0.cmp(&right.0));
            Some((subnet, machine_ids))
        })
        .collect()
}

fn duplicate_group_log_view(groups: &[(Ipv4Net, Vec<MachineId>)]) -> Vec<(String, Vec<String>)> {
    groups
        .iter()
        .map(|(subnet, machine_ids)| {
            (
                subnet.to_string(),
                machine_ids
                    .iter()
                    .map(|machine_id| machine_id.0.clone())
                    .collect(),
            )
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalSubnetConflict {
    subnet: Ipv4Net,
    winner_machine_id: MachineId,
}

fn local_duplicate_subnet_conflict(
    machines: &[MachineRecord],
    local_machine_id: &MachineId,
) -> Option<LocalSubnetConflict> {
    for (subnet, machine_ids) in duplicate_subnet_groups(machines) {
        let Some(local_index) = machine_ids
            .iter()
            .position(|machine_id| machine_id == local_machine_id)
        else {
            continue;
        };
        if local_index == 0 {
            return None;
        }
        let winner_machine_id = machine_ids.first().cloned().expect("non-empty group");
        return Some(LocalSubnetConflict {
            subnet,
            winner_machine_id,
        });
    }

    None
}

fn target_subnet_winner(
    machines: &[MachineRecord],
    local_machine_id: &MachineId,
    target_subnet: Ipv4Net,
) -> MachineId {
    let mut contenders: Vec<MachineId> = machines
        .iter()
        .filter(|machine| machine.subnet == Some(target_subnet))
        .map(|machine| machine.id.clone())
        .collect();
    contenders.push(local_machine_id.clone());
    contenders.sort_by(|left, right| left.0.cmp(&right.0));
    contenders.first().cloned().expect("at least one contender")
}

fn plan_local_subnet_heal(
    machines: &[MachineRecord],
    local_machine_id: &MachineId,
    cluster_cidr: &str,
    subnet_prefix_len: u8,
) -> Result<Option<LocalSubnetHealPlan>, String> {
    for (subnet, machine_ids) in duplicate_subnet_groups(machines) {
        let Some(local_index) = machine_ids
            .iter()
            .position(|machine_id| machine_id == local_machine_id)
        else {
            continue;
        };
        if local_index == 0 {
            return Ok(None);
        }

        let target_subnet = allocate_replacement_subnet(
            machines,
            local_machine_id,
            cluster_cidr,
            subnet_prefix_len,
        )?;
        let winner_machine_id = machine_ids.first().cloned().expect("non-empty group");
        return Ok(Some(LocalSubnetHealPlan {
            current_subnet: subnet,
            winner_machine_id,
            target_subnet,
        }));
    }

    Ok(None)
}

fn allocate_replacement_subnet(
    machines: &[MachineRecord],
    local_machine_id: &MachineId,
    cluster_cidr: &str,
    subnet_prefix_len: u8,
) -> Result<Ipv4Net, String> {
    let cluster: Ipv4Net = cluster_cidr
        .parse()
        .map_err(|err| format!("invalid cluster CIDR '{cluster_cidr}': {err}"))?;
    let allocated = machines.iter().filter_map(|machine| {
        if machine.id == *local_machine_id {
            return None;
        }
        machine.subnet
    });
    let mut ipam = Ipam::with_allocated(cluster, subnet_prefix_len, allocated);
    ipam.allocate()
        .ok_or_else(|| "no available subnets for local heal".into())
}

fn push_summary_section(lines: &mut Vec<String>, label: &str, values: &[String]) {
    lines.push(format!("{label}: {}", values.len()));
    lines.extend(values.iter().map(|value| format!("  {value}")));
}

fn format_status(machine: &MachineRecord) -> &'static str {
    match machine.status {
        MachineStatus::Up => "up",
        MachineStatus::Down => "down",
        MachineStatus::Unknown => "—",
    }
}

fn install_script_args(install: &MachineInstallOptions) -> String {
    let mut args = vec!["install".to_string()];
    if let Some(mode) = install.mode {
        args.push("--mode".into());
        args.push(
            match mode {
                InstallMode::Docker => "docker",
                InstallMode::HostExec => "host-exec",
                InstallMode::HostService => "host-service",
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

fn format_participation(machine: &MachineRecord) -> &'static str {
    match machine.participation {
        Participation::Enabled => "enabled",
        Participation::Draining => "draining",
        Participation::Disabled => "disabled",
    }
}

fn format_liveness(machine: &MachineRecord, now: u64) -> &'static str {
    match machine_liveness(machine, now) {
        MachineLiveness::Fresh => "fresh",
        MachineLiveness::Stale => "stale",
        MachineLiveness::Down => "down",
    }
}

fn format_heartbeat(ts: u64, now: u64) -> String {
    if ts == 0 {
        return "never".into();
    }
    let ago = now.saturating_sub(ts);
    if ago < 60 {
        format!("{ago}s ago")
    } else if ago < 3600 {
        format!("{}m ago", ago / 60)
    } else if ago < 86400 {
        format!("{}h ago", ago / 3600)
    } else {
        format!("{}d ago", ago / 86400)
    }
}

fn format_timestamp(ts: u64) -> String {
    if ts == 0 {
        return "—".into();
    }
    DateTime::from_timestamp(ts as i64, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Mode;
    use crate::daemon::ActiveMesh;
    use crate::daemon::ssh::{TestSshEnvGuard, TestSshProgramGuard, test_ssh_env_lock};
    use crate::deploy::remote::RemoteControlHandle;
    use crate::mesh::driver::WireguardDriver;
    use crate::mesh::orchestrator::Mesh;
    use crate::mesh::wireguard::MemoryWireGuard;
    use crate::model::{OverlayIp, PublicKey};
    use crate::node::identity::Identity;
    use crate::store::backends::memory::{MemoryService, MemoryStore};
    use crate::store::network::{DEFAULT_CLUSTER_CIDR, NetworkConfig};
    use crate::time::now_unix_secs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn machine_list_shows_disabled_explicitly() {
        let (state, store, _) = make_state(false).await;
        let disabled = test_machine_record(
            "peer-disabled",
            "10.210.1.0/24",
            Participation::Disabled,
            0,
            PublicKey([2; 32]),
        );
        store
            .upsert_self_machine(&disabled)
            .await
            .expect("upsert disabled peer");

        let response = state.handle_machine_list().await;
        assert!(response.ok);
        assert!(response.message.contains("LIVENESS"));
        assert!(response.message.contains("peer-disabled"));
        assert!(response.message.contains("disabled"));
        assert!(response.message.contains("stale"));
    }

    #[tokio::test]
    async fn machine_list_shows_down_liveness() {
        let (state, store, _) = make_state(false).await;
        let mut down = test_machine_record(
            "peer-down",
            "10.210.1.0/24",
            Participation::Enabled,
            now_unix_secs(),
            PublicKey([2; 32]),
        );
        down.status = MachineStatus::Down;
        store
            .upsert_self_machine(&down)
            .await
            .expect("upsert down peer");

        let response = state.handle_machine_list().await;
        assert!(response.ok);
        assert!(response.message.contains("peer-down"));
        assert!(response.message.contains("down"));
    }

    #[tokio::test]
    async fn allocate_machine_subnets_returns_unique_values() {
        let (state, store, _) = make_state(false).await;
        store
            .upsert_self_machine(&test_machine_record(
                "peer-1",
                "10.210.1.0/24",
                Participation::Enabled,
                0,
                PublicKey([2; 32]),
            ))
            .await
            .expect("upsert existing peer");

        let subnets = state
            .allocate_machine_subnets(3)
            .await
            .expect("allocate subnets");

        assert_eq!(subnets.len(), 3);
        assert_eq!(
            subnets
                .iter()
                .map(ToString::to_string)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3
        );
        assert!(!subnets.contains(&"10.210.1.0/24".parse().expect("valid subnet")));
    }

    #[test]
    fn plan_local_subnet_heal_reassigns_losing_machine() {
        let machines = vec![
            test_machine_record(
                "alpha",
                "10.210.0.0/24",
                Participation::Enabled,
                0,
                PublicKey([2; 32]),
            ),
            test_machine_record(
                "beta",
                "10.210.1.0/24",
                Participation::Enabled,
                0,
                PublicKey([3; 32]),
            ),
            test_machine_record(
                "gamma",
                "10.210.1.0/24",
                Participation::Enabled,
                0,
                PublicKey([4; 32]),
            ),
        ];

        let plan = plan_local_subnet_heal(
            &machines,
            &MachineId("gamma".into()),
            DEFAULT_CLUSTER_CIDR,
            24,
        )
        .expect("plan should succeed")
        .expect("gamma should heal");

        assert_eq!(plan.current_subnet, "10.210.1.0/24".parse().expect("valid"));
        assert_eq!(plan.winner_machine_id, MachineId("beta".into()));
        assert_eq!(plan.target_subnet, "10.210.2.0/24".parse().expect("valid"));
    }

    #[test]
    fn plan_local_subnet_heal_keeps_winner_in_place() {
        let machines = vec![
            test_machine_record(
                "alpha",
                "10.210.0.0/24",
                Participation::Enabled,
                0,
                PublicKey([2; 32]),
            ),
            test_machine_record(
                "beta",
                "10.210.1.0/24",
                Participation::Enabled,
                0,
                PublicKey([3; 32]),
            ),
            test_machine_record(
                "gamma",
                "10.210.1.0/24",
                Participation::Enabled,
                0,
                PublicKey([4; 32]),
            ),
        ];

        let plan = plan_local_subnet_heal(
            &machines,
            &MachineId("beta".into()),
            DEFAULT_CLUSTER_CIDR,
            24,
        )
        .expect("plan should succeed");

        assert!(plan.is_none());
    }

    #[test]
    fn plan_local_subnet_heal_is_noop_after_subnet_changes() {
        let machines = vec![
            test_machine_record(
                "alpha",
                "10.210.0.0/24",
                Participation::Enabled,
                0,
                PublicKey([2; 32]),
            ),
            test_machine_record(
                "beta",
                "10.210.1.0/24",
                Participation::Enabled,
                0,
                PublicKey([3; 32]),
            ),
            test_machine_record(
                "gamma",
                "10.210.2.0/24",
                Participation::Enabled,
                0,
                PublicKey([4; 32]),
            ),
        ];

        let plan = plan_local_subnet_heal(
            &machines,
            &MachineId("gamma".into()),
            DEFAULT_CLUSTER_CIDR,
            24,
        )
        .expect("plan should succeed");

        assert!(plan.is_none());
    }

    #[tokio::test]
    async fn machine_add_warns_on_degraded_mesh_and_publishes_disabled_joiner() {
        let _guard = test_ssh_env_lock().lock().await;
        let (mut state, store, network) = make_state(true).await;
        store
            .upsert_self_machine(&test_machine_record(
                "stale-peer",
                "10.210.1.0/24",
                Participation::Enabled,
                0,
                PublicKey([3; 32]),
            ))
            .await
            .expect("upsert stale peer");

        let join_response = JoinResponse {
            machine_id: MachineId("joiner-1".into()),
            public_key: PublicKey([4; 32]),
            overlay_ip: "fd00::4".parse().map(OverlayIp).expect("valid overlay"),
            subnet: Some("10.210.2.0/24".parse().expect("valid subnet")),
            endpoints: vec!["203.0.113.10:51820".into()],
        }
        .encode()
        .expect("encode join response");

        let ssh_dir = unique_temp_dir("ployz-fake-ssh");
        std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
        let fake_ssh = write_fake_ssh(&ssh_dir);
        let _ssh_guard = TestSshProgramGuard::set(fake_ssh);
        let _join_guard =
            TestSshEnvGuard::set("PLOYZ_TEST_JOIN_RESPONSE", Some(join_response.into()));
        let _ready_guard =
            TestSshEnvGuard::set("PLOYZ_TEST_READY_RESPONSE", Some("{\"ready\":true}".into()));

        let response = state
            .handle_machine_add(&["join-target".into()], &MachineAddOptions::default())
            .await;
        assert!(response.ok, "{}", response.message);
        assert!(
            response
                .message
                .contains("warning: enabled peer 'stale-peer' has a stale heartbeat")
        );
        assert!(response.message.contains("awaiting_self_publication: 1"));

        let machines = store.list_machines().await.expect("list machines");
        assert!(
            !machines
                .into_iter()
                .any(|machine| machine.id.0 == "joiner-1")
        );
        assert!(
            network
                .current_peers()
                .into_iter()
                .any(|machine| machine.id.0 == "joiner-1")
        );

        teardown_state(&mut state).await;
    }

    #[tokio::test]
    async fn machine_add_accepts_running_joiner_before_full_sync() {
        let _guard = test_ssh_env_lock().lock().await;
        let (mut state, store, network) = make_state(true).await;

        let join_response = JoinResponse {
            machine_id: MachineId("joiner-2".into()),
            public_key: PublicKey([5; 32]),
            overlay_ip: "fd00::5".parse().map(OverlayIp).expect("valid overlay"),
            subnet: Some("10.210.1.0/24".parse().expect("valid subnet")),
            endpoints: vec!["203.0.113.11:51820".into()],
        }
        .encode()
        .expect("encode join response");

        let ssh_dir = unique_temp_dir("ployz-fake-ssh");
        std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
        let fake_ssh = write_fake_ssh(&ssh_dir);
        let _ssh_guard = TestSshProgramGuard::set(fake_ssh);
        let _join_guard =
            TestSshEnvGuard::set("PLOYZ_TEST_JOIN_RESPONSE", Some(join_response.into()));
        let _ready_guard = TestSshEnvGuard::set(
            "PLOYZ_TEST_READY_RESPONSE",
            Some(
                "{\"ready\":false,\"phase\":\"running\",\"store_healthy\":true,\"sync_connected\":false,\"heartbeat_started\":true}".into(),
            ),
        );

        let response = state
            .handle_machine_add(&["join-target".into()], &MachineAddOptions::default())
            .await;
        assert!(response.ok, "{}", response.message);
        assert!(response.message.contains("awaiting_self_publication: 1"));

        let machines = store.list_machines().await.expect("list machines");
        assert!(
            !machines
                .into_iter()
                .any(|machine| machine.id.0 == "joiner-2")
        );
        assert!(
            network
                .current_peers()
                .into_iter()
                .any(|machine| machine.id.0 == "joiner-2")
        );

        teardown_state(&mut state).await;
    }

    #[tokio::test]
    async fn machine_remove_refuses_enabled_without_force() {
        let (state, store, _) = make_state(false).await;
        store
            .upsert_self_machine(&test_machine_record(
                "peer-1",
                "10.210.1.0/24",
                Participation::Enabled,
                10,
                PublicKey([2; 32]),
            ))
            .await
            .expect("upsert peer");

        let response = state.handle_machine_remove("peer-1", false).await;
        assert!(!response.ok);
        assert!(response.message.contains("must be disabled"));
    }

    #[tokio::test]
    async fn machine_remove_deletes_disabled_record() {
        let (state, store, _) = make_state(false).await;
        store
            .upsert_self_machine(&test_machine_record(
                "peer-1",
                "10.210.1.0/24",
                Participation::Disabled,
                10,
                PublicKey([2; 32]),
            ))
            .await
            .expect("upsert peer");

        let response = state.handle_machine_remove("peer-1", false).await;
        assert!(response.ok, "{}", response.message);

        let machines = store.list_machines().await.expect("list machines");
        assert!(!machines.into_iter().any(|machine| machine.id.0 == "peer-1"));
    }

    #[tokio::test]
    async fn memory_mode_local_subnet_heal_updates_local_config_and_store() {
        let store = Arc::new(MemoryStore::new());
        store
            .upsert_self_machine(&test_machine_record(
                "founder",
                "10.210.1.0/24",
                Participation::Enabled,
                0,
                PublicKey([2; 32]),
            ))
            .await
            .expect("upsert founder");
        store
            .upsert_self_machine(&test_machine_record(
                "peer",
                "10.210.1.0/24",
                Participation::Enabled,
                0,
                PublicKey([3; 32]),
            ))
            .await
            .expect("upsert peer");

        let mut state = make_state_with_store(
            Identity::generate(MachineId("peer".into()), [3; 32]),
            "10.210.1.0/24",
            store.clone(),
        )
        .await;
        state
            .active
            .as_mut()
            .expect("active mesh")
            .mesh
            .up()
            .await
            .expect("mesh up");

        state.heal_local_subnet_conflict_if_needed().await;
        let Some(pending) = state.pending_subnet_heal else {
            panic!("expected pending subnet heal");
        };
        state.pending_subnet_heal = Some(PendingSubnetHeal {
            planned_at: pending.planned_at.saturating_sub(SUBNET_HEAL_SETTLE_SECS),
            ..pending
        });
        state.heal_local_subnet_conflict_if_needed().await;

        let healed_config = NetworkConfig::load(&NetworkConfig::path(&state.data_dir, "alpha"))
            .expect("load healed config");
        assert_eq!(
            healed_config.subnet,
            "10.210.0.0/24".parse().expect("valid")
        );
        let machines = store.list_machines().await.expect("list machines");
        let peer = machines
            .into_iter()
            .find(|machine| machine.id.0 == "peer")
            .expect("peer present");
        assert_eq!(peer.subnet, Some("10.210.0.0/24".parse().expect("valid")));
        assert_eq!(
            state
                .active
                .as_ref()
                .map(|active| active.config.subnet)
                .expect("active config present"),
            "10.210.0.0/24".parse().expect("valid")
        );

        teardown_state(&mut state).await;
    }

    #[tokio::test]
    async fn local_subnet_heal_skips_when_store_unhealthy() {
        let store = Arc::new(MemoryStore::new());
        store
            .upsert_self_machine(&test_machine_record(
                "founder",
                "10.210.1.0/24",
                Participation::Enabled,
                0,
                PublicKey([2; 32]),
            ))
            .await
            .expect("upsert founder");
        store
            .upsert_self_machine(&test_machine_record(
                "peer",
                "10.210.1.0/24",
                Participation::Enabled,
                0,
                PublicKey([3; 32]),
            ))
            .await
            .expect("upsert peer");

        let mut state = make_state_with_store(
            Identity::generate(MachineId("peer".into()), [3; 32]),
            "10.210.1.0/24",
            store,
        )
        .await;
        state
            .active
            .as_mut()
            .expect("active mesh")
            .mesh
            .up()
            .await
            .expect("mesh up");

        let service = match &state.active.as_ref().expect("active").mesh.store {
            StoreDriver::Memory { service, .. } => service.clone(),
            StoreDriver::Corrosion { .. } | StoreDriver::CorrosionHost { .. } => {
                panic!("expected memory store")
            }
        };
        service.set_healthy(false);

        state.heal_local_subnet_conflict_if_needed().await;

        let healed_config = NetworkConfig::load(&NetworkConfig::path(&state.data_dir, "alpha"))
            .expect("load config");
        assert_eq!(
            healed_config.subnet,
            "10.210.1.0/24".parse().expect("valid")
        );

        teardown_state(&mut state).await;
    }

    #[tokio::test]
    async fn local_subnet_heal_skips_when_mesh_not_running() {
        let store = Arc::new(MemoryStore::new());
        store
            .upsert_self_machine(&test_machine_record(
                "founder",
                "10.210.1.0/24",
                Participation::Enabled,
                0,
                PublicKey([2; 32]),
            ))
            .await
            .expect("upsert founder");
        store
            .upsert_self_machine(&test_machine_record(
                "peer",
                "10.210.1.0/24",
                Participation::Enabled,
                0,
                PublicKey([3; 32]),
            ))
            .await
            .expect("upsert peer");

        let mut state = make_state_with_store(
            Identity::generate(MachineId("peer".into()), [3; 32]),
            "10.210.1.0/24",
            store,
        )
        .await;

        state.heal_local_subnet_conflict_if_needed().await;

        let healed_config = NetworkConfig::load(&NetworkConfig::path(&state.data_dir, "alpha"))
            .expect("load config");
        assert_eq!(
            healed_config.subnet,
            "10.210.1.0/24".parse().expect("valid")
        );
    }

    async fn make_state(start_mesh: bool) -> (DaemonState, Arc<MemoryStore>, Arc<MemoryWireGuard>) {
        let identity = Identity::generate(MachineId("founder".into()), [1; 32]);
        let founder_subnet: Ipv4Net = "10.210.0.0/24".parse().expect("valid subnet");
        let config = NetworkConfig::new(
            crate::model::NetworkName("alpha".into()),
            &identity.public_key,
            DEFAULT_CLUSTER_CIDR,
            founder_subnet,
        );

        let store = Arc::new(MemoryStore::new());
        let service = Arc::new(MemoryService::new());
        let network = Arc::new(MemoryWireGuard::new());
        let founder_record = test_machine_record(
            "founder",
            "10.210.0.0/24",
            Participation::Disabled,
            0,
            identity.public_key.clone(),
        );
        store
            .upsert_self_machine(&founder_record)
            .await
            .expect("upsert founder");

        let mut mesh = Mesh::new(
            WireguardDriver::Memory(network.clone()),
            StoreDriver::Memory {
                store: store.clone(),
                service,
            },
            None,
            identity.machine_id.clone(),
            51820,
        );
        if start_mesh {
            mesh.up().await.expect("mesh up");
        }

        let mut state = DaemonState::new(
            &unique_temp_dir("ployz-machine-state"),
            identity,
            Mode::Memory,
            DEFAULT_CLUSTER_CIDR.into(),
            24,
            4317,
            "127.0.0.1:0".into(),
            1,
        );
        state.active = Some(ActiveMesh {
            config,
            mesh,
            remote_control: RemoteControlHandle::noop(),
            gateway: crate::services::gateway::GatewayHandle::noop(),
            dns: crate::services::dns::DnsHandle::noop(),
        });

        (state, store, network)
    }

    async fn make_state_with_store(
        identity: Identity,
        subnet: &str,
        store: Arc<MemoryStore>,
    ) -> DaemonState {
        let subnet: Ipv4Net = subnet.parse().expect("valid subnet");
        let data_dir = unique_temp_dir("ployz-machine-heal-state");
        let config = NetworkConfig::new(
            crate::model::NetworkName("alpha".into()),
            &identity.public_key,
            DEFAULT_CLUSTER_CIDR,
            subnet,
        );
        config
            .save(&NetworkConfig::path(&data_dir, "alpha"))
            .expect("save config");

        let mesh = Mesh::new(
            WireguardDriver::Memory(Arc::new(MemoryWireGuard::new())),
            StoreDriver::Memory {
                store,
                service: Arc::new(MemoryService::new()),
            },
            None,
            identity.machine_id.clone(),
            51820,
        );

        let mut state = DaemonState::new(
            &data_dir,
            identity,
            Mode::Memory,
            DEFAULT_CLUSTER_CIDR.into(),
            24,
            4317,
            "127.0.0.1:0".into(),
            1,
        );
        state.active = Some(ActiveMesh {
            config,
            mesh,
            remote_control: RemoteControlHandle::noop(),
            gateway: crate::services::gateway::GatewayHandle::noop(),
            dns: crate::services::dns::DnsHandle::noop(),
        });
        state
    }

    async fn teardown_state(state: &mut DaemonState) {
        let Some(active) = state.active.as_mut() else {
            return;
        };
        active.mesh.destroy().await.expect("destroy mesh");
    }

    fn test_machine_record(
        id: &str,
        subnet: &str,
        participation: Participation,
        last_heartbeat: u64,
        public_key: PublicKey,
    ) -> MachineRecord {
        MachineRecord {
            id: MachineId(id.into()),
            public_key,
            overlay_ip: format!("fd00::{id_len:x}", id_len = id.len())
                .parse()
                .map(OverlayIp)
                .expect("valid overlay"),
            subnet: Some(subnet.parse().expect("valid subnet")),
            bridge_ip: None,
            endpoints: vec!["127.0.0.1:51820".into()],
            status: MachineStatus::Unknown,
            participation,
            last_heartbeat,
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        }
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
    }

    fn write_fake_ssh(dir: &std::path::Path) -> PathBuf {
        let script = dir.join("ssh");
        std::fs::write(
            &script,
            "#!/bin/sh\nfor arg in \"$@\"; do\n  command=\"$arg\"\ndone\ncase \"$command\" in\n  *\"mesh join --token-stdin\"*)\n    cat >/dev/null\n    exit 0\n    ;;\n  *\"mesh init --name-stdin\"*)\n    cat >/dev/null\n    exit 0\n    ;;\n  *\"mesh destroy --name-stdin\"*)\n    cat >/dev/null\n    exit 0\n    ;;\n  *\"mesh self-record\"*)\n    printf '%s' \"$PLOYZ_TEST_JOIN_RESPONSE\"\n    exit 0\n    ;;\n  *\"mesh ready --json\"*)\n    printf '%s' \"$PLOYZ_TEST_READY_RESPONSE\"\n    exit 0\n    ;;\n  *)\n    exit 0\n    ;;\nesac\n",
        )
        .expect("write fake ssh");

        #[cfg(unix)]
        {
            let mut permissions = std::fs::metadata(&script)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script, permissions).expect("set script permissions");
        }

        script
    }
}
