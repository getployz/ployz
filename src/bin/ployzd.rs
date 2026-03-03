use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::{Parser, Subcommand, ValueEnum};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use ployz::adapters::corrosion::CorrosionStore;
use ployz::transport::listener::{IncomingCommand, serve};
use ployz::transport::{DaemonRequest, DaemonResponse};
use ployz::{
    Affordances, Identity, InviteStore, InviteRecord, Machine, MachineRecord, MachineStore,
    MemoryService,
    MemorySyncProbe, MemoryWireGuard, Mesh, Mode, NetworkConfig, NetworkId, NetworkName,
    load_daemon_config, resolve_profile,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RuntimeMode {
    Dev,
    Agent,
    Prod,
}

impl From<RuntimeMode> for Mode {
    fn from(value: RuntimeMode) -> Self {
        match value {
            RuntimeMode::Dev => Mode::Dev,
            RuntimeMode::Agent => Mode::Agent,
            RuntimeMode::Prod => Mode::Prod,
        }
    }
}

#[derive(Parser)]
#[command(name = "ployzd", about = "Ployz control plane daemon")]
struct Cli {
    /// Data directory. Defaults to a platform-appropriate path.
    #[arg(long)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Perform privileged one-time install/update setup.
    Configure {
        #[arg(long, value_enum, default_value_t = RuntimeMode::Prod)]
        mode: RuntimeMode,
    },
    /// Start the daemon (control loop + command listener).
    Run {
        #[arg(long, value_enum, default_value_t = RuntimeMode::Agent)]
        mode: RuntimeMode,
        /// Socket path. Defaults to a platform-appropriate path.
        #[arg(long)]
        socket: Option<String>,
    },
}

type Result<T> = std::result::Result<T, CliError>;

#[derive(Debug, Error)]
enum CliError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Identity(#[from] ployz::IdentityError),
    #[error(transparent)]
    Config(#[from] ployz::ConfigLoadError),
    #[error("{0}")]
    Store(String),
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let Cli { data_dir, command } = Cli::parse();
    let aff = Affordances::detect();

    match command {
        Command::Configure { mode } => cmd_configure(mode.into()),
        Command::Run { mode, socket } => {
            let cfg = load_daemon_config(data_dir, socket, &aff)?;
            cmd_run(&cfg.data_dir, mode.into(), &cfg.socket).await
        }
    }
}

fn cmd_configure(mode: Mode) -> Result<()> {
    let profile = resolve_profile(&Affordances::detect(), mode);
    println!("configure profile: {profile:?}");
    println!("configure is install-time only; runtime daemon stays rootless");
    Ok(())
}

async fn cmd_run(data_dir: &Path, mode: Mode, socket_path: &str) -> Result<()> {
    let profile = resolve_profile(&Affordances::detect(), mode);
    tracing::info!(?profile, "resolved profile");

    // Auto-generate identity if not present.
    let id_path = data_dir.join("identity.json");
    let identity = Identity::load_or_generate(&id_path)?;
    println!("machine: {}", identity.machine_id);

    let store = build_store().await?;

    let cancel = CancellationToken::new();
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<IncomingCommand>(32);

    // Spawn socket listener.
    let listener_cancel = cancel.clone();
    let socket_owned = socket_path.to_owned();
    let listener_handle = tokio::spawn(async move {
        if let Err(e) = serve(&socket_owned, cmd_tx, listener_cancel).await {
            tracing::error!(?e, "socket listener failed");
        }
    });

    // Spawn ctrl-c handler.
    let ctrl_cancel = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        println!("\nreceived ctrl-c, shutting down...");
        ctrl_cancel.cancel();
    });

    let mut state = DaemonState::new(data_dir, identity, store);

    println!("ployzd running (socket: {socket_path})");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            Some(cmd) = cmd_rx.recv() => {
                let response = state.handle(cmd.request).await;
                let _ = cmd.reply.send(response);
            }
        }
    }

    // Graceful shutdown: detach mesh if running.
    if let Some(ref mut machine) = state.machine {
        let _ = machine.mesh.detach().await;
    }

    listener_handle.await.ok();
    println!("ployzd stopped");
    Ok(())
}

