use crate::machine_liveness::{MachineLiveness, machine_liveness};
use crate::mesh::tasks::PeerSyncCommand;
use crate::model::{
    JOIN_RESPONSE_PREFIX, JoinResponse, MachineId, MachineRecord, MachineStatus, Participation,
};
use crate::network::ipam::Ipam;
use crate::store::MachineStore;
use crate::store::driver::StoreDriver;
use chrono::DateTime;
use ipnet::Ipv4Net;
use ployz_sdk::transport::{DaemonResponse, MachineAddOptions};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{Duration, Instant, sleep, timeout};

use super::super::DaemonState;
use super::super::ssh::{EphemeralSshIdentityFile, SshOptions, run_ssh, run_ssh_with_stdin};
use crate::time::now_unix_secs;

const INVITE_TTL_SECS: u64 = 600;
const REMOTE_READY_TIMEOUT: Duration = Duration::from_secs(30);
const REMOTE_READY_POLL_INTERVAL: Duration = Duration::from_secs(1);
const REMOTE_READY_SSH_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_CLEANUP_SSH_TIMEOUT: Duration = Duration::from_secs(10);
const MACHINE_INIT_BOOTSTRAP_COMMAND: &str = "set -eu; command -v ployzd >/dev/null 2>&1 || { echo 'ployzd not installed'; exit 12; }; command -v docker >/dev/null 2>&1 || { echo 'docker not installed'; exit 13; }; sudo -n ployzd status >/dev/null 2>&1 || { echo 'ployzd not running under sudo'; exit 15; };";
const MACHINE_ADD_BOOTSTRAP_COMMAND: &str = "set -eu; command -v ployzd >/dev/null 2>&1 || { echo 'ployzd not installed'; exit 12; }; sudo -n ployzd status >/dev/null 2>&1 || { echo 'ployzd not running under sudo'; exit 15; };";
const REMOTE_MESH_INIT_COMMAND: &str = "set -eu; sudo -n ployzd mesh init --name-stdin";
const REMOTE_MESH_JOIN_COMMAND: &str = "set -eu; sudo -n ployzd mesh join --token-stdin";
const REMOTE_MESH_SELF_RECORD_COMMAND: &str = "set -eu; sudo -n ployzd mesh self-record";
const REMOTE_MESH_READY_COMMAND: &str =
    "set -eu; sudo -n ployzd mesh ready --json | jq -r '.message'";
const REMOTE_MESH_DOWN_COMMAND: &str = "set -eu; sudo -n ployzd mesh down";
const REMOTE_MESH_DESTROY_COMMAND: &str = "set -eu; sudo -n ployzd mesh destroy --name-stdin";

#[derive(Clone)]
struct MachineAddContext {
    network_name: String,
    store: StoreDriver,
    peer_sync_tx: mpsc::Sender<PeerSyncCommand>,
    ssh_options: SshOptions,
}

#[derive(Debug)]
enum MachineAddOutcome {
    PublishedDisabled {
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
    FailedPublish {
        target: String,
        reason: String,
    },
}

#[derive(Debug, Default)]
struct MachineAddSummary {
    published_disabled: Vec<String>,
    failed_preflight: Vec<String>,
    failed_join: Vec<String>,
    failed_self_record: Vec<String>,
    failed_ready: Vec<String>,
    failed_publish: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RemoteReadyPayload {
    ready: bool,
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
        &mut self,
        target: &str,
        network: &str,
    ) -> DaemonResponse {
        if self.active.is_some() {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                "machine init requires no local running network; switch context or run `mesh down` first",
            );
        }

        if let Err(err) = run_ssh(target, MACHINE_INIT_BOOTSTRAP_COMMAND, &SshOptions::default()).await {
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
        &mut self,
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
        tracing::info!(warning_count = warnings.len(), "machine add degraded-mesh check complete");

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

            let task_context = context.clone();
            tasks.spawn(async move { run_machine_add_target(task_context, target, token).await });
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

    pub(crate) async fn handle_machine_drain(&self, id: &str) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let machine_id = MachineId(id.to_string());
        let mut record = match find_machine_record(&active.mesh.store, &machine_id).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                return self.err("MACHINE_NOT_FOUND", format!("machine '{id}' not found"));
            }
            Err(err) => {
                return self.err("LIST_FAILED", format!("failed to read machines: {err}"));
            }
        };

