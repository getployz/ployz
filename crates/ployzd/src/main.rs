use ployzd::config::load_daemon_process_config;
use ployzd::dispatch::run_daemon_process_until_shutdown;
use ployzd::role_cli::parse_role_args;

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
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let role = parse_role_args(args).map_err(MainError::Role)?;
    let config = load_daemon_process_config(role, env_var).map_err(MainError::Config)?;
    run_daemon_process_until_shutdown(&config)
        .await
        .map_err(MainError::Runtime)
}

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error("{0}")]
    Role(ployzd::role_cli::DaemonRoleParseError),
    #[error("{0}")]
    Config(ployzd::config::DaemonProcessConfigError),
    #[error("{0}")]
    Runtime(ployzd::dispatch::DaemonError),
}

impl MainError {
    const fn exit_code(&self) -> i32 {
        match self {
            Self::Role(_) | Self::Config(_) => 2,
            Self::Runtime(_) => 3,
        }
    }
}
