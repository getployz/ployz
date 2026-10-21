#[cfg(not(target_os = "linux"))]
compile_error!("ployzd supports Linux only");

mod volume_plugin;

use std::{
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use clap::{Parser, Subcommand};
use ployz_core::{
    MachineRelease, MachineUpgradeAttemptId, NETWORK_CONFLICT_EXIT_STATUS, StoragePreparation,
};
use ployzd::{
    daemon::{ContainerMode, Daemon, DaemonConfig, Error, wait_until_socket_accepts},
    diag,
    dns::{self, DnsExit},
    installer::{DEFAULT_SOCKET_PATH, InstallMode, InstallRequest, Readiness},
    machine::DEFAULT_DATA_DIR,
    management::ManagementConfig,
    runtime_unit::UnitSupervision,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt, copy, stdin, stdout},
    net::UnixStream,
};

const DIAL_STDIO_SOCKET_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Parser)]
#[command(about = "Ployz Machine daemon")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(short, long, default_value = DEFAULT_DATA_DIR)]
    data_dir: PathBuf,
    #[arg(long, default_value = DEFAULT_SOCKET_PATH)]
    socket: PathBuf,
    #[arg(long = "dns-upstream", value_name = "ADDR")]
    dns_upstreams: Vec<SocketAddr>,
    #[arg(long, hide = true)]
    machine_api_address: Option<SocketAddr>,
    /// UDP port of the management transport; tests pass 0 to avoid the fixed port.
    #[arg(long, hide = true, default_value_t = ployz_core::MANAGEMENT_PORT)]
    management_port: u16,
    #[arg(long)]
    containerd_socket: Option<PathBuf>,
    #[arg(long, hide = true)]
    volume_plugin_socket: Option<PathBuf>,
    /// Tracing filter. Overrides `PLOYZ_LOG`. Default: info.
    #[arg(long, value_name = "FILTER")]
    log_level: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Print the daemon version.
    Version,
    /// Bridge standard input/output to the local Machine API socket.
    #[command(hide = true)]
    DialStdio,
    /// Serve Internal DNS from the spec the daemon publishes under its run directory.
    #[command(hide = true)]
    Dns {
        /// Exit 0 without serving; tells an older daemon this release serves DNS itself.
        #[arg(long, hide = true)]
        probe: bool,
    },
    /// Serve the Docker Volume plugin on its systemd socket.
    VolumePlugin,
    /// Hold the log files of Ployz containers in the Log Store.
    Observe {
        #[command(subcommand)]
        command: Option<ObserveCommand>,
    },
    /// Execute one accepted Machine upgrade from its transient systemd service.
    #[command(hide = true)]
    UpgradeWorker {
        #[arg(long)]
        attempt: MachineUpgradeAttemptId,
    },
    /// Write this release's systemd unit files for an installer running another release.
    ///
    /// Other releases' installers call these flags; renaming one makes them exit 2 and write their own units.
    #[command(hide = true)]
    InstallUnits {
        #[arg(long)]
        bin_dir: PathBuf,
        #[arg(long)]
        run_dir: PathBuf,
        #[arg(long)]
        systemd_dir: PathBuf,
    },
    /// Install or replace this Machine's daemon release.
    Install {
        /// Release channel (stable or beta) or exact published version.
        #[arg(long, default_value = "stable")]
        version: MachineRelease,
        /// Prepare ZFS storage, or leave this Machine stateless.
        #[arg(long, default_value = "none")]
        storage: StoragePreparation,
        /// Replace daemon software only; do not install Docker, OS packages, or prepare storage.
        #[arg(long)]
        software_only: bool,
        /// Write files and units but do not contact or start systemd.
        #[arg(long)]
        install_only: bool,
        /// Add this existing operator to the Ployz service group during host preparation.
        #[arg(long, value_name = "USER")]
        group_user: Option<String>,
        /// Print what this host is and what blocks Ployz on it, without changing anything.
        #[arg(long, requires = "json", conflicts_with_all = ["software_only", "install_only", "group_user", "storage", "version"])]
        probe: bool,
        /// Print the probe as JSON.
        #[arg(long, requires = "probe")]
        json: bool,
        /// Install Docker and build ZFS ahead of enroll, without starting Ployz or creating a pool.
        #[arg(long, conflicts_with_all = ["software_only", "install_only", "group_user", "storage", "version", "probe"])]
        prepare: bool,
        /// Build no ZFS during `--prepare`.
        #[arg(long, requires = "prepare")]
        skip_zfs: bool,
        /// Remove what `--prepare` built for ZFS.
        #[arg(long, hide = true, conflicts_with_all = ["software_only", "install_only", "group_user", "storage", "version", "probe", "prepare"])]
        discard_zfs_build: bool,
    },
}

