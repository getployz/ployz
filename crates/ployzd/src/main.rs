use ployz_telemetry::{FailureClass, Surface, Telemetry};
use ployzd::dispatch::run_daemon_process;
use ployzd::role_cli::parse_role_args;

fn main() {
    // The subscriber is installed before telemetry bootstrap so everything
    // the daemon writes after this line is structured; only telemetry's own
    // pre-subscriber diagnostics remain plain stderr text.
    ployzd::logging::init_daemon_logging();
    let telemetry = Telemetry::bootstrap(Surface::Daemon, env!("CARGO_PKG_VERSION"));
    let runtime = tokio::runtime::Runtime::new().expect("could not start Tokio runtime");
    let result = runtime.block_on(run(&telemetry));
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

async fn run(telemetry: &Telemetry) -> Result<(), MainError> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let role = parse_role_args(args).map_err(MainError::Role)?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        role = role.as_str(),
        "daemon process starting"
    );
    telemetry.capture_daemon_started();
    run_daemon_process(role).await.map_err(MainError::Runtime)
}

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error("{0}")]
    Role(ployzd::role_cli::DaemonRoleParseError),
    #[error("{0}")]
    Runtime(ployzd::dispatch::DaemonError),
}

impl MainError {
    const fn exit_code(&self) -> i32 {
        match self {
            Self::Role(_) => 2,
            Self::Runtime(ployzd::dispatch::DaemonError::Keeper(
                ployzd::roles::keeper::KeeperRoleRuntimeError::UpgradeFirstConverge { .. },
            )) => 75,
            Self::Runtime(_) => 3,
        }
    }

    const fn failure_class(&self) -> FailureClass {
        match self {
            Self::Role(_) => FailureClass::DaemonRole,
            Self::Runtime(_) => FailureClass::DaemonRuntime,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_first_converge_uses_the_systemd_revert_exit_status() {
        let error = MainError::Runtime(ployzd::dispatch::DaemonError::Keeper(
            ployzd::roles::keeper::KeeperRoleRuntimeError::UpgradeFirstConverge {
                detail: "mesh convergence failed".to_owned(),
            },
        ));

        assert_eq!(error.exit_code(), 75);
    }
}
