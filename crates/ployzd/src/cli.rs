use clap::{Args, Parser, Subcommand, ValueEnum};
use ployz_api::{
    DebugTickTask as ProtocolDebugTickTask, InstallRuntimeTarget as ApiInstallRuntimeTarget,
    InstallServiceMode as ApiInstallServiceMode, InstallSource as MachineInstallSource,
};
use ployz_config::{RuntimeTarget, ServiceMode};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum RuntimeTargetArg {
    Docker,
    Host,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum ServiceModeArg {
    User,
    System,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum InstallSourceArg {
    Release,
    Git,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum DebugTickTaskArg {
    PeerSync,
    Heartbeat,
    Heal,
    All,
}

impl From<RuntimeTargetArg> for RuntimeTarget {
    fn from(value: RuntimeTargetArg) -> Self {
        match value {
            RuntimeTargetArg::Docker => RuntimeTarget::Docker,
            RuntimeTargetArg::Host => RuntimeTarget::Host,
        }
    }
}

impl From<ServiceModeArg> for ServiceMode {
    fn from(value: ServiceModeArg) -> Self {
        match value {
            ServiceModeArg::User => ServiceMode::User,
            ServiceModeArg::System => ServiceMode::System,
        }
    }
}

impl From<InstallSourceArg> for MachineInstallSource {
    fn from(value: InstallSourceArg) -> Self {
        match value {
            InstallSourceArg::Release => MachineInstallSource::Release,
            InstallSourceArg::Git => MachineInstallSource::Git,
        }
    }
}

impl From<RuntimeTargetArg> for ApiInstallRuntimeTarget {
    fn from(value: RuntimeTargetArg) -> Self {
        match value {
            RuntimeTargetArg::Docker => ApiInstallRuntimeTarget::Docker,
            RuntimeTargetArg::Host => ApiInstallRuntimeTarget::Host,
        }
    }
}

impl From<ServiceModeArg> for ApiInstallServiceMode {
    fn from(value: ServiceModeArg) -> Self {
        match value {
            ServiceModeArg::User => ApiInstallServiceMode::User,
            ServiceModeArg::System => ApiInstallServiceMode::System,
        }
    }
}

impl From<DebugTickTaskArg> for ProtocolDebugTickTask {
    fn from(value: DebugTickTaskArg) -> Self {
        match value {
            DebugTickTaskArg::PeerSync => ProtocolDebugTickTask::PeerSync,
            DebugTickTaskArg::Heartbeat => ProtocolDebugTickTask::Heartbeat,
            DebugTickTaskArg::Heal => ProtocolDebugTickTask::Heal,
            DebugTickTaskArg::All => ProtocolDebugTickTask::All,
        }
    }
}

#[derive(Debug)]
pub(crate) enum CliError {
    Usage(String),
    Io(String),
    Serialize(String),
    Config(String),
    Identity(String),
    Daemon { code: String, message: String },
    Transport { socket: String, message: String },
}

impl CliError {
    pub(crate) fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) | Self::Config(_) => 2,
            Self::Io(_)
            | Self::Serialize(_)
            | Self::Identity(_)
            | Self::Daemon { .. }
            | Self::Transport { .. } => 1,
        }
    }

    pub(crate) fn print(&self) {
        match self {
            Self::Usage(message)
            | Self::Io(message)
            | Self::Serialize(message)
            | Self::Config(message)
            | Self::Identity(message) => eprintln!("error: {message}"),
            Self::Daemon { code, message } => eprintln!("error [{code}]: {message}"),
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
pub(crate) struct Cli {
    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) config: Option<PathBuf>,

    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) data_dir: Option<PathBuf>,

    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) socket: Option<String>,

    #[arg(long, global = true, conflicts_with = "plain")]
    pub(crate) json: bool,

    #[arg(long, global = true, conflicts_with = "json")]
    pub(crate) plain: bool,

    #[arg(short = 'q', long, global = true)]
    pub(crate) quiet: bool,

    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    Run {
        #[arg(long, value_enum, default_value_t = RuntimeTargetArg::Docker)]
        runtime: RuntimeTargetArg,
        #[arg(long, value_enum, default_value_t = ServiceModeArg::User)]
        service_mode: ServiceModeArg,
        #[arg(long)]
        remote_control_port: Option<u16>,
    },
    Status,
    Doctor,
    #[command(hide = true)]
    Debug {
        #[command(subcommand)]
        action: DebugAction,
    },
    Deploy(Box<DeployCommand>),
    #[command(alias = "network")]
    Mesh {
        #[command(subcommand)]
        action: MeshAction,
    },
    Machine {
        #[command(subcommand)]
        action: MachineAction,
    },
    #[command(hide = true)]
    RpcStdio,
}