#[derive(Subcommand)]
enum ObserveCommand {
    /// Print Log Store or Docker `local` log files as `docker logs -t` does.
    #[command(hide = true)]
    Decode {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
}

fn main() -> ExitCode {
    let args = Args::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("{error}", error = ployz_core::error_chain::inline(&error),);
            return ExitCode::FAILURE;
        }
    };
    let code = match runtime.block_on(run(args)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}", error = ployz_core::error_chain::inline(&error),);
            daemon_error_exit_code(&error)
        }
    };
    runtime.shutdown_timeout(Duration::from_secs(5));
    code
}

fn daemon_error_exit_code(error: &Error) -> ExitCode {
    if error.needs_operator() {
        ExitCode::from(NETWORK_CONFLICT_EXIT_STATUS)
    } else {
        ExitCode::FAILURE
    }
}

async fn run(args: Args) -> Result<ExitCode, Error> {
    if matches!(args.command, Some(Command::Version)) {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(Command::Observe {
        command: Some(ObserveCommand::Decode { paths }),
    }) = &args.command
    {
        ployzd::observe::decode(paths)?;
        return Ok(ExitCode::SUCCESS);
    }
    if matches!(args.command, Some(Command::DialStdio)) {
        dial_stdio(&args.socket).await?;
        return Ok(ExitCode::SUCCESS);
    }
    if matches!(args.command, Some(Command::Dns { probe: true })) {
        return Ok(ExitCode::SUCCESS);
    }
    let run_dir = args
        .socket
        .parent()
        .unwrap_or_else(|| Path::new("/run/ployz"))
        .to_owned();
    if let Some(Command::UpgradeWorker { attempt }) = args.command {
        ployzd::installer::upgrade::run_worker(attempt, &args.data_dir, &run_dir)
            .await
            .map_err(io::Error::other)?;
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(Command::InstallUnits {
        bin_dir,
        run_dir,
        systemd_dir,
    }) = &args.command
    {
        ployzd::installer::write_units(bin_dir, run_dir, systemd_dir).map_err(io::Error::other)?;
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(Command::Install {
        version,
        storage,
        software_only,
        install_only,
        group_user,
        probe,
        json: _,
        prepare,
        skip_zfs,
        discard_zfs_build,
    }) = args.command
    {
        if prepare {
            ployzd::installer::prepare(skip_zfs)
                .await
                .map_err(io::Error::other)?;
            return Ok(ExitCode::SUCCESS);
        }
        if discard_zfs_build {
            ployzd::installer::discard_zfs_build().map_err(io::Error::other)?;
            return Ok(ExitCode::SUCCESS);
        }
        if probe {
            println!("{}", ployzd::installer::probe().map_err(io::Error::other)?);
            return Ok(ExitCode::SUCCESS);
        }
        let request = install_request(version, storage, software_only, install_only, group_user)
            .map_err(Error::from)?;
        let outcome = ployzd::installer::install(request, &args.data_dir, &args.socket)
            .await
            .map_err(io::Error::other)?;
        match outcome.readiness {
            Readiness::InstallationOnly => {
                println!(
                    "Ployz {} installed; systemd was not started",
                    outcome.target
                );
            }
            Readiness::Running => {
                println!("Ployz {} is running and ready", outcome.target);
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    diag::init(args.log_level.as_deref())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if matches!(args.command, Some(Command::Observe { command: None })) {
        ployzd::observe::run(&run_dir, &args.data_dir).await?;
        return Ok(ExitCode::SUCCESS);
    }
    if matches!(args.command, Some(Command::VolumePlugin)) {
        let socket = volume_plugin::listener(Path::new(ployzd::VOLUME_PLUGIN_SOCKET))?;
        volume_plugin::run(socket.listener, &args.data_dir, &run_dir).await?;
        drop(socket.claim);
        return Ok(ExitCode::SUCCESS);
    }
    if matches!(args.command, Some(Command::Dns { .. })) {
        return Ok(match dns::serve(&run_dir).await? {
            DnsExit::Abdicated => ExitCode::from(dns::ABDICATED),
            DnsExit::Stopped | DnsExit::SpecChanged => ExitCode::SUCCESS,
        });
    }
    let daemon = Daemon::start(DaemonConfig {
        data_dir: args.data_dir,
        socket: args.socket,
        dns_upstreams: args.dns_upstreams,
        units: UnitSupervision::detect(),
        machine_api_address: args.machine_api_address,
        containerd_socket: args.containerd_socket,
        volume_plugin_socket: args.volume_plugin_socket,
        containers: ContainerMode::Auto,
        management: ManagementConfig {
            port: args.management_port,
            ..ManagementConfig::default()
        },
    })
    .await?;
    daemon.wait().await?;
    Ok(ExitCode::SUCCESS)
}

fn install_request(
    release: MachineRelease,
    storage: StoragePreparation,
    software_only: bool,
    install_only: bool,
    group_user: Option<String>,
) -> io::Result<InstallRequest> {
    if install_only && storage != StoragePreparation::None {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--install-only cannot be combined with --storage",
        ));
    }
    if install_only && group_user.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--install-only cannot be combined with --group-user",
        ));
    }
    let mode = if install_only {
        InstallMode::InstallationOnly
    } else if software_only {
        if storage != StoragePreparation::None {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--software-only cannot be combined with --storage",
            ));
        }
        if group_user.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--software-only cannot be combined with --group-user",
            ));
        }
        InstallMode::SoftwareOnly
    } else {
        InstallMode::PrepareHost {
            storage,
            group_user,
        }
    };
    Ok(InstallRequest { release, mode })
}

