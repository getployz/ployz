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
    /// Seed the invites table with random test rows.
    DebugSeedInvites {
        #[arg(default_value_t = 10000)]
        count: u64,
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
        Command::DebugSeedInvites { count } => DaemonRequest::DebugSeedInvites { count: *count },
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
