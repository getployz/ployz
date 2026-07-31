use ployz_telemetry::{FailureClass, Surface, Telemetry};
use ployzd::config::load_daemon_process_config;
use ployzd::dispatch::run_daemon_process_until_shutdown;
use ployzd::logging::role_process_name;
use ployzd::role_cli::{DaemonProcessRole, DaemonRoleParseError, parse_role_args};

fn main() {
    // The role is parsed before the subscriber is installed so every log
    // line carries the process name; the subscriber is installed before
    // telemetry bootstrap so everything the daemon writes after this line
    // is structured. Only telemetry's own pre-subscriber diagnostics remain
    // plain stderr text. A role parse failure still installs a subscriber
    // (under the binary name) so the failure path stays observable.
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let role = parse_role_args(args);
    let process = match &role {
        Ok(role) => role_process_name(role),
        Err(_) => "ployzd",
    };
    ployzd::logging::init_daemon_logging(process);
    let telemetry = Telemetry::bootstrap(Surface::Daemon, env!("CARGO_PKG_VERSION"));
    let runtime = tokio::runtime::Runtime::new().expect("could not start Tokio runtime");
    let result = runtime.block_on(run(&telemetry, role));
    // Telemetry sinks flush with blocking calls and must outlive the async runtime.
    drop(runtime);
    if let Err(error) = &result {
        telemetry.capture_failure(error.failure_class());
        eprintln!("{error}");
    }
    telemetry.shutdown();
    if let Err(error) = result {
        std::process::exit(error.exit_code());
    }
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

async fn run(
    telemetry: &Telemetry,
    role: Result<DaemonProcessRole, DaemonRoleParseError>,
) -> Result<(), MainError> {
    let role = role.map_err(MainError::Role)?;
    let config = load_daemon_process_config(role, env_var).map_err(MainError::Config)?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "daemon process starting"
    );
    telemetry.capture_daemon_started();
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

    const fn failure_class(&self) -> FailureClass {
        match self {
            Self::Role(_) => FailureClass::DaemonRole,
            Self::Config(_) => FailureClass::DaemonConfig,
            Self::Runtime(_) => FailureClass::DaemonRuntime,
        }
    }
}