async fn dial_stdio(path: &Path) -> io::Result<()> {
    let stream = wait_until_socket_accepts(path, DIAL_STDIO_SOCKET_TIMEOUT).await?;
    bridge(stdin(), stdout(), stream).await
}

async fn bridge(
    mut input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
    stream: UnixStream,
) -> io::Result<()> {
    let (mut socket_read, mut socket_write) = stream.into_split();
    let forward = async {
        copy(&mut input, &mut socket_write).await?;
        socket_write.shutdown().await
    };
    let back = async {
        copy(&mut socket_read, &mut output).await?;
        output.flush().await
    };
    tokio::try_join!(forward, back).map(|_| ())
}

#[cfg(test)]
mod tests {
    use ployzd::network::NetworkError;
    use tokio::{
        io::{AsyncReadExt, duplex},
        net::UnixListener,
    };

    use super::*;

    fn release(value: &str) -> MachineRelease {
        MachineRelease::parse(value).unwrap()
    }

    async fn bridged(
        input: impl AsyncRead + Unpin,
        daemon: impl AsyncFnOnce(UnixStream),
    ) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ployz.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (output, mut client_output) = duplex(64);
        let stream = UnixStream::connect(&path).await.unwrap();
        let daemon = async {
            let (socket, _) = listener.accept().await.unwrap();
            daemon(socket).await;
        };
        let bridged = tokio::time::timeout(Duration::from_secs(5), bridge(input, output, stream));
        let ((), bridged) = tokio::join!(daemon, bridged);
        bridged
            .expect("bridge outlived the daemon's socket")
            .unwrap();
        let mut seen = Vec::new();
        client_output.read_to_end(&mut seen).await.unwrap();
        seen
    }

    #[tokio::test]
    async fn bridge_ends_when_the_daemon_closes_while_stdin_stays_open() {
        let (_client_input, input) = duplex(64);
        let seen = bridged(input, async |mut socket: UnixStream| {
            socket.write_all(b"reply").await.unwrap();
        })
        .await;
        assert_eq!(seen, b"reply");
    }

    #[tokio::test]
    async fn bridge_returns_the_reply_sent_after_stdin_closes() {
        let (mut client_input, input) = duplex(64);
        client_input.write_all(b"request").await.unwrap();
        drop(client_input);
        let seen = bridged(input, async |mut socket: UnixStream| {
            let mut request = Vec::new();
            socket.read_to_end(&mut request).await.unwrap();
            assert_eq!(request, b"request");
            socket.write_all(b"reply").await.unwrap();
        })
        .await;
        assert_eq!(seen, b"reply");
    }

    #[test]
    fn network_conflicts_use_the_dedicated_exit_status() {
        let conflict = Error::Network(NetworkError::DockerNetworkConflict {
            reason: "ownership is unproven".into(),
            expected: "expected".into(),
            observed: "observed".into(),
            recovery: "recovery",
        });

        assert_eq!(
            daemon_error_exit_code(&conflict),
            ExitCode::from(NETWORK_CONFLICT_EXIT_STATUS)
        );
        let wireguard = Error::Network(NetworkError::WireGuardConflict {
            reason: "the existing device identity differs from the Machine record",
            recovery: "recovery",
        });
        assert_eq!(
            daemon_error_exit_code(&wireguard),
            ExitCode::from(NETWORK_CONFLICT_EXIT_STATUS)
        );
        assert_eq!(
            daemon_error_exit_code(&Error::RecordOwner(ployzd::machine::RecordOwnerStopped)),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn install_cli_rejects_host_options_for_software_only_replacement() {
        let storage = install_request(
            release("stable"),
            StoragePreparation::Zfs { device: None },
            true,
            false,
            None,
        )
        .unwrap_err();
        assert_eq!(storage.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            storage.to_string(),
            "--software-only cannot be combined with --storage"
        );

        let group = install_request(
            release("stable"),
            StoragePreparation::None,
            true,
            false,
            Some("operator".into()),
        )
        .unwrap_err();
        assert_eq!(group.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            group.to_string(),
            "--software-only cannot be combined with --group-user"
        );
    }

    #[test]
    fn install_cli_rejects_host_options_for_installation_only() {
        let storage = install_request(
            release("stable"),
            StoragePreparation::Zfs { device: None },
            false,
            true,
            None,
        )
        .unwrap_err();
        assert_eq!(storage.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            storage.to_string(),
            "--install-only cannot be combined with --storage"
        );

        let group = install_request(
            release("stable"),
            StoragePreparation::None,
            false,
            true,
            Some("operator".into()),
        )
        .unwrap_err();
        assert_eq!(group.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            group.to_string(),
            "--install-only cannot be combined with --group-user"
        );
    }

    #[test]
    fn install_cli_builds_one_explicit_mode() {
        let replacement =
            install_request(release("1.2.3"), StoragePreparation::None, true, true, None).unwrap();
        assert!(matches!(replacement.mode, InstallMode::InstallationOnly));

        let host = install_request(
            release("1.2.3"),
            StoragePreparation::Zfs { device: None },
            false,
            false,
            Some("operator".into()),
        )
        .unwrap();
        assert!(matches!(
            host.mode,
            InstallMode::PrepareHost {
                storage: StoragePreparation::Zfs { device: None },
                group_user: Some(_)
            }
        ));
    }
}
