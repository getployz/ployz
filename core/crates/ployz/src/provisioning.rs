use std::{
    env,
    ffi::OsString,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use clap::ArgMatches;
use ployz_core::StorageChoice;
use thiserror::Error;
use uuid::Uuid;

use crate::context::{Connection, ConnectionError, SshDestination, Transport};

mod bootstrap;

use bootstrap::Bootstrap;

/// A failure while preparing a Machine through the shared daemon installer.
#[derive(Debug, Error)]
pub enum ProvisionError {
    /// The remote setup command did not include a destination.
    #[error("remote machine destination is required")]
    MissingDestination,
    /// The requested connection is not an SSH connection.
    #[error("Machine provisioning requires an SSH destination, not {0}")]
    RemoteTransport(String),
    /// The SSH destination is malformed or uses a removed transport spelling.
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    /// The local OpenSSH client is unavailable.
    #[error("local ssh client not found; install an ssh client")]
    SshClientMissing(#[source] io::Error),
    /// The initial remote identity command could not be run.
    #[error("run ssh whoami: {0}")]
    Whoami(#[source] io::Error),
    /// The initial remote identity command exited unsuccessfully.
    #[error("ssh whoami failed: {0}")]
    WhoamiFailed(String),
    /// The remote identity was not valid UTF-8.
    #[error("ssh whoami returned non-UTF-8 output")]
    WhoamiUtf8,
    /// The remote identity command returned no user.
    #[error("ssh whoami returned an empty user")]
    EmptyUser,
    /// The remote sudo preflight could not be run.
    #[error("check remote sudo: {0}")]
    Sudo(#[source] io::Error),
    /// A non-root remote user could not authenticate with sudo.
    #[error("remote user {user} could not authenticate or obtain sudo privileges to install Ployz")]
    SudoRequired { user: String },
    /// The remote platform inspection command could not be run.
    #[error("inspect remote Machine platform: {0}")]
    Platform(#[source] io::Error),
    /// The remote platform inspection command exited unsuccessfully.
    #[error("remote Machine platform inspection failed: {0}")]
    PlatformFailed(String),
    /// The remote platform response was not valid UTF-8.
    #[error("remote Machine platform inspection returned non-UTF-8 output")]
    PlatformUtf8,
    /// The target host cannot run the Linux Machine daemon.
    #[error("Ployz Machine must be Linux")]
    UnsupportedOs,
    /// No daemon release archive exists for the target architecture.
    #[error("unsupported Machine architecture: {0}")]
    UnsupportedArchitecture(String),
    /// A bootstrap filesystem or process operation failed.
    #[error("{stage}: {source}")]
    BootstrapIo {
        stage: &'static str,
        #[source]
        source: io::Error,
    },
    /// A bootstrap process exited unsuccessfully.
    #[error("{stage} exited with {status}")]
    BootstrapCommand {
        stage: &'static str,
        status: std::process::ExitStatus,
    },
    /// A bootstrap checksum or version did not match the current CLI release.
    #[error("bootstrap verification: {0}")]
    BootstrapVerification(String),
    /// A published bootstrap release could not be downloaded.
    #[error("{stage}: {source}")]
    BootstrapDownload {
        stage: &'static str,
        #[source]
        source: reqwest::Error,
    },
    /// The bootstrap or its local release files could not be copied to the Machine.
    #[error("transfer bootstrap release: {0}")]
    Transfer(#[source] io::Error),
    /// Remote staging or transfer exited unsuccessfully.
    #[error("bootstrap release transfer exited with {status}")]
    TransferFailed { status: std::process::ExitStatus },
    /// The shared Machine installer could not be spawned.
    #[error("run Ployz installer: {0}")]
    Install(#[source] io::Error),
    /// The shared Machine installer exited unsuccessfully.
    #[error("Ployz installer exited with {status}")]
    InstallFailed { status: std::process::ExitStatus },
    /// Remote bootstrap cleanup could not be run.
    #[error("remove remote bootstrap: {0}")]
    Cleanup(#[source] io::Error),
    /// Remote bootstrap cleanup exited unsuccessfully.
    #[error("remote bootstrap cleanup exited with {status}")]
    CleanupFailed { status: std::process::ExitStatus },
    /// Setup failed and the subsequent remote cleanup also failed.
    #[error("{primary}; cleanup: {cleanup}")]
    CleanupAfter {
        primary: Box<ProvisionError>,
        cleanup: Box<ProvisionError>,
    },
    /// Local Machine installation requires root privileges.
    #[error("run this command with sudo")]
    NotRoot,
    /// Reading an interactive storage selection failed.
    #[error("read storage choice: {0}")]
    StorageInput(#[source] io::Error),
    /// The selected storage value is invalid.
    #[error(transparent)]
    StorageChoice(#[from] ployz_core::ValueError),
    /// ZFS was selected while installation was explicitly disabled.
    #[error("zfs storage preparation requires the installer; remove --no-install")]
    ZfsWithoutInstaller,
}

/// Resolve Machine storage preparation once before provisioning or enrollment.
pub(crate) fn resolve_storage(matches: &ArgMatches) -> Result<StorageChoice, ProvisionError> {
    let storage = match matches.get_one::<StorageChoice>("storage").copied() {
        Some(storage) => storage,
        None if matches.get_flag("yes")
            || !io::stdin().is_terminal()
            || !io::stdout().is_terminal() =>
        {
            StorageChoice::None
        }
        None => {
            print!(
                "Storage preparation [zfs/none] (none keeps this Machine currently stateless): "
            );
            io::stdout().flush().map_err(ProvisionError::StorageInput)?;
            let mut answer = String::new();
            io::stdin()
                .read_line(&mut answer)
                .map_err(ProvisionError::StorageInput)?;
            let answer = answer.trim();
            if answer.is_empty() {
                StorageChoice::None
            } else {
                StorageChoice::parse(answer)?
            }
        }
    };
    if storage == StorageChoice::Zfs && matches.get_flag("no-install") {
        return Err(ProvisionError::ZfsWithoutInstaller);
    }
    announce_storage(storage);
    Ok(storage)
}

pub(crate) fn announce_storage(storage: StorageChoice) {
    if storage == StorageChoice::None {
        println!("Storage: none — this Machine currently supports stateless workloads only.");
    }
}

enum Preparation<'user> {
    Host {
        storage: StorageChoice,
        group_user: Option<&'user str>,
    },
    SoftwareOnly,
}

fn install_arguments(
    version: &str,
    preparation: Preparation<'_>,
    release_dir: Option<&Path>,
) -> Vec<OsString> {
    let mut arguments = vec!["install".into(), "--version".into(), version.into()];
    match preparation {
        Preparation::Host {
            storage,
            group_user,
        } => {
            arguments.extend(["--storage".into(), storage.as_str().into()]);
            if let Some(user) = group_user {
                arguments.extend(["--group-user".into(), user.into()]);
            }
        }
        Preparation::SoftwareOnly => arguments.push("--software-only".into()),
    }
    if let Some(directory) = release_dir {
        arguments.extend(["--release-dir".into(), directory.as_os_str().to_owned()]);
    }
    arguments
}

struct Remote {
    destination: SshDestination,
    key: PathBuf,
    timeout: std::time::Duration,
    control_path: Option<PathBuf>,
}

impl Remote {
    fn from_matches(matches: &ArgMatches) -> Result<Self, ProvisionError> {
        let value = matches
            .get_one::<String>("destination")
            .ok_or(ProvisionError::MissingDestination)?;
        let connection = value.parse::<Connection>()?;
        let Transport::Ssh { destination, .. } = connection.transport() else {
            return Err(ProvisionError::RemoteTransport(connection.to_string()));
        };
        Ok(Self {
            destination: destination.clone(),
            key: ssh_key(matches),
            timeout: crate::cli::ssh_timeout(matches),
            control_path: crate::connect::control_path(),
        })
    }

    fn ssh(&self) -> Command {
        let mut command = Command::new("ssh");
        command.args(self.common_arguments());
        if let Some(port) = self.destination.port() {
            command.args([OsString::from("-p"), port.to_string().into()]);
        }
        // Provisioning may wait for human authentication; only network setup is timed.
        command.args(["-tt", self.destination.target()]);
        command.stdin(Stdio::inherit());
        command
    }

    fn scp(&self) -> Command {
        let mut command = Command::new("scp");
        command.args(self.common_arguments());
        if let Some(port) = self.destination.port() {
            command.args([OsString::from("-P"), port.to_string().into()]);
        }
        command
    }

    fn common_arguments(&self) -> Vec<OsString> {
        let mut arguments: Vec<OsString> =
            crate::connect::ssh_control_args(self.control_path.as_deref())
                .into_iter()
                .map(Into::into)
                .collect();
        arguments.extend([
            "-o".into(),
            format!("ConnectTimeout={}", self.timeout.as_secs().max(1)).into(),
            "-o".into(),
            "BatchMode=no".into(),
            "-i".into(),
            self.key.as_os_str().to_owned(),
        ]);
        arguments
    }

    async fn platform(&self) -> Result<String, ProvisionError> {
        let mut command = self.ssh();
        command.arg("uname -s; uname -m");
        let output = tokio::process::Command::from(command)
            .output()
            .await
            .map_err(ProvisionError::Platform)?;
        if !output.status.success() {
            return Err(ProvisionError::PlatformFailed(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let output = String::from_utf8(output.stdout).map_err(|_| ProvisionError::PlatformUtf8)?;
        let mut lines = output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty());
        if lines.next() != Some("Linux") {
            return Err(ProvisionError::UnsupportedOs);
        }
        lines
            .next()
            .map(str::to_owned)
            .ok_or_else(|| ProvisionError::PlatformFailed("uname returned no architecture".into()))
    }

    async fn stage(
        &self,
        bootstrap: &Bootstrap,
        version: &str,
        directory: &str,
    ) -> Result<(), ProvisionError> {
        let mut create = self.ssh();
        create.arg(format!("mkdir -m 700 -- {}", shell_quote(directory)));
        transfer_status(tokio::process::Command::from(create).status().await)?;

        let mut transfer = self.scp();
        transfer.arg(bootstrap.daemon());
        if let Some(files) = bootstrap.release_files(version) {
            transfer.args(files);
        }
        transfer.arg(format!("{}:{}/", self.destination.copy_target(), directory));
        transfer_status(tokio::process::Command::from(transfer).status().await)
    }

    async fn verify(&self, daemon: &str) -> Result<(), ProvisionError> {
        let mut command = self.ssh();
        command.arg(format!(
            "chmod 700 {path} && {path} version",
            path = shell_quote(daemon)
        ));
        bootstrap::verify_command_version("preflight remote bootstrap daemon", command).await
    }

    async fn install(
        &self,
        daemon: &str,
        arguments: &[OsString],
        via_sudo: bool,
    ) -> Result<(), ProvisionError> {
        let mut command = self.ssh();
        command.arg(remote_command(daemon, arguments, via_sudo));
        installer_status(tokio::process::Command::from(command).status().await)
    }

    async fn cleanup(&self, directory: &str) -> Result<(), ProvisionError> {
        let mut cleanup = self.ssh();
        cleanup.arg(format!("rm -rf -- {}", shell_quote(directory)));
        let status = tokio::process::Command::from(cleanup)
            .status()
            .await
            .map_err(ProvisionError::Cleanup)?;
        if status.success() {
            Ok(())
        } else {
            Err(ProvisionError::CleanupFailed { status })
        }
    }

    async fn close_control_master(&self) {
        let Some(control_path) = self.control_path.as_deref() else {
            return;
        };
        let mut command = Command::new("ssh");
        command.args(crate::connect::ssh_control_args(Some(control_path)));
        if let Some(port) = self.destination.port() {
            command.arg("-p").arg(port.to_string());
        }
        command.args(["-O", "exit", self.destination.target()]);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = tokio::process::Command::from(command).status().await;
    }
}

fn ssh_key(matches: &ArgMatches) -> PathBuf {
    let key = matches
        .get_one::<String>("ssh-key")
        .expect("ssh-key has a default");
    key.strip_prefix("~/").map_or_else(
        || PathBuf::from(key),
        |relative| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(relative)
        },
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn remote_command(program: &str, arguments: &[OsString], via_sudo: bool) -> String {
    let mut command = String::new();
    if via_sudo {
        command.push_str("sudo ");
    }
    command.push_str(&shell_quote(program));
    for argument in arguments {
        command.push(' ');
        command.push_str(&shell_quote(&argument.to_string_lossy()));
    }
    command
}

fn transfer_status(result: io::Result<std::process::ExitStatus>) -> Result<(), ProvisionError> {
    let status = result.map_err(ProvisionError::Transfer)?;
    if status.success() {
        Ok(())
    } else {
        Err(ProvisionError::TransferFailed { status })
    }
}

fn installer_status(result: io::Result<std::process::ExitStatus>) -> Result<(), ProvisionError> {
    let status = result.map_err(ProvisionError::Install)?;
    if status.success() {
        Ok(())
    } else {
        Err(ProvisionError::InstallFailed { status })
    }
}

fn finish_remote(
    primary: Result<(), ProvisionError>,
    cleanup: Result<(), ProvisionError>,
) -> Result<(), ProvisionError> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(primary), Err(cleanup)) => Err(ProvisionError::CleanupAfter {
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
        }),
    }
}

/// Prepare a remote Linux Machine through its verified bootstrap daemon.
///
/// # Errors
///
/// Returns a [`ProvisionError`] when the SSH target or privilege preflight is
/// invalid, when platform inspection, bootstrap acquisition, checksum or version
/// verification fails, when staging or installation fails, or when the staged
/// bootstrap cannot be removed.
pub async fn provision(matches: &ArgMatches, storage: StorageChoice) -> Result<(), ProvisionError> {
    let remote = Remote::from_matches(matches)?;
    let mut whoami = remote.ssh();
    whoami.arg("whoami");
    let output = tokio::process::Command::from(whoami)
        .output()
        .await
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                ProvisionError::SshClientMissing(error)
            } else {
                ProvisionError::Whoami(error)
            }
        })?;
    if !output.status.success() {
        return Err(ProvisionError::WhoamiFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let user = String::from_utf8(output.stdout).map_err(|_| ProvisionError::WhoamiUtf8)?;
    let user = user.trim();
    if user.is_empty() {
        return Err(ProvisionError::EmptyUser);
    }

    let via_sudo = user != "root";
    if via_sudo {
        let mut sudo = remote.ssh();
        sudo.arg("sudo true");
        let status = tokio::process::Command::from(sudo)
            .status()
            .await
            .map_err(ProvisionError::Sudo)?;
        if !status.success() {
            return Err(ProvisionError::SudoRequired {
                user: user.to_owned(),
            });
        }
    }

    let architecture = remote.platform().await?;
    let bootstrap = Bootstrap::acquire(&architecture).await?;
    let version = matches
        .get_one::<String>("version")
        .expect("version has a default");
    let remote_directory = format!("/tmp/ployz-bootstrap-{}", Uuid::new_v4());
    let remote_daemon = format!("{remote_directory}/ployzd");
    let release_dir = bootstrap
        .release_dir(version)
        .map(|_| Path::new(&remote_directory));
    let arguments = install_arguments(
        version,
        Preparation::Host {
            storage,
            group_user: via_sudo.then_some(user),
        },
        release_dir,
    );
    let primary = async {
        remote.stage(&bootstrap, version, &remote_directory).await?;
        remote.verify(&remote_daemon).await?;
        remote.install(&remote_daemon, &arguments, via_sudo).await
    }
    .await;
    let cleanup = remote.cleanup(&remote_directory).await;
    // Host preparation may add the SSH user to the ployz group. A multiplexed
    // session authenticated before installation retains its old group list.
    remote.close_control_master().await;
    finish_remote(primary, cleanup)
}

/// Install and start local `ployzd` through a verified temporary daemon.
///
/// # Errors
///
/// Returns [`ProvisionError::NotRoot`] without local root privileges. It also
/// reports unsupported platforms, release acquisition and filesystem failures,
/// checksum or executable-version verification failures, and installer spawn or
/// non-zero exit failures.
pub async fn provision_local(version: &str, storage: StorageChoice) -> Result<(), ProvisionError> {
    let group_user = env::var("SUDO_USER").ok().filter(|user| !user.is_empty());
    provision_local_with(
        version,
        Preparation::Host {
            storage,
            group_user: group_user.as_deref(),
        },
    )
    .await
}

/// Synchronize the local daemon binary without preparing host software or storage.
///
/// # Errors
///
/// Returns the first privilege, platform, bootstrap acquisition, checksum,
/// executable-version, process, or installation failure.
pub async fn synchronize_local_daemon() -> Result<(), ProvisionError> {
    provision_local_with(env!("CARGO_PKG_VERSION"), Preparation::SoftwareOnly).await
}

async fn provision_local_with(
    version: &str,
    preparation: Preparation<'_>,
) -> Result<(), ProvisionError> {
    if !process_is_root() {
        return Err(ProvisionError::NotRoot);
    }
    if env::consts::OS != "linux" {
        return Err(ProvisionError::UnsupportedOs);
    }
    let bootstrap = Bootstrap::acquire(env::consts::ARCH).await?;
    bootstrap.verify_local_version().await?;
    let arguments = install_arguments(version, preparation, bootstrap.release_dir(version));
    installer_status(
        tokio::process::Command::new(bootstrap.daemon())
            .args(arguments)
            .status()
            .await,
    )
}

pub(crate) fn process_is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|uid| uid.trim() == "0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisioning_ssh_allows_interactive_authentication_with_shared_options() {
        for (extra, seconds) in [(vec![], "5"), (vec!["--ssh-timeout", "17"], "17")] {
            let mut args = vec!["ployz", "machine", "add", "root@host"];
            args.extend(extra);
            let root = crate::cli::command().try_get_matches_from(args).unwrap();
            let matches = root
                .subcommand_matches("machine")
                .unwrap()
                .subcommand_matches("add")
                .unwrap();
            let remote = Remote::from_matches(matches).unwrap();
            let ssh = remote.ssh();
            let scp = remote.scp();
            for command in [&ssh, &scp] {
                let args: Vec<_> = command.get_args().collect();
                assert!(args.contains(&std::ffi::OsStr::new("BatchMode=no")));
                for arg in
                    crate::connect::ssh_control_args(crate::connect::control_path().as_deref())
                {
                    assert!(args.contains(&std::ffi::OsStr::new(&arg)));
                }
                assert!(
                    command
                        .get_args()
                        .any(|arg| arg == format!("ConnectTimeout={seconds}").as_str())
                );
            }
            assert!(ssh.get_args().any(|arg| arg == "-tt"));
        }
    }

    #[test]
    fn shared_installer_arguments_keep_bootstrap_and_target_distinct() {
        assert_eq!(
            install_arguments(
                "1.2.3",
                Preparation::Host {
                    storage: StorageChoice::Zfs,
                    group_user: Some("deploy"),
                },
                None,
            ),
            [
                "install",
                "--version",
                "1.2.3",
                "--storage",
                "zfs",
                "--group-user",
                "deploy",
            ]
            .map(OsString::from)
            .to_vec()
        );
        assert_eq!(
            install_arguments("1.2.3", Preparation::SoftwareOnly, None),
            ["install", "--version", "1.2.3", "--software-only"]
                .map(OsString::from)
                .to_vec()
        );
    }

    #[test]
    fn cleanup_failures_preserve_primary_evidence() {
        let primary = ProvisionError::BootstrapVerification("bad version".into());
        let cleanup = ProvisionError::Cleanup(io::Error::other("ssh failed"));
        let error = finish_remote(Err(primary), Err(cleanup)).unwrap_err();

        assert_eq!(
            error.to_string(),
            "bootstrap verification: bad version; cleanup: remove remote bootstrap: ssh failed"
        );
    }

    #[test]
    fn storage_resolution_honors_explicit_and_safe_noninteractive_choices() {
        assert_eq!(
            storage_matches(["ployz", "machine", "add", "root@host", "--storage", "zfs"]),
            StorageChoice::Zfs
        );
        assert_eq!(
            storage_matches(["ployz", "machine", "init", "root@host", "--storage", "none"]),
            StorageChoice::None
        );
        assert_eq!(
            storage_matches(["ployz", "machine", "add", "root@host", "--yes"]),
            StorageChoice::None
        );
    }

    #[test]
    fn zfs_requires_the_installer() {
        let matches = crate::cli::command()
            .try_get_matches_from([
                "ployz",
                "machine",
                "add",
                "root@host",
                "--storage",
                "zfs",
                "--no-install",
            ])
            .unwrap();
        assert_eq!(
            resolve_storage(
                matches
                    .subcommand_matches("machine")
                    .unwrap()
                    .subcommand_matches("add")
                    .unwrap(),
            )
            .unwrap_err()
            .to_string(),
            "zfs storage preparation requires the installer; remove --no-install"
        );
    }

    fn storage_matches<const N: usize>(args: [&str; N]) -> StorageChoice {
        let matches = crate::cli::command().try_get_matches_from(args).unwrap();
        let (_, matches) = matches
            .subcommand_matches("machine")
            .unwrap()
            .subcommand()
            .unwrap();
        resolve_storage(matches).unwrap()
    }

    #[tokio::test]
    async fn local_provision_requires_root() {
        if process_is_root() {
            return;
        }
        assert!(matches!(
            provision_local(env!("CARGO_PKG_VERSION"), StorageChoice::None).await,
            Err(ProvisionError::NotRoot)
        ));
    }
}
