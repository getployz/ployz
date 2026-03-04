use crate::node::invite::{issue_invite_token, parse_and_verify_invite_token};
use crate::store::{InviteStore, MachineStore};
use crate::store::model::{InviteRecord, NetworkName};
use crate::store::network::NetworkConfig;
use crate::transport::{DaemonRequest, DaemonResponse};

use super::ssh::{now_unix_secs, run_ssh, shell_escape};
use super::DaemonState;

impl DaemonState {
    pub async fn handle(&mut self, req: DaemonRequest) -> DaemonResponse {
        match req {
            DaemonRequest::Status => self.handle_status(),
            DaemonRequest::MeshList => self.handle_mesh_list(),
            DaemonRequest::MeshStatus { network } => self.handle_mesh_status(&network),
            DaemonRequest::MeshJoin { token } => self.handle_mesh_join(&token).await,
            DaemonRequest::MeshCreate { network } => self.handle_mesh_create(&network),
            DaemonRequest::MeshInit { network } => self.handle_mesh_init(&network).await,
            DaemonRequest::MeshUp { network } => self.handle_mesh_up(&network).await,
            DaemonRequest::MeshDown => self.handle_mesh_down().await,
            DaemonRequest::MeshDestroy { network } => self.handle_mesh_destroy(&network).await,
            DaemonRequest::MachineList => self.handle_machine_list().await,
            DaemonRequest::MachineInit { target, network } => {
                self.handle_machine_init(&target, &network).await
            }
            DaemonRequest::MachineAdd { target } => self.handle_machine_add(&target).await,
            DaemonRequest::MachineInviteCreate { ttl_secs } => {
                self.handle_machine_invite_create(ttl_secs).await
            }
            DaemonRequest::MachineInviteImport { token } => {
                self.handle_machine_invite_import(&token).await
            }
        }
    }

    fn handle_status(&self) -> DaemonResponse {
        let id = &self.identity;
        match &self.active {
            Some(active) => {
                let net = &active.config;
                self.ok(format!(
                    "machine:  {}\nnetwork:  {}\noverlay:  {}\nphase:    {:?}",
                    id.machine_id,
                    net.name,
                    net.overlay_ip,
                    active.mesh.phase(),
                ))
            }
            None => self.ok(format!(
                "machine:  {}\nnetwork:  none\nphase:    idle",
                id.machine_id,
            )),
        }
    }

    fn handle_mesh_list(&self) -> DaemonResponse {
        let networks_dir = self.data_dir.join("networks");
        let entries = match std::fs::read_dir(&networks_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return self.ok("no networks found");
            }
            Err(err) => {
                return self.err("IO_ERROR", format!("failed to read networks dir: {err}"));
            }
        };

