use clap::{Parser, Subcommand};
use ployz::transport::{DaemonRequest, Transport, UnixSocketTransport};
use ployz::{Affordances, default_socket_path};
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
}

#[derive(Subcommand)]
enum MeshAction {
    /// Create a mesh network config only.
    Create { network: String },
    /// Create and start a new mesh network.
    Init { network: String },
    /// Start an existing mesh network.
    Up { network: String },
    /// Stop mesh infra but keep network config and data.
    Down,
    /// Tear down all mesh resources and leave the network permanently.
    Destroy { network: String },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let socket = cli
        .socket
        .unwrap_or_else(|| default_socket_path(&Affordances::detect()));
    let transport = UnixSocketTransport::new(socket.clone());

    let request = match &cli.command {
        Command::Status => DaemonRequest::Status,
        Command::Mesh { action } => match action {
            MeshAction::Create { network } => DaemonRequest::MeshCreate {
                network: network.clone(),
            },
            MeshAction::Init { network } => DaemonRequest::MeshInit {
                network: network.clone(),
            },
            MeshAction::Up { network } => DaemonRequest::MeshUp {
                network: network.clone(),
            },
            MeshAction::Down => DaemonRequest::MeshDown,
            MeshAction::Destroy { network } => DaemonRequest::MeshDestroy {
                network: network.clone(),
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