        record.participation = Participation::Draining;
        record.updated_at = now_unix_secs();

        match active.mesh.store.upsert_machine(&record).await {
            Ok(()) => self.ok(format!("machine '{id}' marked draining")),
            Err(err) => self.err(
                "UPSERT_FAILED",
                format!("failed to update machine participation: {err}"),
            ),
        }
    }

    pub(crate) async fn handle_machine_label(
        &self,
        id: &str,
        set: &[(String, String)],
        remove: &[String],
    ) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let resolved_id = if id == "self" {
            self.identity.machine_id.clone()
        } else {
            MachineId(id.to_string())
        };

        let mut record = match find_machine_record(&active.mesh.store, &resolved_id).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                return self.err("MACHINE_NOT_FOUND", format!("machine '{id}' not found"));
            }
            Err(err) => {
                return self.err("LIST_FAILED", format!("failed to read machines: {err}"));
            }
        };

        for (key, value) in set {
            record.labels.insert(key.clone(), value.clone());
        }
        for key in remove {
            record.labels.remove(key);
        }
        record.updated_at = now_unix_secs();

        match active.mesh.store.upsert_machine(&record).await {
            Ok(()) => {
                let labels_display: Vec<String> = record
                    .labels
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect();
                if labels_display.is_empty() {
                    self.ok(format!("machine '{}' labels cleared", resolved_id))
                } else {
                    self.ok(format!(
                        "machine '{}' labels: {}",
                        resolved_id,
                        labels_display.join(", ")
                    ))
                }
            }
            Err(err) => self.err(
                "UPSERT_FAILED",
                format!("failed to update machine labels: {err}"),
            ),
        }
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
            MachineAddOutcome::PublishedDisabled { target, joiner_id } => {
                self.published_disabled
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
            MachineAddOutcome::FailedPublish { target, reason } => {
                self.failed_publish.push(format!("{target}: {reason}"));
            }
        }
    }

    fn has_failures(&self) -> bool {
        !self.failed_preflight.is_empty()
            || !self.failed_join.is_empty()
            || !self.failed_self_record.is_empty()
            || !self.failed_ready.is_empty()
            || !self.failed_publish.is_empty()
    }

    fn into_response(self, state: &DaemonState, warnings: &[String]) -> DaemonResponse {
        let mut lines = Vec::new();
        if !warnings.is_empty() {
            lines.extend(warnings.iter().cloned());
            lines.push(String::new());
        }

        lines.push("machine add summary".into());
        push_summary_section(&mut lines, "published_disabled", &self.published_disabled);
        push_summary_section(&mut lines, "failed_preflight", &self.failed_preflight);
        push_summary_section(&mut lines, "failed_join", &self.failed_join);
        push_summary_section(&mut lines, "failed_self_record", &self.failed_self_record);
        push_summary_section(&mut lines, "failed_ready", &self.failed_ready);
        push_summary_section(&mut lines, "failed_publish", &self.failed_publish);

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
    token: String,
) -> MachineAddOutcome {
    tracing::info!(%target, "machine add target: ssh preflight starting");
    if let Err(err) = run_ssh(&target, MACHINE_ADD_BOOTSTRAP_COMMAND, &context.ssh_options).await {
        return MachineAddOutcome::FailedPreflight {
            target,
            reason: err,
        };
    }
    tracing::info!(%target, "machine add target: ssh preflight complete");

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
                    "{err}\nhint: run `ployzd mesh self-record` on the joiner and `ployzd mesh accept <response>` on this machine"
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

    let joiner_id = record.id.clone();
    tracing::info!(%target, joiner_id = %joiner_id, "machine add target: transient peer install starting");
    if let Err(err) = upsert_transient_peer(&context.peer_sync_tx, record.clone()).await {
        let _ = best_effort_remote_cleanup(&target, &context.network_name, &context.ssh_options)
            .await;
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

    tracing::info!(%target, joiner_id = %joiner_id, "machine add target: publishing machine record");
    if let Err(err) = context.store.upsert_machine(&record).await {
        let _ = remove_transient_peer(&context.peer_sync_tx, &joiner_id).await;
        let _ = best_effort_remote_cleanup(&target, &context.network_name, &context.ssh_options)
            .await;
        return MachineAddOutcome::FailedPublish {
            target,
            reason: format!("failed to publish joiner record: {err}"),
        };
    }
    tracing::info!(%target, joiner_id = %joiner_id, "machine add target: published disabled");

    MachineAddOutcome::PublishedDisabled { target, joiner_id }
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
        let last_error =
            match timeout(
                REMOTE_READY_SSH_TIMEOUT,
                run_ssh(target, REMOTE_MESH_READY_COMMAND, ssh_options),
            )
            .await
            {
                Ok(Ok(output)) => match serde_json::from_str::<RemoteReadyPayload>(&output) {
                Ok(payload) => {
                    if payload.ready {
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
                "self-record output missing {JOIN_RESPONSE_PREFIX} line\nhint: run `ployzd mesh self-record` on the joiner and `ployzd mesh accept <response>` on this machine"
            ));
        }
    };

    let join_response = JoinResponse::decode(response_line)
        .map_err(|err| format!("failed to decode join response: {err}"))?;
    Ok(join_response.into_machine_record())
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
    use crate::daemon::ssh::{TEST_SSH_BIN_ENV, test_ssh_env_lock};
    use crate::deploy::remote::RemoteControlHandle;
    use crate::mesh::driver::WireguardDriver;
    use crate::mesh::orchestrator::Mesh;
    use crate::mesh::wireguard::MemoryWireGuard;
    use crate::model::{OverlayIp, PublicKey};
    use crate::node::identity::Identity;
    use crate::store::backends::memory::{MemoryService, MemoryStore};
    use crate::store::network::{DEFAULT_CLUSTER_CIDR, NetworkConfig};
    use crate::time::now_unix_secs;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn machine_list_shows_disabled_explicitly() {
        let (state, store) = make_state(false).await;
        let disabled = test_machine_record(
            "peer-disabled",
            "10.210.1.0/24",
            Participation::Disabled,
            0,
            PublicKey([2; 32]),
        );
        store
            .upsert_machine(&disabled)
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
        let (state, store) = make_state(false).await;
        let mut down = test_machine_record(
            "peer-down",
            "10.210.1.0/24",
            Participation::Enabled,
            now_unix_secs(),
            PublicKey([2; 32]),
        );
        down.status = MachineStatus::Down;
        store.upsert_machine(&down).await.expect("upsert down peer");

        let response = state.handle_machine_list().await;
        assert!(response.ok);
        assert!(response.message.contains("peer-down"));
        assert!(response.message.contains("down"));
    }

    #[tokio::test]
    async fn allocate_machine_subnets_returns_unique_values() {
        let (state, store) = make_state(false).await;
        store
            .upsert_machine(&test_machine_record(
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

    #[tokio::test]
    async fn machine_add_warns_on_degraded_mesh_and_publishes_disabled_joiner() {
        let _guard = test_ssh_env_lock().lock().expect("env lock");
        let (mut state, store) = make_state(true).await;
        store
            .upsert_machine(&test_machine_record(
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
            subnet: Some("10.210.9.0/24".parse().expect("valid subnet")),
            endpoints: vec!["203.0.113.10:51820".into()],
        }
        .encode()
        .expect("encode join response");

        let ssh_dir = unique_temp_dir("ployz-fake-ssh");
        std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
        let fake_ssh = write_fake_ssh(&ssh_dir);
        let _ssh_guard = EnvVarGuard::set(TEST_SSH_BIN_ENV, Some(fake_ssh.into_os_string()));
        let _join_guard = EnvVarGuard::set("PLOYZ_TEST_JOIN_RESPONSE", Some(join_response.into()));
        let _ready_guard =
            EnvVarGuard::set("PLOYZ_TEST_READY_RESPONSE", Some("{\"ready\":true}".into()));

        let response = state
            .handle_machine_add(&["join-target".into()], &MachineAddOptions::default())
            .await;
        assert!(response.ok, "{}", response.message);
        assert!(
            response
                .message
                .contains("warning: enabled peer 'stale-peer' has a stale heartbeat")
        );
        assert!(response.message.contains("published_disabled: 1"));

        let machines = store.list_machines().await.expect("list machines");
        let joiner = machines
            .into_iter()
            .find(|machine| machine.id.0 == "joiner-1")
            .expect("joiner published");
        assert_eq!(joiner.participation, Participation::Disabled);

        teardown_state(&mut state).await;
    }

    #[tokio::test]
    async fn machine_drain_updates_participation_and_keeps_record() {
        let (state, store) = make_state(false).await;
        store
            .upsert_machine(&test_machine_record(
                "peer-1",
                "10.210.1.0/24",
                Participation::Enabled,
                10,
                PublicKey([2; 32]),
            ))
            .await
            .expect("upsert peer");

        let response = state.handle_machine_drain("peer-1").await;
        assert!(response.ok, "{}", response.message);

        let machines = store.list_machines().await.expect("list machines");
        let peer = machines
            .into_iter()
            .find(|machine| machine.id.0 == "peer-1")
            .expect("peer present");
        assert_eq!(peer.participation, Participation::Draining);
    }

    #[tokio::test]
    async fn machine_remove_refuses_enabled_without_force() {
        let (state, store) = make_state(false).await;
        store
            .upsert_machine(&test_machine_record(
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
        let (state, store) = make_state(false).await;
        store
            .upsert_machine(&test_machine_record(
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

    async fn make_state(start_mesh: bool) -> (DaemonState, Arc<MemoryStore>) {
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
            .upsert_machine(&founder_record)
            .await
            .expect("upsert founder");

        let mut mesh = Mesh::new(
            WireguardDriver::Memory(network),
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

        (state, store)
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

    fn write_fake_ssh(dir: &PathBuf) -> PathBuf {
        let script = dir.join("ssh");
        std::fs::write(
            &script,
            "#!/bin/sh\ncommand=\"$2\"\ncase \"$command\" in\n  *\"mesh join --token-stdin\"*)\n    cat >/dev/null\n    exit 0\n    ;;\n  *\"mesh init --name-stdin\"*)\n    cat >/dev/null\n    exit 0\n    ;;\n  *\"mesh destroy --name-stdin\"*)\n    cat >/dev/null\n    exit 0\n    ;;\n  *\"mesh self-record\"*)\n    printf '%s' \"$PLOYZ_TEST_JOIN_RESPONSE\"\n    exit 0\n    ;;\n  *\"mesh ready --json\"*)\n    printf '%s' \"$PLOYZ_TEST_READY_RESPONSE\"\n    exit 0\n    ;;\n  *)\n    exit 0\n    ;;\nesac\n",
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

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<OsString>) -> Self {
            let previous = std::env::var_os(key);
            match value {
                Some(value) => {
                    // Tests serialize PATH/env mutations behind a process-wide mutex.
                    unsafe { std::env::set_var(key, value) }
                }
                None => {
                    // Tests serialize PATH/env mutations behind a process-wide mutex.
                    unsafe { std::env::remove_var(key) }
                }
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.as_ref() {
                Some(value) => {
                    // Tests serialize PATH/env mutations behind a process-wide mutex.
                    unsafe { std::env::set_var(self.key, value) }
                }
                None => {
                    // Tests serialize PATH/env mutations behind a process-wide mutex.
                    unsafe { std::env::remove_var(self.key) }
                }
            }
        }
    }
}