#[derive(Debug, Subcommand)]
pub(crate) enum DebugAction {
    #[command(hide = true)]
    Tick {
        #[arg(long, value_enum, default_value_t = DebugTickTaskArg::All)]
        task: DebugTickTaskArg,
        #[arg(long, default_value_t = 1)]
        repeat: u32,
    },
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct DeployCommand {
    #[command(subcommand)]
    pub(crate) action: Option<DeployAction>,

    #[command(flatten)]
    pub(crate) manifest: DeployManifestArgs,
}

#[derive(Debug, Subcommand)]
pub(crate) enum DeployAction {
    Preview(DeployManifestArgs),
    Service(DeployServiceArgs),
}

#[derive(Debug, Args, Clone)]
pub(crate) struct DeployManifestArgs {
    #[arg(short = 'f', long, value_name = "PATH")]
    pub(crate) file: Option<String>,

    #[arg(short = 'n', long)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DeployServiceArgs {
    pub(crate) name: String,

    #[arg(long)]
    pub(crate) image: String,

    #[arg(long)]
    pub(crate) namespace: String,

    #[arg(short, long, value_name = "HOST:CONTAINER")]
    pub(crate) publish: Vec<String>,

    #[arg(short, long, value_name = "KEY=VALUE")]
    pub(crate) env: Vec<String>,

    #[arg(short, long, value_name = "SRC:DST")]
    pub(crate) volume: Vec<String>,

    #[arg(long, default_value = "overlay")]
    pub(crate) network: String,

    #[arg(long)]
    pub(crate) pull: bool,

    #[arg(long, default_value = "unless-stopped")]
    pub(crate) restart: String,

    #[arg(short = 'n', long)]
    pub(crate) dry_run: bool,

    #[arg(last = true)]
    pub(crate) command: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum MeshAction {
    List,
    Status {
        network: String,
    },
    Join {
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        token_stdin: bool,
    },
    Ready {
        #[arg(long)]
        json: bool,
    },
    Create {
        network: String,
    },
    Init {
        network: Option<String>,
        #[arg(long)]
        name_stdin: bool,
    },
    Up {
        network: String,
        #[arg(long)]
        skip_bootstrap_wait: bool,
    },
    Down,
    Destroy {
        network: Option<String>,
        #[arg(long)]
        name_stdin: bool,
    },
    SelfRecord,
    Accept {
        response: String,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum MachineAction {
    #[command(alias = "list")]
    Ls,
    Init {
        target: String,
        #[arg(long)]
        network: String,
        #[arg(long, value_enum)]
        runtime: Option<RuntimeTargetArg>,
        #[arg(long, value_enum)]
        service_mode: Option<ServiceModeArg>,
        #[arg(long, value_enum)]
        install_source: Option<InstallSourceArg>,
        #[arg(long)]
        install_version: Option<String>,
        #[arg(long)]
        install_git_url: Option<String>,
        #[arg(long)]
        install_git_ref: Option<String>,
    },
    Add {
        #[arg(long, value_name = "PATH")]
        identity: Option<PathBuf>,
        #[arg(long, value_enum)]
        runtime: Option<RuntimeTargetArg>,
        #[arg(long, value_enum)]
        service_mode: Option<ServiceModeArg>,
        #[arg(long, value_enum)]
        install_source: Option<InstallSourceArg>,
        #[arg(long)]
        install_version: Option<String>,
        #[arg(long)]
        install_git_url: Option<String>,
        #[arg(long)]
        install_git_ref: Option<String>,
        #[arg(required = true, num_args = 1..)]
        targets: Vec<String>,
    },
    Rm {
        id: String,
        #[arg(long)]
        force: bool,
    },
    Invite {
        #[command(subcommand)]
        action: MachineInviteAction,
    },
    #[command(hide = true)]
    Operation {
        #[command(subcommand)]
        action: MachineOperationAction,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum MachineInviteAction {
    Create {
        #[arg(long, default_value_t = 600)]
        ttl_secs: u64,
    },
    Import {
        #[arg(long)]
        token: String,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum MachineOperationAction {
    List,
    Get { id: String },
}
