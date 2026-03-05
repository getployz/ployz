use crate::store::MachineStore;
use crate::transport::DaemonResponse;

use super::super::DaemonState;
use super::super::ssh::{run_ssh, shell_escape};

impl DaemonState {
    pub(crate) async fn handle_machine_list(&self) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(a) => a,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let machines = match active.mesh.store().list_machines().await {
            Ok(m) => m,
            Err(e) => return self.err("LIST_FAILED", format!("failed to list machines: {e}")),
        };

        if machines.is_empty() {
            return self.ok("no machines");
        }

        let lines: Vec<String> = machines
            .iter()
            .map(|m| format!("{}  {}  {}", m.id, m.overlay_ip, m.endpoints.join(",")))
            .collect();
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

        let bootstrap = "set -eu; command -v ployzd >/dev/null 2>&1 || { echo 'ployzd not installed'; exit 12; }; command -v ployz >/dev/null 2>&1 || { echo 'ployz not installed'; exit 13; }; command -v docker >/dev/null 2>&1 || { echo 'docker not installed'; exit 14; }; ployz status >/dev/null 2>&1 || { echo 'ployzd not running'; exit 15; };";
        if let Err(err) = run_ssh(target, bootstrap).await {
            return self.err("SSH_BOOTSTRAP_FAILED", err);
        }

        let import_cmd = format!(
            "set -eu; ployz machine invite import --token \"{}\"",
            shell_escape(&token)
        );
        if let Err(err) = run_ssh(target, &import_cmd).await {
            return self.err("REMOTE_INVITE_IMPORT_FAILED", err);
        }

        let join_cmd = format!(
            "set -eu; ployz mesh join --token \"{}\"",
            shell_escape(&token)
        );
        if let Err(err) = run_ssh(target, &join_cmd).await {
            return self.err("REMOTE_JOIN_FAILED", err);
        }

        self.ok(format!(
            "machine add completed\n  target:  {target}\n  network: {}",
            running.name,
        ))
    }
}