async fn build_store() -> Result<Arc<CorrosionStore>> {
    let endpoint =
        std::env::var("PLOYZ_CORROSION_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let store = CorrosionStore::connect(&endpoint)
        .await
        .map_err(CliError::Store)?;
    println!("membership backend: corrosion ({endpoint})");
    Ok(Arc::new(store))
}

struct DaemonState {
    data_dir: PathBuf,
    identity: Identity,
    machine: Option<
        Machine<MemoryWireGuard, MemoryService, CorrosionStore, MemoryWireGuard, MemorySyncProbe>,
    >,
    store: Arc<CorrosionStore>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InviteClaims {
    invite_id: String,
    network_id: NetworkId,
    network_name: String,
    issued_by: String,
    issuer_verify_key: String,
    expires_at: u64,
    nonce: String,
}

impl DaemonState {
    fn new(data_dir: &Path, identity: Identity, store: Arc<CorrosionStore>) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            identity,
            machine: None,
            store,
        }
    }

    async fn handle(&mut self, req: DaemonRequest) -> DaemonResponse {
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

    fn ok(&self, message: impl Into<String>) -> DaemonResponse {
        DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: message.into(),
        }
    }

    fn err(&self, code: &str, message: impl Into<String>) -> DaemonResponse {
        DaemonResponse {
            ok: false,
            code: code.into(),
            message: message.into(),
        }
    }

    fn handle_status(&self) -> DaemonResponse {
        let id = &self.identity;
        match &self.machine {
            Some(machine) => {
                let net = &machine.network;
                self.ok(format!(
                    "machine:  {}\nnetwork:  {}\noverlay:  {}\nphase:    {:?}",
                    id.machine_id,
                    net.name,
                    net.overlay_ip,
                    machine.phase(),
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

        let running = self.machine.as_ref().map(|m| m.network.name.0.as_str());
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
            .machine
            .as_ref()
            .map(|m| m.network.name.0 == network)
            .unwrap_or(false);
        let state = if running { "running" } else { "created" };
        self.ok(format!(
            "network: {}\noverlay: {}\nstate:   {}",
            config.name, config.overlay_ip, state
        ))
    }

    async fn handle_mesh_join(&mut self, token: &str) -> DaemonResponse {
        if let Some(machine) = &self.machine {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh down` first",
                    machine.network.name
                ),
            );
        }

        let invite = match self.parse_and_verify_invite_token(token) {
            Ok(invite) => invite,
            Err(err) => return self.err("INVALID_INVITE_TOKEN", err),
        };

        let now = now_unix_secs();
        if now > invite.expires_at {
            return self.err("INVITE_EXPIRED", "invite token has expired");
        }

        if let Err(err) = self
            .store
            .consume_invite(&invite.network_id, &invite.invite_id, now)
            .await
        {
            return self.map_invite_consume_error(err);
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
        if let Some(machine) = &self.machine {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh down` first",
                    machine.network.name,
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

    fn create_network_config(&self, network: &str) -> std::result::Result<NetworkConfig, String> {
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
        if let Some(machine) = &self.machine {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh down` first",
                    machine.network.name,
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
        match &mut self.machine {
            Some(machine) => match machine.mesh.destroy().await {
                Ok(()) => {
                    if let Err(e) = self
                        .store
                        .delete_machine(&machine.network.id, &self.identity.machine_id)
                        .await
                    {
                        tracing::warn!(?e, "failed to remove local machine from membership");
                    }
                    self.machine = None;
                    self.ok("mesh stopped (config kept)")
                }
                Err(e) => self.err("NETWORK_STOP_FAILED", format!("mesh down failed: {e}")),
            },
            None => self.err("NO_RUNNING_NETWORK", "no mesh running"),
        }
    }

    async fn handle_mesh_destroy(&mut self, network: &str) -> DaemonResponse {
        let running_target = self
            .machine
            .as_ref()
            .map(|machine| machine.network.name.0 == network)
            .unwrap_or(false);

        let config_path = NetworkConfig::path(&self.data_dir, network);
        if !running_target && !config_path.exists() {
            return self.err(
                "NETWORK_NOT_FOUND",
                format!("network '{network}' does not exist"),
            );
        }

        if running_target {
            if let Some(machine) = &mut self.machine {
                if let Err(e) = machine.mesh.destroy().await {
                    return DaemonResponse {
                        ok: false,
                        code: "NETWORK_DESTROY_FAILED".into(),
                        message: format!("destroy failed: {e}"),
                    };
                }
                if let Err(e) = self
                    .store
                    .delete_machine(&machine.network.id, &self.identity.machine_id)
                    .await
                {
                    tracing::warn!(?e, "failed to remove local machine from membership");
                }
            }
            self.machine = None;
        }

        if let Err(e) = NetworkConfig::delete(&self.data_dir, network) {
            return DaemonResponse {
                ok: false,
                code: "IO_ERROR".into(),
                message: format!("failed to delete network config: {e}"),
            };
        }

        self.ok(format!("mesh '{network}' destroyed"))
    }

    async fn handle_machine_init(&mut self, target: &str, network: &str) -> DaemonResponse {
        if self.machine.is_some() {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                "machine init requires no local running network; switch context or run `mesh down` first",
            );
        }

        let bootstrap = "set -eu; command -v ployzd >/dev/null 2>&1 || { echo 'ployzd not installed'; exit 12; }; command -v docker >/dev/null 2>&1 || { echo 'docker not installed'; exit 13; };";
        if let Err(err) = self.run_ssh(target, bootstrap).await {
            return self.err("SSH_BOOTSTRAP_FAILED", err);
        }

        let init_cmd = format!("set -eu; ployz mesh init \"{}\"", shell_escape(network));
        if let Err(err) = self.run_ssh(target, &init_cmd).await {
            return self.err("REMOTE_INIT_FAILED", err);
        }

        self.ok(format!(
            "remote founder initialized\n  target:  {target}\n  network: {network}"
        ))
    }

    async fn handle_machine_add(&mut self, target: &str) -> DaemonResponse {
        let running = match self.machine.as_ref() {
            Some(machine) => machine.network.clone(),
            None => {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "machine add requires a running network on this daemon",
                );
            }
        };

        let token = match self.issue_invite_token(&running, 600).await {
            Ok(token) => token,
            Err(err) => return self.err("INVITE_CREATE_FAILED", err),
        };

        let bootstrap = "set -eu; command -v ployzd >/dev/null 2>&1 || { echo 'ployzd not installed'; exit 12; }; command -v ployz >/dev/null 2>&1 || { echo 'ployz not installed'; exit 13; }; command -v docker >/dev/null 2>&1 || { echo 'docker not installed'; exit 14; }; ployz status >/dev/null 2>&1 || { echo 'ployzd not running'; exit 15; };";
        if let Err(err) = self.run_ssh(target, bootstrap).await {
            return self.err("SSH_BOOTSTRAP_FAILED", err);
        }

        let import_cmd = format!(
            "set -eu; ployz machine invite import --token \"{}\"",
            shell_escape(&token)
        );
        if let Err(err) = self.run_ssh(target, &import_cmd).await {
            return self.err("REMOTE_INVITE_IMPORT_FAILED", err);
        }

        let join_cmd = format!(
            "set -eu; ployz mesh join --token \"{}\"",
            shell_escape(&token)
        );
        if let Err(err) = self.run_ssh(target, &join_cmd).await {
            return self.err("REMOTE_JOIN_FAILED", err);
        }

        self.ok(format!(
            "machine add completed\n  target:  {target}\n  network: {}",
            running.name,
        ))
    }

    async fn handle_machine_invite_create(&self, ttl_secs: u64) -> DaemonResponse {
        let machine = match self.machine.as_ref() {
            Some(machine) => machine,
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

        let token = match self.issue_invite_token(&machine.network, ttl_secs).await {
            Ok(token) => token,
            Err(err) => return self.err("INVITE_CREATE_FAILED", err),
        };

        self.ok(format!(
            "invite token created\n  network: {}\n  ttl:     {}s\n  token:   {}",
            machine.network.name, ttl_secs, token
        ))
    }

    async fn handle_machine_invite_import(&self, token: &str) -> DaemonResponse {
        let invite = match self.parse_and_verify_invite_token(token) {
            Ok(invite) => invite,
            Err(err) => return self.err("INVALID_INVITE_TOKEN", err),
        };

        if now_unix_secs() > invite.expires_at {
            return self.err("INVITE_EXPIRED", "invite token has expired");
        }

        let network_name = NetworkName(invite.network_name.clone());
        let record = InviteRecord {
            id: invite.invite_id.clone(),
            network_id: invite.network_id.clone(),
            network_name,
            issued_by: ployz::MachineId(invite.issued_by.clone()),
            expires_at: invite.expires_at,
            nonce: invite.nonce,
            max_uses: 1,
            used: 0,
            revoked: false,
        };

        match self.store.create_invite(&record.network_id, &record).await {
            Ok(()) => self.ok(format!(
                "invite imported\n  network: {}\n  invite:  {}",
                record.network_name, record.id
            )),
            Err(ployz::PortError::Operation { operation, .. }) if operation == "invite_exists" => {
                self.ok(format!(
                    "invite already present\n  network: {}\n  invite:  {}",
                    record.network_name, record.id
                ))
            }
            Err(err) => self.err(
                "INVITE_IMPORT_FAILED",
                format!("failed to import invite: {err}"),
            ),
        }
    }

    async fn issue_invite_token(
        &self,
        network: &NetworkConfig,
        ttl_secs: u64,
    ) -> std::result::Result<String, String> {
        let expires_at = now_unix_secs()
            .checked_add(ttl_secs)
            .ok_or_else(|| "ttl overflow".to_string())?;

        let mut nonce_bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let mut nonce = String::with_capacity(32);
        for b in nonce_bytes {
            use std::fmt::Write as _;
            let _ = write!(&mut nonce, "{b:02x}");
        }

        let mut invite_id_bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut invite_id_bytes);
        let mut invite_id = String::with_capacity(32);
        for b in invite_id_bytes {
            use std::fmt::Write as _;
            let _ = write!(&mut invite_id, "{b:02x}");
        }

        let signing_key = SigningKey::from_bytes(&self.identity.private_key.0);
        let issuer_verify_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

        let token = InviteClaims {
            invite_id: invite_id.clone(),
            network_id: network.id.clone(),
            network_name: network.name.0.clone(),
            issued_by: self.identity.machine_id.0.clone(),
            issuer_verify_key,
            expires_at,
            nonce: nonce.clone(),
        };

        let record = InviteRecord {
            id: invite_id,
            network_id: network.id.clone(),
            network_name: network.name.clone(),
            issued_by: self.identity.machine_id.clone(),
            expires_at,
            nonce,
            max_uses: 1,
            used: 0,
            revoked: false,
        };

        self.store
            .create_invite(&network.id, &record)
            .await
            .map_err(|e| format!("store invite: {e}"))?;

        let claims_json = serde_json::to_vec(&token).map_err(|e| format!("encode invite: {e}"))?;
        let signature = signing_key.sign(&claims_json);
        let claims_encoded = URL_SAFE_NO_PAD.encode(&claims_json);
        let sig_encoded = URL_SAFE_NO_PAD.encode(signature.to_bytes());

        Ok(format!("{claims_encoded}.{sig_encoded}"))
    }

    fn parse_and_verify_invite_token(
        &self,
        encoded: &str,
    ) -> std::result::Result<InviteClaims, String> {
        let (claims_b64, sig_b64) = encoded
            .split_once('.')
            .ok_or_else(|| "invalid invite token format".to_string())?;

        let claims_json = URL_SAFE_NO_PAD
            .decode(claims_b64)
            .map_err(|e| format!("decode invite claims: {e}"))?;
        let claims: InviteClaims = serde_json::from_slice(&claims_json)
            .map_err(|e| format!("parse invite claims: {e}"))?;

        let sig_bytes = URL_SAFE_NO_PAD
            .decode(sig_b64)
            .map_err(|e| format!("decode invite signature: {e}"))?;
        let sig_arr: [u8; 64] = sig_bytes
            .as_slice()
            .try_into()
            .map_err(|_| "invalid invite signature length".to_string())?;
        let signature = Signature::from_bytes(&sig_arr);

        let key_bytes = URL_SAFE_NO_PAD
            .decode(&claims.issuer_verify_key)
            .map_err(|e| format!("decode issuer verify key: {e}"))?;
        let key_arr: [u8; 32] = key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| "invalid issuer verify key length".to_string())?;
        let verify_key = VerifyingKey::from_bytes(&key_arr)
            .map_err(|e| format!("parse issuer verify key: {e}"))?;

        verify_key
            .verify(&claims_json, &signature)
            .map_err(|e| format!("verify invite signature: {e}"))?;

        Ok(claims)
    }

    fn map_invite_consume_error(&self, err: ployz::PortError) -> DaemonResponse {
        match err {
            ployz::PortError::Operation { operation, message } => match operation {
                "invite_not_found" => self.err("INVITE_NOT_FOUND", message),
                "invite_expired" => self.err("INVITE_EXPIRED", message),
                "invite_consumed" => self.err("INVITE_CONSUMED", message),
                "invite_revoked" => self.err("INVITE_REVOKED", message),
                _ => self.err("INVITE_REDEEM_FAILED", message),
            },
        }
    }

    async fn run_ssh(&self, target: &str, remote_script: &str) -> std::result::Result<(), String> {
        let output = TokioCommand::new("ssh")
            .arg(target)
            .arg(remote_script)
            .output()
            .await
            .map_err(|e| format!("ssh execution failed: {e}"))?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        Err(format!(
            "ssh to '{target}' failed (status: {}){}",
            output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into()),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ))
    }

    async fn start_mesh(&mut self, net_config: NetworkConfig) -> std::result::Result<(), String> {
        // Seed the membership store with self.
        let self_record = MachineRecord {
            id: self.identity.machine_id.clone(),
            network_id: net_config.id.clone(),
            network: net_config.name.clone(),
            public_key: self.identity.public_key.clone(),
            overlay_ip: net_config.overlay_ip,
            endpoints: vec!["127.0.0.1:51820".into()],
        };
        self.store
            .upsert_machine(&net_config.id, &self_record)
            .await
            .map_err(|e| format!("failed to seed store: {e}"))?;

        let mut machine = self.new_machine(net_config);
        machine
            .init_network()
            .await
            .map_err(|e| format!("failed to start network: {e}"))?;

        self.machine = Some(machine);
        Ok(())
    }

    fn new_machine(
        &self,
        net_config: NetworkConfig,
    ) -> Machine<MemoryWireGuard, MemoryService, CorrosionStore, MemoryWireGuard, MemorySyncProbe>
    {
        let wg = Arc::new(MemoryWireGuard::new());
        let service = Arc::new(MemoryService::new());
        let mesh = Mesh::new(
            net_config.id.clone(),
            wg.clone(),
            service,
            self.store.clone(),
            Some(wg),
            None,
        );

        // Clone identity fields for Machine (Machine doesn't need to own persistence).
        let identity = Identity::generate(
            self.identity.machine_id.clone(),
            self.identity.private_key.0,
        );

        Machine::new(identity, net_config, mesh)
    }
}

fn shell_escape(input: &str) -> String {
    input.replace('"', "\\\"")
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
