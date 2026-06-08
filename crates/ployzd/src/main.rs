use ployzd::config::{LoadedDaemonProcessConfig, load_daemon_process_config};
use ployzd::daemon_runtime::run_daemon_process_until_shutdown;
use ployzd::role::parse_role_args;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(error.exit_code());
    }
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

async fn run() -> Result<(), MainError> {
    let role = parse_role_args(std::env::args().skip(1)).map_err(MainError::Role)?;
    let loaded = load_daemon_process_config(role.clone(), env_var).map_err(MainError::Config)?;
    match loaded {
        LoadedDaemonProcessConfig::Configured(config) => run_daemon_process_until_shutdown(&config)
            .await
            .map_err(MainError::Runtime),
        LoadedDaemonProcessConfig::TunnelConfigPending { side } => {
            Err(MainError::TunnelConfigPending { side })
        }
    }
}

#[derive(Debug)]
enum MainError {
    Role(ployzd::role::DaemonRoleParseError),
    Config(ployzd::config::DaemonProcessConfigError),
    Runtime(ployzd::daemon_runtime::DaemonRuntimeError),
    TunnelConfigPending { side: ployzd::role::TunnelSide },
}

impl MainError {
    const fn exit_code(&self) -> i32 {
        match self {
            Self::Role(_) | Self::Config(_) => 2,
            Self::Runtime(_) | Self::TunnelConfigPending { .. } => 3,
        }
    }
}

impl std::fmt::Display for MainError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Role(error) => write!(formatter, "{error}"),
            Self::Config(error) => write!(formatter, "{error}"),
            Self::Runtime(error) => write!(formatter, "{error}"),
            Self::TunnelConfigPending { side } => write!(
                formatter,
                "ployzd tunnel {} config is pending",
                side.as_str()
            ),
        }
    }
}
