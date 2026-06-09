use ployzd::config::load_daemon_process_config;
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
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if let Some(identity) = parse_tunnel_identity_command(&args)? {
        println!("public-key {}", identity.public_key);
        return Ok(());
    }
    let role = parse_role_args(args).map_err(MainError::Role)?;
    let config = load_daemon_process_config(role, env_var).map_err(MainError::Config)?;
    run_daemon_process_until_shutdown(&config)
        .await
        .map_err(MainError::Runtime)
}

fn parse_tunnel_identity_command(
    args: &[String],
) -> Result<Option<TunnelIdentityOutput>, MainError> {
    match args {
        [command, subcommand, flag, path]
            if command == "tunnel" && subcommand == "identity" && flag == "--secret-key-file" =>
        {
            let public_key =
                ployzd::iroh_tunnel::assure_tunnel_identity_file(std::path::Path::new(path))
                    .map_err(MainError::TunnelIdentity)?;
            Ok(Some(TunnelIdentityOutput { public_key }))
        }
        _ => Ok(None),
    }
}

#[derive(Debug)]
struct TunnelIdentityOutput {
    public_key: String,
}

#[derive(Debug)]
enum MainError {
    Role(ployzd::role::DaemonRoleParseError),
    Config(ployzd::config::DaemonProcessConfigError),
    TunnelIdentity(ployzd::iroh_tunnel::TunnelRuntimeError),
    Runtime(ployzd::daemon_runtime::DaemonRuntimeError),
}

impl MainError {
    const fn exit_code(&self) -> i32 {
        match self {
            Self::Role(_) | Self::Config(_) | Self::TunnelIdentity(_) => 2,
            Self::Runtime(_) => 3,
        }
    }
}

impl std::fmt::Display for MainError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Role(error) => write!(formatter, "{error}"),
            Self::Config(error) => write!(formatter, "{error}"),
            Self::TunnelIdentity(error) => write!(formatter, "{error}"),
            Self::Runtime(error) => write!(formatter, "{error}"),
        }
    }
}
