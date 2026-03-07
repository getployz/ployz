use clap::{Parser, Subcommand};
use ployz::transport::{DaemonRequest, Transport, UnixSocketTransport};
use ployz::{Affordances, load_client_config};
use std::process;

#[derive(Parser)]
#[command(name = "ployz", about = "Ployz operator CLI")]
struct Cli {
    /// Socket path. Defaults to a platform-appropriate path.
    #[arg(long)]
    socket: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show daemon and mesh status.
    Status,
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
    /// Workload sidecar management.
    Workload {
        #[command(subcommand)]
        action: WorkloadAction,
    },
    /// Service management (run containers from specs).
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
}

#[derive(Subcommand)]
enum MeshAction {
    /// List known mesh networks and their local state.
    List,
    /// Show local state for one mesh network.
    Status { network: String },
    /// Join an existing mesh network using an invite token.
    Join {
        #[arg(long)]
        token: String,
    },
    /// Create a mesh network config only.
    Create { network: String },
    /// Create and start a new mesh network.
    Init { network: String },
    /// Start an existing mesh network.
    Up {
        network: String,
        #[arg(long)]
        skip_bootstrap_wait: bool,
    },
    /// Stop mesh infra but keep network config and data.
    Down,
    /// Tear down all mesh resources and leave the network permanently.
    Destroy { network: String },
    /// Print this machine's identity as an encoded JoinResponse (requires running network).
    SelfRecord,
    /// Accept a JoinResponse and seed the joiner's record into the local store.
    Accept { response: String },
}

#[derive(Subcommand)]
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
    Add { target: String },
    /// Invite token operations.
    Invite {
        #[command(subcommand)]
        action: MachineInviteAction,
    },
}

#[derive(Subcommand)]
enum WorkloadAction {
    /// Create a new workload sidecar.
    Create { name: String },
    /// Destroy a workload sidecar.
    Destroy { name: String },
    /// List active workloads.
    #[command(alias = "list")]
    Ls,
}

#[derive(Subcommand)]
enum ServiceAction {
    /// Run a service (like docker run).
    Run {
        /// Container image to run.
        image: String,
        /// Service name. Defaults to image name (without tag/registry).
        #[arg(long)]
        name: Option<String>,
        /// Namespace.
        #[arg(long, default_value = "default")]
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
        /// Network mode (overlay, host, none).
        #[arg(long, default_value = "overlay")]
        network: String,
        /// Always pull image before running.
        #[arg(long)]
        pull: bool,
        /// Restart policy (unless-stopped, always, on-failure, no).
        #[arg(long, default_value = "unless-stopped")]
        restart: String,
        /// Command to run (overrides image CMD).
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// List running services.
    #[command(alias = "list")]
    Ls,
    /// Remove a service.
    #[command(alias = "remove")]
    Rm {
        /// Service name.
        name: String,
        /// Namespace.
        #[arg(long, default_value = "default")]
        namespace: String,
    },
}

#[derive(Subcommand)]
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
    let cli = Cli::parse();
    let resolved = match load_client_config(cli.socket, &Affordances::detect()) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(2);
        }
    };
    let socket = resolved.socket;
    let transport = UnixSocketTransport::new(socket.clone());

    let request = match &cli.command {
        Command::Status => DaemonRequest::Status,
        Command::Mesh { action } => match action {
            MeshAction::List => DaemonRequest::MeshList,
            MeshAction::Status { network } => DaemonRequest::MeshStatus {
                network: network.clone(),
            },
            MeshAction::Join { token } => DaemonRequest::MeshJoin {
                token: token.clone(),
            },
            MeshAction::Create { network } => DaemonRequest::MeshCreate {
                network: network.clone(),
            },
            MeshAction::Init { network } => DaemonRequest::MeshInit {
                network: network.clone(),
            },
            MeshAction::Up {
                network,
                skip_bootstrap_wait,
            } => DaemonRequest::MeshUp {
                network: network.clone(),
                skip_bootstrap_wait: *skip_bootstrap_wait,
            },
            MeshAction::Down => DaemonRequest::MeshDown,
            MeshAction::Destroy { network } => DaemonRequest::MeshDestroy {
                network: network.clone(),
            },
            MeshAction::SelfRecord => DaemonRequest::MeshSelfRecord,
            MeshAction::Accept { response } => DaemonRequest::MeshAccept {
                response: response.clone(),
            },
        },
        Command::Machine { action } => match action {
            MachineAction::Ls => DaemonRequest::MachineList,
            MachineAction::Init { target, network } => DaemonRequest::MachineInit {
                target: target.clone(),
                network: network.clone(),
            },
            MachineAction::Add { target } => DaemonRequest::MachineAdd {
                target: target.clone(),
            },
            MachineAction::Invite { action } => match action {
                MachineInviteAction::Create { ttl_secs } => DaemonRequest::MachineInviteCreate {
                    ttl_secs: *ttl_secs,
                },
                MachineInviteAction::Import { token } => DaemonRequest::MachineInviteImport {
                    token: token.clone(),
                },
            },
        },
        Command::Workload { action } => match action {
            WorkloadAction::Create { name } => DaemonRequest::WorkloadCreate {
                name: name.clone(),
            },
            WorkloadAction::Destroy { name } => DaemonRequest::WorkloadDestroy {
                name: name.clone(),
            },
            WorkloadAction::Ls => DaemonRequest::WorkloadList,
        },
        Command::Service { action } => match action {
            ServiceAction::Run {
                image,
                name,
                namespace,
                publish,
                env,
                volume,
                network,
                pull,
                restart,
                command,
            } => {
                let spec = build_service_spec(
                    image, name, namespace, publish, env, volume, network, pull, restart, command,
                );
                let spec_json = match serde_json::to_string(&spec) {
                    Ok(json) => json,
                    Err(e) => {
                        eprintln!("error: failed to serialize spec: {e}");
                        process::exit(1);
                    }
                };
                DaemonRequest::ServiceRun { spec_json }
            }
            ServiceAction::Ls => DaemonRequest::ServiceList,
            ServiceAction::Rm { name, namespace } => DaemonRequest::ServiceRemove {
                name: name.clone(),
                namespace: namespace.clone(),
            },
        },
    };

    match transport.request(request).await {
        Ok(resp) => {
            println!("{}", resp.message);
            if !resp.ok {
                process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("is ployzd running? (socket: {socket})");
            process::exit(1);
        }
    }
}

