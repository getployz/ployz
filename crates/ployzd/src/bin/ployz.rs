use clap::{Parser, Subcommand, ValueEnum};
use ployz_config::{RuntimeTarget, ServiceMode};
use ployzd::install;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{self, Command, ExitStatus};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RuntimeTargetArg {
    Docker,
    Host,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ServiceModeArg {
    User,
    System,
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

#[derive(Debug, Parser)]
#[command(name = "ployz", about = "Ployz operator CLI", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<CommandLine>,
}

#[derive(Debug, Subcommand)]
enum CommandLine {
    Daemon {
        #[command(subcommand)]
        action: DaemonCommand,
    },
    #[command(external_subcommand)]
    Passthrough(Vec<OsString>),
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    /// Install or reconfigure the local daemon runtime.
    Install {
        #[arg(long, value_enum)]
        runtime: RuntimeTargetArg,
        #[arg(long, value_enum, default_value_t = ServiceModeArg::User)]
        service_mode: ServiceModeArg,
        #[arg(long, value_name = "PATH")]
        install_manifest: Option<PathBuf>,
    },
}

fn main() {
    match run() {
        Ok(code) => {
            if code != 0 {
                process::exit(code);
            }
        }
        Err(message) => {
            eprintln!("error: {message}");
            process::exit(1);
        }
    }
}

fn run() -> Result<i32, String> {
    let cli = Cli::parse();
    match cli.command {
        Some(CommandLine::Daemon {
            action:
                DaemonCommand::Install {
                    runtime,
                    service_mode,
                    install_manifest,
                },
        }) => {
            let manifest = install::daemon_install(
                runtime.into(),
                service_mode.into(),
                install_manifest.as_deref(),
            )?;
            let backend = manifest
                .service_backend
                .map(install::ServiceBackend::as_str)
                .unwrap_or("none");
            println!(
                "daemon install complete\n  runtime: {}\n  service-mode: {}\n  backend: {backend}\n  config: {}\n  socket: {}",
                runtime_target_name(manifest.runtime_target),
                service_mode_name(manifest.service_mode),
                manifest.config_path.display(),
                manifest.socket_path
            );
            Ok(0)
        }
        Some(CommandLine::Passthrough(args)) => forward_to_ployzd(&args),
        None => forward_to_ployzd(&[]),
    }
}

fn forward_to_ployzd(args: &[OsString]) -> Result<i32, String> {
    let current_exe =
        std::env::current_exe().map_err(|error| format!("current_exe failed: {error}"))?;
    let ployzd_path = current_exe.with_file_name("ployzd");
    let status = Command::new(&ployzd_path)
        .args(args)
        .status()
        .map_err(|error| format!("run '{}': {error}", ployzd_path.display()))?;
    Ok(exit_code(status))
}

fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

fn runtime_target_name(runtime_target: RuntimeTarget) -> &'static str {
    match runtime_target {
        RuntimeTarget::Docker => "docker",
        RuntimeTarget::Host => "host",
    }
}

fn service_mode_name(service_mode: ServiceMode) -> &'static str {
    match service_mode {
        ServiceMode::User => "user",
        ServiceMode::System => "system",
    }
}
