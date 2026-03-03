use clap::{Parser, Subcommand, ValueEnum};
use ployz::transport::listener::{IncomingCommand, serve};
use ployz::transport::{DaemonRequest, DaemonResponse};
use ployz::{
    Affordances, Identity, Machine, MachineRecord, MembershipStore, MemoryService, MemoryStore,
    MemorySyncProbe, MemoryWireGuard, Mesh, Mode, NetworkConfig, NetworkName, default_data_dir,
    default_socket_path, resolve_profile,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
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
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let aff = Affordances::detect();
    let data_dir = cli.data_dir.unwrap_or_else(|| default_data_dir(&aff));

    match cli.command {
        Command::Configure { mode } => cmd_configure(mode.into()),
        Command::Run { mode, socket } => {
            let socket = socket.unwrap_or_else(|| default_socket_path(&aff));
            cmd_run(&data_dir, mode.into(), &socket).await
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

    let mut state = DaemonState::new(data_dir, identity);

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

struct DaemonState {
    data_dir: PathBuf,
    identity: Identity,
    machine: Option<
        Machine<MemoryWireGuard, MemoryService, MemoryStore, MemoryWireGuard, MemorySyncProbe>,
    >,
    store: Arc<MemoryStore>,
}

impl DaemonState {
    fn new(data_dir: &Path, identity: Identity) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            identity,
            machine: None,
            store: Arc::new(MemoryStore::new()),
        }
    }

    async fn handle(&mut self, req: DaemonRequest) -> DaemonResponse {
        match req {
            DaemonRequest::Status => self.handle_status(),
            DaemonRequest::MeshCreate { network } => self.handle_mesh_create(&network),
            DaemonRequest::MeshInit { network } => self.handle_mesh_init(&network).await,
            DaemonRequest::MeshUp { network } => self.handle_mesh_up(&network).await,
            DaemonRequest::MeshDown => self.handle_mesh_down().await,
            DaemonRequest::MeshDestroy { network } => self.handle_mesh_destroy(&network).await,
        }
    }

    fn handle_status(&self) -> DaemonResponse {
        let id = &self.identity;
        match &self.machine {
            Some(machine) => {
                let net = &machine.network;
                DaemonResponse {
                    ok: true,
                    message: format!(
                        "machine:  {}\nnetwork:  {}\noverlay:  {}\nphase:    {:?}",
                        id.machine_id,
                        net.name,
                        net.overlay_ip,
                        machine.phase(),
                    ),
                }
            }
            None => DaemonResponse {
                ok: true,
                message: format!(
                    "machine:  {}\nnetwork:  none\nphase:    idle",
                    id.machine_id,
                ),
            },
        }
    }

    fn handle_mesh_create(&self, network: &str) -> DaemonResponse {
        let net_config = match self.create_network_config(network) {
            Ok(config) => config,
            Err(message) => {
                return DaemonResponse {
                    ok: false,
                    message,
                };
            }
        };

        DaemonResponse {
            ok: true,
            message: format!(
                "created network '{}'\n  overlay: {}\n  state:   created",
                net_config.name, net_config.overlay_ip,
            ),
        }
    }

    async fn handle_mesh_init(&mut self, network: &str) -> DaemonResponse {
        if let Some(machine) = &self.machine {
            return DaemonResponse {
                ok: false,
                message: format!(
                    "network '{}' is already running -- run `mesh down` first",
                    machine.network.name,
                ),
            };
        }

        let net_config = match self.create_network_config(network) {
            Ok(config) => config,
            Err(message) => {
                return DaemonResponse {
                    ok: false,
                    message,
                };
            }
        };

        let network_name = net_config.name.clone();
        let overlay_ip = net_config.overlay_ip;
        if let Err(e) = self.start_mesh(net_config).await {
            return DaemonResponse {
                ok: false,
                message: format!(
                    "initialized network '{}' but failed to start: {e}\n  state:   created",
                    network_name,
                ),
            };
        }

        DaemonResponse {
            ok: true,
            message: format!(
                "initialized and started network '{}'\n  overlay: {}\n  state:   running",
                network_name, overlay_ip,
            ),
        }
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
            return DaemonResponse {
                ok: false,
                message: format!(
                    "network '{}' is already running -- run `mesh down` first",
                    machine.network.name,
                ),
            };
        }

        let config_path = NetworkConfig::path(&self.data_dir, network);
        if !config_path.exists() {
            return DaemonResponse {
                ok: false,
                message: format!(
                    "network '{network}' does not exist -- run `mesh create {network}` or `mesh init {network}`"
                ),
            };
        }

        let net_config = match NetworkConfig::load(&config_path) {
            Ok(config) => config,
            Err(e) => {
                return DaemonResponse {
                    ok: false,
                    message: format!("failed to load network config: {e}"),
                };
            }
        };

        let network_name = net_config.name.clone();
        if let Err(e) = self.start_mesh(net_config).await {
            return DaemonResponse {
                ok: false,
                message: e,
            };
        }

        DaemonResponse {
            ok: true,
            message: format!("mesh '{}' started", network_name),
        }
    }

    async fn handle_mesh_down(&mut self) -> DaemonResponse {
        match &mut self.machine {
            Some(machine) => match machine.mesh.destroy().await {
                Ok(()) => {
                    self.machine = None;
                    DaemonResponse {
                        ok: true,
                        message: "mesh stopped (config kept)".into(),
                    }
                }
                Err(e) => DaemonResponse {
                    ok: false,
                    message: format!("mesh down failed: {e}"),
                },
            },
            None => DaemonResponse {
                ok: false,
                message: "no mesh running".into(),
            },
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
            return DaemonResponse {
                ok: false,
                message: format!("network '{network}' does not exist"),
            };
        }

        if running_target {
            if let Some(machine) = &mut self.machine {
                if let Err(e) = machine.mesh.destroy().await {
                    return DaemonResponse {
                        ok: false,
                        message: format!("destroy failed: {e}"),
                    };
                }
            }
            self.machine = None;
        }

        if let Err(e) = NetworkConfig::delete(&self.data_dir, network) {
            return DaemonResponse {
                ok: false,
                message: format!("failed to delete network config: {e}"),
            };
        }

        DaemonResponse {
            ok: true,
            message: format!("mesh '{network}' destroyed"),
        }
    }

    async fn start_mesh(&mut self, net_config: NetworkConfig) -> std::result::Result<(), String> {
        // Seed the membership store with self.
        let self_record = MachineRecord {
            id: self.identity.machine_id.clone(),
            network: net_config.name.clone(),
            public_key: self.identity.public_key.clone(),
            overlay_ip: net_config.overlay_ip,
            endpoints: vec!["127.0.0.1:51820".into()],
        };
        self.store
            .upsert_machine(&self_record)
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
    ) -> Machine<MemoryWireGuard, MemoryService, MemoryStore, MemoryWireGuard, MemorySyncProbe>
    {
        let wg = Arc::new(MemoryWireGuard::new());
        let service = Arc::new(MemoryService::new());
        let mesh = Mesh::new(wg.clone(), service, self.store.clone(), Some(wg), None);

        // Clone identity fields for Machine (Machine doesn't need to own persistence).
        let identity = Identity::generate(
            self.identity.machine_id.clone(),
            self.identity.private_key.0,
        );

        Machine::new(identity, net_config, mesh)
    }
}
