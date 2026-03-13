use clap::{Parser, Subcommand, ValueEnum};
use ployzd::{Mode, install};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{self, Command, ExitStatus};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RuntimeMode {
    Docker,
    HostExec,
    HostService,
}

impl From<RuntimeMode> for Mode {
    fn from(value: RuntimeMode) -> Self {
        match value {
            RuntimeMode::Docker => Mode::Docker,
            RuntimeMode::HostExec => Mode::HostExec,
            RuntimeMode::HostService => Mode::HostService,
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
        mode: RuntimeMode,
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
                    mode,
                    install_manifest,
                },
        }) => {
            let manifest = install::daemon_install(mode.into(), install_manifest.as_deref())?;
            let configured = manifest.configured_mode.map(mode_name).unwrap_or("unknown");
            let backend = manifest
                .service_backend
                .map(install::ServiceBackend::as_str)
                .unwrap_or("none");
            println!(
                "daemon install complete\n  mode: {configured}\n  backend: {backend}\n  config: {}\n  socket: {}",
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

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Memory => "memory",
        Mode::Docker => "docker",
        Mode::HostExec => "host-exec",
        Mode::HostService => "host-service",
    }
}
