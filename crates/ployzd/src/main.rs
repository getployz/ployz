use ployz_telemetry::{ConfigFile, Consent, PersistedTelemetry, Surface, Telemetry};
use ployzd::config::load_daemon_process_config;
use ployzd::dispatch::run_daemon_process_until_shutdown;
use ployzd::role_cli::parse_role_args;

const TELEMETRY_NOTICE: &str = "Ployz collects anonymous usage and error telemetry. Disable it with `ployz telemetry disable`.";
const SENTRY_DSN: &str = match option_env!("PLOYZ_SENTRY_DSN") {
    Some(value) => value,
    None => "",
};
const POSTHOG_KEY: &str = match option_env!("PLOYZ_POSTHOG_KEY") {
    Some(value) => value,
    None => "",
};

#[tokio::main]
async fn main() {
    let (mut telemetry, mut telemetry_config, telemetry_enabled) = start_telemetry();
    let result = run(&mut telemetry, telemetry_config.as_mut(), telemetry_enabled).await;
    if let Err(error) = &result {
        if let Some(telemetry) = &telemetry {
            telemetry.capture_error(error, error.failure_tag());
        }
        eprintln!("{error}");
    }
    if let Some(telemetry) = telemetry {
        telemetry.shutdown();
    }
    if let Err(error) = result {
        std::process::exit(error.exit_code());
    }
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

async fn run(
    telemetry: &mut Option<Telemetry>,
    telemetry_config: Option<&mut ConfigFile>,
    telemetry_enabled: bool,
) -> Result<(), MainError> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let role = parse_role_args(args).map_err(MainError::Role)?;
    let config = load_daemon_process_config(role, env_var).map_err(MainError::Config)?;
    if let Some(telemetry) = telemetry {
        let _attach_result =
            telemetry.attach_posthog(POSTHOG_KEY, config.machine_id().as_str().to_owned());
    }
    if telemetry_enabled {
        show_notice(telemetry_config);
    }
    if let Some(telemetry) = telemetry {
        telemetry.capture_daemon_started();
    }
    run_daemon_process_until_shutdown(&config)
        .await
        .map_err(MainError::Runtime)
}

fn start_telemetry() -> (Option<Telemetry>, Option<ConfigFile>, bool) {
    let config = match ConfigFile::load_or_create_default() {
        Ok(config) => Some(config),
        Err(error) => {
            eprintln!("{error}");
            None
        }
    };
    if let Some(diagnostic) = config.as_ref().and_then(ConfigFile::diagnostic) {
        eprintln!("{diagnostic}");
    }
    let consent = Consent::from_environment(
        config
            .as_ref()
            .map_or(PersistedTelemetry::Corrupt, ConfigFile::persisted_telemetry),
    );
    let enabled = consent.is_enabled();
    let telemetry = Telemetry::start(
        Surface::Daemon,
        env!("CARGO_PKG_VERSION"),
        consent,
        SENTRY_DSN,
    )
    .ok();
    (telemetry, config, enabled)
}

fn show_notice(config: Option<&mut ConfigFile>) {
    let Some(config) = config else {
        return;
    };
    if !config.notice_needed(Surface::Daemon) {
        return;
    }
    eprintln!("{TELEMETRY_NOTICE}");
    if let Err(error) = config.mark_notice_seen(Surface::Daemon) {
        eprintln!("{error}");
    }
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

    const fn failure_tag(&self) -> &'static str {
        match self {
            Self::Role(_) => "role",
            Self::Config(_) => "config",
            Self::Runtime(_) => "runtime",
        }
    }
}
