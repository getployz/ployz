use clap::{Args, Parser, Subcommand, ValueEnum};
use ployz_sdk::load_client_config;
use ployz_sdk::spec::{
    ContainerSpec, Namespace, NetworkMode, Placement, PortProtocol, PublishedPort, PullPolicy,
    Resources, RestartPolicy, RolloutStrategy, ServicePort, ServiceSpec, VolumeMount, VolumeSource,
};
use ployz_sdk::transport::{
    DaemonRequest, DaemonResponse, DeployManifestFormat, DeployManifestInput, DeployOptions,
    Transport, UnixSocketTransport,
};
use ployzd::daemon::{ActiveMesh, DaemonState};
use ployzd::ipc::listener::{IncomingCommand, serve};
use ployzd::{Affordances, Identity, Mode, load_daemon_config};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

type Result<T> = std::result::Result<T, CliError>;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RuntimeMode {
    Docker,
    HostExec,
    HostService,
}

impl From<RuntimeMode> for Mode {
    fn from(value: RuntimeMode) -> Self {
        match value {
            RuntimeMode::Docker => Mode::Docker,
            RuntimeMode::HostExec => Mode::HostExec,
            RuntimeMode::HostService => Mode::HostService,
        }
    }
}

#[derive(Debug)]
enum CliError {
    Usage(String),
    Io(String),
    Serialize(String),
    Config(String),
    Identity(String),
    Transport { socket: String, message: String },
}

impl CliError {
    fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) | Self::Config(_) => 2,
            Self::Io(_) | Self::Serialize(_) | Self::Identity(_) | Self::Transport { .. } => 1,
        }
    }

    fn print(&self) {
        match self {
            Self::Usage(message)
            | Self::Io(message)
            | Self::Serialize(message)
            | Self::Config(message)
            | Self::Identity(message) => eprintln!("error: {message}"),
            Self::Transport { socket, message } => {
                eprintln!("error: {message}");
                eprintln!("is ployzd running? (socket: {socket})");
            }
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "ployzd",
    about = "Ployz control plane daemon and operator CLI",
    version,
    subcommand_required = true,
    arg_required_else_help = true,
    propagate_version = true,
    disable_help_subcommand = false
)]
struct Cli {
    /// Config path. Defaults to PLOYZ_CONFIG or an XDG path.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Data directory. Defaults to a platform-appropriate path.
    #[arg(long, global = true, value_name = "PATH")]
    data_dir: Option<PathBuf>,

    /// Socket path. Defaults to a platform-appropriate path.
    #[arg(long, global = true, value_name = "PATH")]
    socket: Option<String>,

    /// Emit a JSON wrapper for command output.
    #[arg(long, global = true, conflicts_with = "plain")]
    json: bool,

    /// Emit stable plain text output.
    #[arg(long, global = true, conflicts_with = "json")]
    plain: bool,

    /// Suppress successful human-readable output.
    #[arg(short = 'q', long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
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
        /// Overlay TCP control port for daemon-to-daemon deploy sessions.
        #[arg(long)]
        remote_control_port: Option<u16>,
    },
    /// Show daemon and mesh status.
    Status,
    /// Deploy manifests into an explicit namespace.
    Deploy(Box<DeployCommand>),
    /// Mesh network management.
    #[command(alias = "network")]
    Mesh {
        #[command(subcommand)]
        action: MeshAction,
    },
    /// Machine lifecycle and onboarding.
    Machine {
        #[command(subcommand)]
        action: MachineAction,
    },
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
struct DeployCommand {
    #[command(subcommand)]
    action: Option<DeployAction>,

    #[command(flatten)]
    manifest: DeployManifestArgs,
}

#[derive(Debug, Subcommand)]
enum DeployAction {
    /// Preview a manifest without applying it.
    Preview(DeployManifestArgs),
    /// Build a single-service deploy request.
    Service(DeployServiceArgs),
}

#[derive(Debug, Args, Clone)]
struct DeployManifestArgs {
    /// Target namespace.
    #[arg(long, value_name = "NAME")]
    namespace: Option<String>,

    /// Manifest file path, or '-' for stdin.
    #[arg(short = 'f', long, value_name = "PATH")]
    file: Option<String>,

