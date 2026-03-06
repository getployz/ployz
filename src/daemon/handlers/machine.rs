use crate::model::{JOIN_RESPONSE_PREFIX, JoinResponse, MachineRecord, MachineStatus};
use crate::store::MachineStore;
use crate::transport::DaemonResponse;
use chrono::DateTime;

use super::super::DaemonState;
use super::super::ssh::{run_ssh, shell_escape};

impl DaemonState {
    pub(crate) async fn handle_machine_list(&self) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(a) => a,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let machines = match active.mesh.store.list_machines().await {
            Ok(m) => m,
            Err(e) => return self.err("LIST_FAILED", format!("failed to list machines: {e}")),
        };

        if machines.is_empty() {
            return self.ok("no machines");
        }

        let now = chrono::Utc::now().timestamp() as u64;

        struct Row {
            id: String,
            status: &'static str,
            sched: &'static str,
            overlay: String,
            subnet: String,
            heartbeat: String,
            created: String,
        }

        let rows: Vec<Row> = machines
            .iter()
            .map(|m| Row {
                id: m.id.0.clone(),
                status: format_status(m),
                sched: format_scheduling(m),
                overlay: m.overlay_ip.0.to_string(),
                subnet: m.subnet.map(|s| s.to_string()).unwrap_or_else(|| "—".into()),
                heartbeat: format_heartbeat(m.last_heartbeat, now),
                created: format_timestamp(m.created_at),
            })
            .collect();

        let w_id = rows.iter().map(|r| r.id.len()).max().unwrap_or(0).max(2);
        let w_ov = rows.iter().map(|r| r.overlay.len()).max().unwrap_or(0).max(10);
        let w_sub = rows.iter().map(|r| r.subnet.len()).max().unwrap_or(0).max(6);
        let w_hb = rows.iter().map(|r| r.heartbeat.len()).max().unwrap_or(0).max(9);

        let mut lines = Vec::with_capacity(rows.len() + 1);
        lines.push(format!(
            "{:<w_id$}  {:<6}  {:<8}  {:<w_ov$}  {:<w_sub$}  {:<w_hb$}  {}",
            "ID", "STATUS", "SCHED", "OVERLAY IP", "SUBNET", "HEARTBEAT", "CREATED",
        ));
        for r in &rows {
            lines.push(format!(
                "{:<w_id$}  {:<6}  {:<8}  {:<w_ov$}  {:<w_sub$}  {:<w_hb$}  {}",
                r.id, r.status, r.sched, r.overlay, r.subnet, r.heartbeat, r.created,
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

        let bootstrap = "set -eu; command -v ployzd >/dev/null 2>&1 || { echo 'ployzd not installed'; exit 12; }; command -v docker >/dev/null 2>&1 || { echo 'docker not installed'; exit 13; };";
        if let Err(err) = run_ssh(target, bootstrap).await {
            return self.err("SSH_BOOTSTRAP_FAILED", err);
        }

        let init_cmd = format!("set -eu; ployz mesh init \"{}\"", shell_escape(network));
        if let Err(err) = run_ssh(target, &init_cmd).await {
            return self.err("REMOTE_INIT_FAILED", err);
        }

        self.ok(format!(
            "remote founder initialized\n  target:  {target}\n  network: {network}"
        ))
    }

    pub(crate) async fn handle_machine_add(&mut self, target: &str) -> DaemonResponse {
        let running = match self.active.as_ref() {
            Some(active) => active.config.clone(),
            None => {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "machine add requires a running network on this daemon",
                );
            }
        };

        let token = match self.do_issue_invite_token(&running, 600).await {
            Ok(token) => token,
            Err(err) => return self.err("INVITE_CREATE_FAILED", err),
        };

        let bootstrap = "set -eu; command -v ployzd >/dev/null 2>&1 || { echo 'ployzd not installed'; exit 12; }; command -v ployz >/dev/null 2>&1 || { echo 'ployz not installed'; exit 13; }; ployz status >/dev/null 2>&1 || { echo 'ployzd not running'; exit 15; };";
        if let Err(err) = run_ssh(target, bootstrap).await {
            return self.err("SSH_BOOTSTRAP_FAILED", err);
        }

        // Step 1: Join the mesh (idempotent — already-joined errors are OK)
        let join_cmd = format!(
            "set -eu; ployz mesh join --token \"{}\"",
            shell_escape(&token)
        );
        match run_ssh(target, &join_cmd).await {
            Ok(_) => {}
            Err(err) if err.contains("already exists") || err.contains("already running") => {
                tracing::info!(target, "remote already joined — continuing to self-record");
            }
            Err(err) => return self.err("REMOTE_JOIN_FAILED", err),
        }

        // Step 2: Get the joiner's identity via self-record
        let self_record_cmd = "ployz mesh self-record";
        let sr_output = match run_ssh(target, self_record_cmd).await {
            Ok(out) => out,
            Err(err) => {
                return self.err(
                    "SELF_RECORD_FAILED",
                    format!("{err}\nhint: run `ployz mesh self-record` on the joiner and `ployz mesh accept <response>` on this machine"),
                );
            }
        };

        // Parse the PLOYZ_JOIN_RESPONSE line from stdout
        let response_line = match sr_output
            .lines()
            .find(|l| l.starts_with(JOIN_RESPONSE_PREFIX))
        {
            Some(line) => line,
            None => {
                return self.err(
                    "SELF_RECORD_FAILED",
                    format!("self-record output missing {JOIN_RESPONSE_PREFIX} line\nhint: run `ployz mesh self-record` on the joiner and `ployz mesh accept <response>` on this machine"),
                );
            }
        };

        let join_resp = match JoinResponse::decode(response_line) {
            Ok(r) => r,
            Err(e) => {
                return self.err(
                    "SELF_RECORD_FAILED",
                    format!("failed to decode join response: {e}"),
                );
            }
        };

        // Step 3: Seed founder's store with joiner's record
        let record = join_resp.into_machine_record();
        let joiner_id = record.id.clone();
        let active = self.active.as_ref().unwrap();
        if let Err(e) = active.mesh.store.upsert_machine(&record).await {
            return self.err(
                "UPSERT_FAILED",
                format!("failed to seed joiner record: {e}"),
            );
        }

        self.ok(format!(
            "machine add completed\n  target:  {target}\n  joiner:  {joiner_id}\n  network: {}",
            running.name,
        ))
    }
}

fn format_status(m: &MachineRecord) -> &'static str {
    match m.status {
        MachineStatus::Up => "up",
        MachineStatus::Down => "down",
        MachineStatus::Unknown => "—",
    }
}

fn format_scheduling(m: &MachineRecord) -> &'static str {
    match m.scheduling {
        crate::model::Scheduling::Enabled => "enabled",
        crate::model::Scheduling::Draining => "draining",
        crate::model::Scheduling::Disabled => "—",
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
