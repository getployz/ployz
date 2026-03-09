use clap::{Parser, Subcommand, ValueEnum};
use ployz::daemon::DaemonState;
use ployz::transport::DaemonResponse;
use ployz::transport::listener::{IncomingCommand, serve};
use ployz::{Affordances, Identity, Mode, load_daemon_config};
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RuntimeMode {
    Memory,
    Docker,
    HostExec,
    HostService,
}

impl From<RuntimeMode> for Mode {
    fn from(value: RuntimeMode) -> Self {
        match value {
            RuntimeMode::Memory => Mode::Memory,
            RuntimeMode::Docker => Mode::Docker,
            RuntimeMode::HostExec => Mode::HostExec,
            RuntimeMode::HostService => Mode::HostService,
        }
    }
}

#[derive(Parser)]
#[command(name = "ployzd", about = "Ployz control plane daemon")]
struct Cli {
    /// Config path. Defaults to PLOYZ_CONFIG or an XDG path.
    #[arg(long)]
    config: Option<PathBuf>,

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
        #[arg(long, value_enum, default_value_t = RuntimeMode::Docker)]
        mode: RuntimeMode,
    },
    /// Start the daemon (control loop + command listener).
    Run {
        #[arg(long, value_enum, default_value_t = RuntimeMode::Docker)]
        mode: RuntimeMode,
        /// Socket path. Defaults to a platform-appropriate path.
        #[arg(long)]
        socket: Option<String>,
        /// Overlay TCP control port for daemon-to-daemon deploy sessions.
        #[arg(long)]
        remote_control_port: Option<u16>,
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
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let Cli {
        config,
        data_dir,
        command,
    } = Cli::parse();
    let aff = Affordances::detect();

    match command {
        Command::Configure { mode } => cmd_configure(mode.into()),
        Command::Run {
            mode,
            socket,
            remote_control_port,
        } => {
            let cfg = load_daemon_config(config, data_dir, socket, remote_control_port, &aff)?;
            cmd_run(
                &cfg.data_dir,
                mode.into(),
                &cfg.socket,
                cfg.cluster_cidr,
                cfg.subnet_prefix_len,
                cfg.remote_control_port,
            )
            .await
        }
    }
}

fn cmd_configure(mode: Mode) -> Result<()> {
    tracing::info!(?mode, "configure");
    tracing::info!("configure is install-time only; runtime daemon stays rootless");
    Ok(())
}

async fn cmd_run(
    data_dir: &Path,
    mode: Mode,
    socket_path: &str,
    cluster_cidr: String,
    subnet_prefix_len: u8,
    remote_control_port: u16,
) -> Result<()> {
    tracing::info!(?mode, "starting daemon");

    let id_path = data_dir.join("identity.json");
    let identity = Identity::load_or_generate(&id_path)?;
    tracing::info!(machine_id = %identity.machine_id, "loaded identity");

    let cancel = CancellationToken::new();
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<IncomingCommand>(32);

    let listener_cancel = cancel.clone();
    let socket_owned = socket_path.to_owned();
    let listener_handle = tokio::spawn(async move {
        if let Err(e) = serve(&socket_owned, cmd_tx, listener_cancel).await {
            tracing::error!(?e, "socket listener failed");
        }
    });

    let ctrl_cancel = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("received ctrl-c, shutting down");
        ctrl_cancel.cancel();
        tokio::signal::ctrl_c().await.ok();
        tracing::warn!("received second ctrl-c, forcing exit");
        std::process::exit(1);
    });

    let mut state = DaemonState::new(
        data_dir,
        identity,
        mode,
        cluster_cidr,
        subnet_prefix_len,
        remote_control_port,
    );

    if let Some(network) = state.read_active_marker() {
        tracing::info!(%network, "resuming network");
        match state.start_mesh_by_name(&network).await {
            Ok(()) => tracing::info!(%network, "resumed network"),
            Err(e) => {
                tracing::warn!(%e, %network, "failed to resume network");
                state.clear_active_marker();
            }
        }
    }

    tracing::info!(socket = socket_path, "daemon running");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            Some(cmd) = cmd_rx.recv() => {
                tokio::select! {
                    response = state.handle(cmd.request) => {
                        let _ = cmd.reply.send(response);
                    }
                    _ = cancel.cancelled() => {
                        let _ = cmd.reply.send(DaemonResponse {
                            ok: false,
                            code: "SHUTDOWN".into(),
                            message: "daemon shutting down".into(),
                        });
                        break;
                    }
                }
            }
        }
    }

    if let Some(active) = state.active.take() {
        let ployz::daemon::ActiveMesh {
            config: _config,
            mut mesh,
            remote_control,
        } = active;
        remote_control.shutdown().await;
        let _ = mesh.detach().await;
    }

    listener_handle.await.ok();
    tracing::info!("daemon stopped");
    Ok(())
}