fn build_service_spec(
    image: &str,
    name: &Option<String>,
    namespace: &str,
    publish: &[String],
    env: &[String],
    volume: &[String],
    network: &str,
    pull: &bool,
    restart: &str,
    command: &[String],
) -> ployz::workload::spec::ServiceSpec {
    use ployz::workload::spec::*;
    use std::collections::BTreeMap;

    let service_name = name.clone().unwrap_or_else(|| {
        // Derive name from image: "registry/repo:tag" -> "repo"
        let s = image.split('/').last().unwrap_or(image);
        s.split(':').next().unwrap_or(s).to_string()
    });

    let ports: Vec<PortBinding> = publish
        .iter()
        .filter_map(|p| {
            let parts: Vec<&str> = p.split(':').collect();
            if let [host, container] = parts.as_slice() {
                Some(PortBinding {
                    host_port: host.parse().ok()?,
                    container_port: container.parse().ok()?,
                    protocol: PortProtocol::Tcp,
                    host_ip: None,
                })
            } else {
                eprintln!("warning: ignoring invalid port mapping: {p}");
                None
            }
        })
        .collect();

    let env_map: BTreeMap<String, String> = env
        .iter()
        .filter_map(|e| {
            let (k, v) = e.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect();

    let volumes: Vec<VolumeMount> = volume
        .iter()
        .filter_map(|v| {
            let parts: Vec<&str> = v.splitn(3, ':').collect();
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
                    eprintln!("warning: ignoring invalid volume: {v}");
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
        schedule: Schedule::Imperative,
        container: ContainerSpec {
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
            pull_policy: if *pull {
                PullPolicy::Always
            } else {
                PullPolicy::IfNotPresent
            },
            resources: Resources::default(),
            sysctls: BTreeMap::new(),
        },
        network: network_mode,
        ports,
        labels: BTreeMap::new(),
        stop_grace_period: None,
        restart: match restart {
            "always" => RestartPolicy::Always,
            "on-failure" => RestartPolicy::OnFailure,
            "no" => RestartPolicy::No,
            _ => RestartPolicy::UnlessStopped,
        },
    }
}