        let mut names: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().to_str().map(ToOwned::to_owned))
            .collect();
        names.sort();

        if names.is_empty() {
            return self.ok("no networks found");
        }

        let running = self.active.as_ref().map(|a| a.config.name.0.as_str());
        let lines: Vec<String> = names
            .iter()
            .map(|name| {
                let state = if running == Some(name.as_str()) {
                    "running"
                } else {
                    "created"
                };
                format!("{name}: {state}")
            })
            .collect();

        self.ok(lines.join("\n"))
    }

    fn handle_mesh_status(&self, network: &str) -> DaemonResponse {
        let config_path = NetworkConfig::path(&self.data_dir, network);
        if !config_path.exists() {
            return self.err(
                "NETWORK_NOT_FOUND",
                format!("network '{network}' does not exist"),
            );
        }

        let config = match NetworkConfig::load(&config_path) {
            Ok(config) => config,
            Err(err) => {
                return self.err("IO_ERROR", format!("failed to load network config: {err}"));
            }
        };

        let running = self
            .active
            .as_ref()
            .map(|a| a.config.name.0 == network)
            .unwrap_or(false);
        let state = if running { "running" } else { "created" };
        self.ok(format!(
            "network: {}\noverlay: {}\nstate:   {}",
            config.name, config.overlay_ip, state
        ))
    }

    async fn handle_mesh_join(&mut self, token: &str) -> DaemonResponse {
        if let Some(active) = &self.active {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh down` first",
                    active.config.name
                ),
            );
        }

        let invite = match parse_and_verify_invite_token(token) {
            Ok(invite) => invite,
            Err(err) => return self.err("INVALID_INVITE_TOKEN", err),
        };

        let now = now_unix_secs();
        if now > invite.expires_at {
            return self.err("INVITE_EXPIRED", "invite token has expired");
        }

        let network = invite.network_name.trim();
        if network.is_empty() {
            return self.err("INVALID_INVITE_TOKEN", "invite token network name is empty");
        }

        let config_path = NetworkConfig::path(&self.data_dir, network);
        if config_path.exists() {
            return self.err(
                "NETWORK_ALREADY_EXISTS",
                format!("network '{network}' already exists -- destroy it first"),
            );
        }

        let mut net_config =
            NetworkConfig::new(NetworkName(network.to_string()), &self.identity.public_key);
        net_config.id = invite.network_id.clone();

        if let Err(e) = net_config.save(&config_path) {
            return self.err("IO_ERROR", format!("failed to save network config: {e}"));
        }

        if let Err(e) = self.start_mesh(net_config).await {
            return self.err(
                "NETWORK_START_FAILED",
                format!("join failed to start mesh: {e}"),
            );
        }

        if let Err(e) = self
            .active
            .as_ref()
            .unwrap()
            .mesh
            .store()
            .consume_invite(&invite.invite_id, now_unix_secs())
            .await
        {
            tracing::warn!(?e, "failed to consume invite (mesh already joined)");
        }

        self.ok(format!("joined and started network '{network}'"))
    }

    fn handle_mesh_create(&self, network: &str) -> DaemonResponse {
        let net_config = match self.create_network_config(network) {
            Ok(config) => config,
            Err(message) => {
                return self.err("NETWORK_ALREADY_EXISTS", message);
            }
        };

        self.ok(format!(
            "created network '{}'\n  overlay: {}\n  state:   created",
            net_config.name, net_config.overlay_ip,
        ))
    }

    async fn handle_mesh_init(&mut self, network: &str) -> DaemonResponse {
        if let Some(active) = &self.active {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh down` first",
                    active.config.name,
                ),
            );
        }

        let net_config = match self.create_network_config(network) {
            Ok(config) => config,
            Err(message) => {
                return self.err("NETWORK_ALREADY_EXISTS", message);
            }
        };

        let network_name = net_config.name.clone();
        let overlay_ip = net_config.overlay_ip;
        if let Err(e) = self.start_mesh(net_config).await {
            return DaemonResponse {
                ok: false,
                code: "NETWORK_START_FAILED".into(),
                message: format!(
                    "initialized network '{}' but failed to start: {e}\n  state:   created",
                    network_name,
                ),
            };
        }

        self.ok(format!(
            "initialized and started network '{}'\n  overlay: {}\n  state:   running",
            network_name, overlay_ip,
        ))
    }

    pub(crate) fn create_network_config(&self, network: &str) -> Result<NetworkConfig, String> {
        let config_path = NetworkConfig::path(&self.data_dir, network);
        if config_path.exists() {
            return Err(format!(
                "network '{network}' already exists -- use `mesh up {network}` or `mesh destroy {network}`"
            ));
        }

        let net_config = NetworkConfig::new(NetworkName(network.into()), &self.identity.public_key);

        if let Err(e) = net_config.save(&config_path) {
            return Err(format!("failed to save network config: {e}"));
        }

        Ok(net_config)
    }

    async fn handle_mesh_up(&mut self, network: &str) -> DaemonResponse {
        if let Some(active) = &self.active {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh down` first",
                    active.config.name,
                ),
            );
        }

        let config_path = NetworkConfig::path(&self.data_dir, network);
        if !config_path.exists() {
            return self.err(
                "NETWORK_NOT_FOUND",
                format!(
                    "network '{network}' does not exist -- run `mesh create {network}` or `mesh init {network}`"
                ),
            );
        }

        let net_config = match NetworkConfig::load(&config_path) {
            Ok(config) => config,
            Err(e) => {
                return DaemonResponse {
                    ok: false,
                    code: "IO_ERROR".into(),
                    message: format!("failed to load network config: {e}"),
                };
            }
        };

        let network_name = net_config.name.clone();
        if let Err(e) = self.start_mesh(net_config).await {
            return DaemonResponse {
                ok: false,
                code: "NETWORK_START_FAILED".into(),
                message: e,
            };
        }

        self.ok(format!("mesh '{}' started", network_name))
    }

    async fn handle_mesh_down(&mut self) -> DaemonResponse {
        let Some(mut active) = self.active.take() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };

        let store = active.mesh.store();
        if let Err(e) = active.mesh.destroy().await {
            self.active = Some(active);
            return self.err("NETWORK_STOP_FAILED", format!("mesh down failed: {e}"));
        }

        if let Err(e) = store.delete_machine(&self.identity.machine_id).await {
            tracing::warn!(?e, "failed to remove local machine from membership");
        }
        self.clear_active_marker();
        self.ok("mesh stopped (config kept)")
    }

    async fn handle_mesh_destroy(&mut self, network: &str) -> DaemonResponse {
        let running_target = self
            .active
            .as_ref()
            .map(|a| a.config.name.0 == network)
            .unwrap_or(false);

        let config_path = NetworkConfig::path(&self.data_dir, network);
        if !running_target && !config_path.exists() {
            return self.err(
                "NETWORK_NOT_FOUND",
                format!("network '{network}' does not exist"),
            );
        }

        if running_target {
            let mut active = self.active.take().unwrap();
            let store = active.mesh.store();
            if let Err(e) = active.mesh.destroy().await {
                self.active = Some(active);
                return DaemonResponse {
                    ok: false,
                    code: "NETWORK_DESTROY_FAILED".into(),
                    message: format!("destroy failed: {e}"),
                };
            }
            if let Err(e) = store.delete_machine(&self.identity.machine_id).await {
                tracing::warn!(?e, "failed to remove local machine from membership");
            }
        }

        if let Err(e) = NetworkConfig::delete(&self.data_dir, network) {
            return DaemonResponse {
                ok: false,
                code: "IO_ERROR".into(),
                message: format!("failed to delete network config: {e}"),
            };
        }

        self.clear_active_marker();
        self.ok(format!("mesh '{network}' destroyed"))
    }

    async fn handle_machine_list(&self) -> DaemonResponse {
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

    async fn handle_machine_init(&mut self, target: &str, network: &str) -> DaemonResponse {
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

    async fn handle_machine_add(&mut self, target: &str) -> DaemonResponse {
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

    async fn handle_machine_invite_create(&self, ttl_secs: u64) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "machine invite create requires a running network",
                );
            }
        };

        if ttl_secs == 0 {
            return self.err("INVALID_ARGUMENT", "ttl_secs must be greater than zero");
        }

        let token = match self.do_issue_invite_token(&active.config, ttl_secs).await {
            Ok(token) => token,
            Err(err) => return self.err("INVITE_CREATE_FAILED", err),
        };

        self.ok(format!(
            "invite token created\n  network: {}\n  ttl:     {}s\n  token:   {}",
            active.config.name, ttl_secs, token
        ))
    }

    async fn handle_machine_invite_import(&self, token: &str) -> DaemonResponse {
        let store = match self.active.as_ref() {
            Some(a) => a.mesh.store(),
            None => {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "invite import requires a running network",
                );
            }
        };

        let invite = match parse_and_verify_invite_token(token) {
            Ok(invite) => invite,
            Err(err) => return self.err("INVALID_INVITE_TOKEN", err),
        };

        if now_unix_secs() > invite.expires_at {
            return self.err("INVITE_EXPIRED", "invite token has expired");
        }

        let record = InviteRecord {
            id: invite.invite_id.clone(),
            expires_at: invite.expires_at,
        };

        match store.create_invite(&record).await {
            Ok(()) => self.ok(format!(
                "invite imported\n  network: {}\n  invite:  {}",
                invite.network_name, record.id
            )),
            Err(crate::PortError::Operation { operation, .. }) if operation == "invite_exists" => {
                self.ok(format!(
                    "invite already present\n  network: {}\n  invite:  {}",
                    invite.network_name, record.id
                ))
            }
            Err(err) => self.err(
                "INVITE_IMPORT_FAILED",
                format!("failed to import invite: {err}"),
            ),
        }
    }

    async fn do_issue_invite_token(
        &self,
        network: &NetworkConfig,
        ttl_secs: u64,
    ) -> Result<String, String> {
        let store = self
            .active
            .as_ref()
            .map(|a| a.mesh.store())
            .ok_or_else(|| "no running network".to_string())?;

        let (token, claims) =
            issue_invite_token(&self.identity, network, ttl_secs, now_unix_secs())?;

        let record = InviteRecord {
            id: claims.invite_id,
            expires_at: claims.expires_at,
        };

        store
            .create_invite(&record)
            .await
            .map_err(|e| format!("store invite: {e}"))?;

        Ok(token)
    }
}