    /// Manifest format (currently only "service" is supported).
    #[arg(long, value_enum, default_value_t = DeployManifestFormat::Service)]
    format: DeployManifestFormat,

    /// Preview only; do not apply changes.
    #[arg(short = 'n', long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct DeployServiceArgs {
    /// Service name.
    name: String,

    /// Container image to run.
    #[arg(long)]
    image: String,

    /// Namespace.
    #[arg(long)]
    namespace: String,

    /// Port mappings (host:container, e.g. 8080:80).
    #[arg(short, long, value_name = "HOST:CONTAINER")]
    publish: Vec<String>,

    /// Environment variables (KEY=VALUE).
    #[arg(short, long, value_name = "KEY=VALUE")]
    env: Vec<String>,

    /// Volume mounts (host_path:container_path or name:container_path).
    #[arg(short, long, value_name = "SRC:DST")]
    volume: Vec<String>,

    /// Network mode (overlay, host, none, or another service name).
    #[arg(long, default_value = "overlay")]
    network: String,

    /// Always pull image before running.
    #[arg(long)]
    pull: bool,

    /// Restart policy (unless-stopped, always, on-failure, no).
    #[arg(long, default_value = "unless-stopped")]
    restart: String,

    /// Preview only; do not apply changes.
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Command to run (overrides image CMD).
    #[arg(last = true)]
    command: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum MeshAction {
    /// List known mesh networks and their local state.
    List,
    /// Show local state for one mesh network.
    Status { network: String },
    /// Join an existing mesh network using an invite token.
    Join {
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        token_stdin: bool,
    },
    /// Report whether the local mesh is ready.
    Ready {
        #[arg(long)]
        json: bool,
    },
    /// Create a mesh network config only.
    Create { network: String },
    /// Create and start a new mesh network.
    Init {
        network: Option<String>,
        #[arg(long)]
        name_stdin: bool,
    },
    /// Start an existing mesh network.
    Up {
        network: String,
        #[arg(long)]
        skip_bootstrap_wait: bool,
    },
    /// Stop mesh infra but keep network config and data.
    Down,
    /// Tear down all mesh resources and leave the network permanently.
    Destroy {
        network: Option<String>,
        #[arg(long)]
        name_stdin: bool,
    },
    /// Print this machine's identity as an encoded JoinResponse (requires running network).
    SelfRecord,
    /// Accept a JoinResponse and seed the joiner's record into the local store.
    Accept { response: String },
}

#[derive(Debug, Subcommand)]
enum MachineAction {
    /// List machines in the running network.
    #[command(alias = "list")]
    Ls,
    /// Bootstrap a remote founder and create/start a network.
    Init {
        target: String,
        #[arg(long)]
        network: String,
    },
    /// Add a remote machine to the currently running network.
    Add {
        #[arg(required = true, num_args = 1..)]
        targets: Vec<String>,
    },
    /// Mark a machine as draining.
    Drain { id: String },
    /// Remove a machine from the mesh.
    Rm {
        id: String,
        #[arg(long)]
        force: bool,
    },
    /// Set or remove labels on a machine.
    Label {
        /// Machine ID (or "self" for the local machine).
        id: String,
        /// Labels to set (key=value).
        #[arg(long, num_args = 1..)]
        set: Vec<String>,
        /// Label keys to remove.
        #[arg(long, num_args = 1..)]
        rm: Vec<String>,
    },
    /// Invite token operations.
    Invite {
        #[command(subcommand)]
        action: MachineInviteAction,
    },
}

#[derive(Debug, Subcommand)]
enum MachineInviteAction {
    /// Create an invite token for joining this running network.
    Create {
        /// Invite TTL in seconds.
        #[arg(long, default_value_t = 600)]
        ttl_secs: u64,
    },
    /// Import an invite token into local invite state.
    Import {
        #[arg(long)]
        token: String,
    },
}

#[tokio::main]
async fn main() {
    match run().await {
        Ok(code) => {
            if code != 0 {
                process::exit(code);
            }
        }
        Err(err) => {
            err.print();
            process::exit(err.exit_code());
        }
    }
}

async fn run() -> Result<i32> {
    let cli = Cli::parse();

    match cli.command {
        Command::Configure { mode } => {
            init_tracing();
            cmd_configure(mode.into())?;
            Ok(0)
        }
        Command::Run {
            mode,
            remote_control_port,
        } => {
            init_tracing();
            let aff = Affordances::detect();
            let cfg = load_daemon_config(
                cli.config,
                cli.data_dir,
                cli.socket,
                remote_control_port,
                &aff,
            )
            .map_err(|err| CliError::Config(err.to_string()))?;
            cmd_run(
                &cfg.data_dir,
                mode.into(),
                &cfg.socket,
                cfg.cluster_cidr,
                cfg.subnet_prefix_len,
                cfg.remote_control_port,
                cfg.gateway_listen_addr,
                cfg.gateway_threads,
            )
            .await?;
            Ok(0)
        }
        other => {
            let resolved = load_client_config(cli.config, cli.socket, &Affordances::detect())
                .map_err(|err| CliError::Config(err.to_string()))?;
            let socket = resolved.socket;
            let transport = UnixSocketTransport::new(socket.clone());
            let request = build_request(other)?;
            let response = transport
                .request(request)
                .await
                .map_err(|err| CliError::Transport {
                    socket,
                    message: err.to_string(),
                })?;

            render_response(cli.json, cli.plain, cli.quiet, &response)?;
            if response.ok { Ok(0) } else { Ok(1) }
        }
    }
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt::try_init();
}

#[allow(clippy::result_large_err)]
fn cmd_configure(mode: Mode) -> Result<()> {
    tracing::info!(?mode, "configure");
    tracing::info!("configure is install-time only; runtime daemon stays rootless");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn cmd_run(
    data_dir: &Path,
    mode: Mode,
    socket_path: &str,
    cluster_cidr: String,
    subnet_prefix_len: u8,
    remote_control_port: u16,
    gateway_listen_addr: String,
    gateway_threads: usize,
) -> Result<()> {
    tracing::info!(?mode, "starting daemon");

    let id_path = data_dir.join("identity.json");
    let identity =
        Identity::load_or_generate(&id_path).map_err(|err| CliError::Identity(err.to_string()))?;
    tracing::info!(machine_id = %identity.machine_id, "loaded identity");

    let cancel = CancellationToken::new();
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<IncomingCommand>(32);

    let listener_cancel = cancel.clone();
    let socket_owned = socket_path.to_owned();
    let listener_handle = tokio::spawn(async move {
        if let Err(err) = serve(&socket_owned, cmd_tx, listener_cancel).await {
            tracing::error!(?err, "socket listener failed");
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
        gateway_listen_addr,
        gateway_threads,
    );

    if let Some(network) = state.read_active_marker() {
        tracing::info!(%network, "resuming network");
        match state.start_mesh_by_name(&network).await {
            Ok(()) => tracing::info!(%network, "resumed network"),
            Err(err) => {
                tracing::warn!(%err, %network, "failed to resume network");
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
        let ActiveMesh {
            config: _config,
            mut mesh,
            remote_control,
            mut gateway,
            mut dns,
        } = active;
        let _ = dns.detach().await;
        let _ = gateway.detach().await;
        remote_control.shutdown().await;
        let _ = mesh.detach().await;
    }

    listener_handle.await.ok();
    tracing::info!("daemon stopped");
    Ok(())
}

fn build_request(command: Command) -> Result<DaemonRequest> {
    match command {
        Command::Status => Ok(DaemonRequest::Status),
        Command::Deploy(command) => build_deploy_request(*command),
        Command::Mesh { action } => build_mesh_request(action),
        Command::Machine { action } => build_machine_request(action),
        Command::Configure { .. } | Command::Run { .. } => Err(CliError::Usage(
            "internal error: daemon command cannot be encoded as a daemon request".into(),
        )),
    }
}

fn build_deploy_request(command: DeployCommand) -> Result<DaemonRequest> {
    match command.action {
        Some(DeployAction::Preview(args)) => build_manifest_request(args, true),
        Some(DeployAction::Service(args)) => build_deploy_service_request(args),
        None => build_manifest_request(command.manifest, false),
    }
}

fn build_manifest_request(args: DeployManifestArgs, force_preview: bool) -> Result<DaemonRequest> {
    let namespace = required_value(args.namespace, "deploy requires --namespace")?;
    let file = required_value(args.file, "deploy requires --file")?;
    let body = read_text_source("deploy manifest", &file)?;
    let manifest_json = encode_manifest_json(DeployManifestInput {
        format: args.format,
        body,
    })?;
    let options = DeployOptions::default();

    if force_preview || args.dry_run {
        Ok(DaemonRequest::DeployPreview {
            namespace,
            manifest_json,
            options,
        })
    } else {
        Ok(DaemonRequest::DeployApply {
            namespace,
            manifest_json,
            options,
        })
    }
}

fn build_deploy_service_request(args: DeployServiceArgs) -> Result<DaemonRequest> {
    let spec = build_service_spec(
        &args.image,
        Some(args.name.as_str()),
        &args.namespace,
        &args.publish,
        &args.env,
        &args.volume,
        &args.network,
        args.pull,
        &args.restart,
        &args.command,
    );
    let spec_json = serde_json::to_string(&spec)
        .map_err(|err| CliError::Serialize(format!("failed to serialize service spec: {err}")))?;
    let manifest_json = encode_manifest_json(DeployManifestInput {
        format: DeployManifestFormat::Service,
        body: spec_json,
    })?;
    let options = DeployOptions::default();

    if args.dry_run {
        Ok(DaemonRequest::DeployPreview {
            namespace: args.namespace,
            manifest_json,
            options,
        })
    } else {
        Ok(DaemonRequest::DeployApply {
            namespace: args.namespace,
            manifest_json,
            options,
        })
    }
}

fn build_mesh_request(action: MeshAction) -> Result<DaemonRequest> {
    match action {
        MeshAction::List => Ok(DaemonRequest::MeshList),
        MeshAction::Status { network } => Ok(DaemonRequest::MeshStatus { network }),
        MeshAction::Join { token, token_stdin } => Ok(DaemonRequest::MeshJoin {
            token: string_arg_or_stdin("mesh join token", "--token-stdin", token, token_stdin)?,
        }),
        MeshAction::Ready { json } => Ok(DaemonRequest::MeshReady { json }),
        MeshAction::Create { network } => Ok(DaemonRequest::MeshCreate { network }),
        MeshAction::Init {
            network,
            name_stdin,
        } => Ok(DaemonRequest::MeshInit {
            network: string_arg_or_stdin("mesh init network", "--name-stdin", network, name_stdin)?,
        }),
        MeshAction::Up {
            network,
            skip_bootstrap_wait,
        } => Ok(DaemonRequest::MeshUp {
            network,
            skip_bootstrap_wait,
        }),
        MeshAction::Down => Ok(DaemonRequest::MeshDown),
        MeshAction::Destroy {
            network,
            name_stdin,
        } => Ok(DaemonRequest::MeshDestroy {
            network: string_arg_or_stdin(
                "mesh destroy network",
                "--name-stdin",
                network,
                name_stdin,
            )?,
        }),
        MeshAction::SelfRecord => Ok(DaemonRequest::MeshSelfRecord),
        MeshAction::Accept { response } => Ok(DaemonRequest::MeshAccept { response }),
    }
}

fn build_machine_request(action: MachineAction) -> Result<DaemonRequest> {
    match action {
        MachineAction::Ls => Ok(DaemonRequest::MachineList),
        MachineAction::Init { target, network } => {
            Ok(DaemonRequest::MachineInit { target, network })
        }
        MachineAction::Add { targets } => Ok(DaemonRequest::MachineAdd { targets }),
        MachineAction::Drain { id } => Ok(DaemonRequest::MachineDrain { id }),
        MachineAction::Rm { id, force } => Ok(DaemonRequest::MachineRemove { id, force }),
        MachineAction::Label { id, set, rm } => {
            let set_pairs: Vec<(String, String)> = set
                .into_iter()
                .filter_map(|entry| {
                    let (key, value) = entry.split_once('=')?;
                    Some((key.to_string(), value.to_string()))
                })
                .collect();
            Ok(DaemonRequest::MachineLabel {
                id,
                set: set_pairs,
                remove: rm,
            })
        }
        MachineAction::Invite { action } => match action {
            MachineInviteAction::Create { ttl_secs } => {
                Ok(DaemonRequest::MachineInviteCreate { ttl_secs })
            }
            MachineInviteAction::Import { token } => {
                Ok(DaemonRequest::MachineInviteImport { token })
            }
        },
    }
}

fn render_response(json: bool, plain: bool, quiet: bool, response: &DaemonResponse) -> Result<()> {
    if json {
        let body = serde_json::to_string_pretty(response)
            .map_err(|err| CliError::Serialize(format!("failed to encode JSON output: {err}")))?;
        println!("{body}");
        return Ok(());
    }

    if response.ok {
        if !quiet {
            println!("{}", response.message);
        }
        return Ok(());
    }

    if plain {
        eprintln!("{}", response.message);
    } else {
        eprintln!("error [{}]: {}", response.code, response.message);
    }
    Ok(())
}

fn required_value<T>(value: Option<T>, message: &str) -> Result<T> {
    match value {
        Some(value) => Ok(value),
        None => Err(CliError::Usage(message.to_string())),
    }
}

fn string_arg_or_stdin(
    label: &str,
    stdin_flag: &str,
    value: Option<String>,
    read_stdin: bool,
) -> Result<String> {
    let [has_value, reads_stdin] = [value.is_some(), read_stdin];
    match [has_value, reads_stdin] {
        [true, false] => {
            let Some(text) = value else {
                unreachable!("presence checked above");
            };
            Ok(text)
        }
        [false, true] => read_stdin_string(label),
        [false, false] => Err(CliError::Usage(format!(
            "{label} requires either an argument or {stdin_flag}"
        ))),
        [true, true] => Err(CliError::Usage(format!(
            "{label} cannot use both an argument and {stdin_flag}"
        ))),
    }
}

fn read_text_source(label: &str, path: &str) -> Result<String> {
    match path {
        "-" => read_stdin_string(label),
        other => std::fs::read_to_string(other)
            .map_err(|err| CliError::Io(format!("failed to read {label} from {other}: {err}"))),
    }
}

fn read_stdin_string(label: &str) -> Result<String> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut bytes)
        .map_err(|err| CliError::Usage(format!("failed to read {label} from stdin: {err}")))?;

    String::from_utf8(bytes)
        .map_err(|err| CliError::Usage(format!("{label} from stdin was not valid utf-8: {err}")))
}

fn encode_manifest_json(manifest: DeployManifestInput) -> Result<String> {
    serde_json::to_string(&manifest)
        .map_err(|err| CliError::Serialize(format!("failed to serialize deploy manifest: {err}")))
}

#[allow(clippy::too_many_arguments)]
fn build_service_spec(
    image: &str,
    name: Option<&str>,
    namespace: &str,
    publish: &[String],
    env: &[String],
    volume: &[String],
    network: &str,
    pull: bool,
    restart: &str,
    command: &[String],
) -> ServiceSpec {
    let service_name = match name {
        Some(name) => name.to_string(),
        None => {
            let image_tail = image.rsplit('/').next().unwrap_or(image);
            image_tail
                .split(':')
                .next()
                .unwrap_or(image_tail)
                .to_string()
        }
    };

    let mut service_ports = Vec::new();
    let mut published_ports = Vec::new();
    for (index, mapping) in publish.iter().enumerate() {
        let parts: Vec<&str> = mapping.split(':').collect();
        let [host, container] = parts.as_slice() else {
            eprintln!("warning: ignoring invalid port mapping: {mapping}");
            continue;
        };
        let service_port = format!("port-{:04}", index + 1);
        let Some(host_port) = host.parse().ok() else {
            eprintln!("warning: ignoring invalid host port in mapping: {mapping}");
            continue;
        };
        let Some(container_port) = container.parse().ok() else {
            eprintln!("warning: ignoring invalid container port in mapping: {mapping}");
            continue;
        };
        service_ports.push(ServicePort {
            name: service_port.clone(),
            container_port,
            protocol: PortProtocol::Tcp,
        });
        published_ports.push(PublishedPort {
            service_port,
            host_port,
            host_ip: None,
        });
    }

    let env_map: BTreeMap<String, String> = env
        .iter()
        .filter_map(|entry| {
            let (key, value) = entry.split_once('=')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect();

    let volumes: Vec<VolumeMount> = volume
        .iter()
        .filter_map(|mapping| {
            let parts: Vec<&str> = mapping.splitn(3, ':').collect();
            match parts.as_slice() {
                [src, dst] => Some(VolumeMount {
                    source: VolumeSource::Bind(src.to_string()),
                    target: dst.to_string(),
                    readonly: false,
                }),
                [src, dst, opts] => Some(VolumeMount {
                    source: VolumeSource::Bind(src.to_string()),
                    target: dst.to_string(),
                    readonly: *opts == "ro",
                }),
                _ => {
                    eprintln!("warning: ignoring invalid volume: {mapping}");
                    None
                }
            }
        })
        .collect();

    let network_mode = match network {
        "host" => NetworkMode::Host,
        "none" => NetworkMode::None,
        "overlay" => NetworkMode::Overlay,
        other => NetworkMode::Service(other.to_string()),
    };

    ServiceSpec {
        name: service_name,
        namespace: Namespace(namespace.to_string()),
        placement: Placement::Singleton,
        template: ContainerSpec {
            image: image.to_string(),
            command: if command.is_empty() {
                None
            } else {
                Some(command.to_vec())
            },
            entrypoint: None,
            env: env_map,
            volumes,
            cap_add: vec![],
            cap_drop: vec![],
            privileged: false,
            user: None,
            pull_policy: if pull {
                PullPolicy::Always
            } else {
                PullPolicy::IfNotPresent
            },
            resources: Resources::empty(),
            sysctls: BTreeMap::new(),
        },
        network: network_mode,
        service_ports,
        publish: published_ports,
        routes: vec![],
        readiness: None,
        rollout: RolloutStrategy::Recreate,
        labels: BTreeMap::new(),
        stop_grace_period: None,
        restart: match restart {
            "always" => RestartPolicy::Always,
            "on-failure" => RestartPolicy::OnFailure,
            "no" => RestartPolicy::No,
            "unless-stopped" => RestartPolicy::UnlessStopped,
            _ => RestartPolicy::UnlessStopped,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_deploy_apply_primitives() {
        let cli = Cli::try_parse_from([
            "ployzd",
            "deploy",
            "--namespace",
            "prod",
            "--file",
            "compose.yaml",
        ])
        .expect("deploy apply args should parse");

        let Command::Deploy(command) = cli.command else {
            panic!("expected deploy command");
        };
        assert!(command.action.is_none());
        assert_eq!(command.manifest.namespace.as_deref(), Some("prod"));
        assert_eq!(command.manifest.file.as_deref(), Some("compose.yaml"));
        assert_eq!(command.manifest.format, DeployManifestFormat::Service);
    }

    #[test]
    fn parse_deploy_preview_subcommand() {
        let cli = Cli::try_parse_from([
            "ployzd",
            "deploy",
            "preview",
            "--namespace",
            "prod",
            "--file",
            "-",
            "--format",
            "service",
        ])
        .expect("deploy preview args should parse");

        let Command::Deploy(command) = cli.command else {
            panic!("expected deploy command");
        };
        let Some(DeployAction::Preview(args)) = command.action else {
            panic!("expected deploy preview subcommand");
        };
        assert_eq!(args.namespace.as_deref(), Some("prod"));
        assert_eq!(args.file.as_deref(), Some("-"));
        assert_eq!(args.format, DeployManifestFormat::Service);
    }

    #[test]
    fn parse_deploy_service_subcommand() {
        let cli = Cli::try_parse_from([
            "ployzd",
            "deploy",
            "service",
            "api",
            "--namespace",
            "prod",
            "--image",
            "nginx:latest",
        ])
        .expect("deploy service args should parse");

        let Command::Deploy(command) = cli.command else {
            panic!("expected deploy command");
        };
        let Some(DeployAction::Service(args)) = command.action else {
            panic!("expected deploy service subcommand");
        };
        assert_eq!(args.name, "api");
        assert_eq!(args.namespace, "prod");
        assert_eq!(args.image, "nginx:latest");
    }
}
