use ployz_core::{error_chain, shell};
use std::{
    env,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use ployz_core::{Hint, RpcErrorCode, StoragePreparation};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::coded::Coded;
use crate::context::{Connection, ConnectionError, SshDestination, Transport};

mod bootstrap;
#[cfg(test)]
mod setup_tests;

use bootstrap::Bootstrap;

/// Whether this process runs as root.
pub(crate) fn process_is_root() -> bool {
    rustix::process::geteuid().is_root()
}

/// A failure while preparing a Machine through the shared daemon installer.
#[derive(Debug, Error)]
pub enum ProvisionError {
    #[error("remote machine destination is required")]
    MissingDestination,
    #[error("Machine provisioning requires an SSH destination, not {0}")]
    RemoteTransport(crate::cli::Shown),
    /// The SSH destination is malformed or uses a removed transport spelling.
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error("local ssh client not found; install an ssh client")]
    SshClientMissing,
    /// The initial remote identity command could not be run.
    #[error("Could not run ssh whoami on the Machine.")]
    Whoami(#[source] io::Error),
    /// The initial remote identity command exited unsuccessfully.
    #[error("ssh whoami failed: {0}")]
    WhoamiFailed(String),
    #[error("ssh whoami returned non-UTF-8 output")]
    WhoamiUtf8,
    #[error("ssh whoami returned an empty user")]
    EmptyUser,
    /// The remote sudo preflight could not be run.
    #[error("Could not check sudo on the Machine.")]
    Sudo(#[source] io::Error),
    /// A non-root remote user could not authenticate with sudo.
    #[error("remote user {user} could not authenticate or obtain sudo privileges to install Ployz")]
    SudoRequired { user: String },
    /// The remote platform inspection command could not be run.
    #[error("Could not inspect the Machine platform.")]
    Platform(#[source] io::Error),
    /// The remote platform inspection command exited unsuccessfully.
    #[error("remote Machine platform inspection failed: {0}")]
    PlatformFailed(String),
    #[error("remote Machine platform inspection returned non-UTF-8 output")]
    PlatformUtf8,
    #[error("Ployz Machine must be Linux")]
    UnsupportedOs,
    #[error("unsupported Machine architecture: {0}")]
    UnsupportedArchitecture(String),
    /// A bootstrap filesystem or process operation failed.
    #[error("Could not {stage}.")]
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
    /// The bootstrap release publishes no signature.
    #[error("Release {0} is not signed, and Ployz installs only signed releases.")]
    BootstrapUnsigned(ployz_core::MachineVersion),
    /// The bootstrap release is not signed by the Ployz release key.
    #[error("bootstrap verification")]
    BootstrapSignature(#[source] ployz_core::ReleaseSignatureError),
    /// A published bootstrap release could not be downloaded.
    #[error("Could not {stage}.")]
    BootstrapDownload {
        stage: &'static str,
        #[source]
        source: reqwest::Error,
    },
    /// The bootstrap or its local release files could not be copied to the Machine.
    #[error("Could not transfer the bootstrap release to the Machine.")]
    Transfer(#[source] io::Error),
    /// Remote staging or transfer exited unsuccessfully.
    #[error("bootstrap release transfer exited with {status}")]
    TransferFailed { status: std::process::ExitStatus },
    /// The shared Machine installer could not be spawned.
    #[error("Could not run the Ployz installer.")]
    Install(#[source] io::Error),
    /// The shared Machine installer exited unsuccessfully.
    #[error("Ployz installer exited with {status}")]
    InstallFailed { status: std::process::ExitStatus },
    /// The local installer exited unsuccessfully, and this was the last line it wrote.
    #[error("{reason}")]
    InstallRefused { reason: String },
    /// The Server's First Key could not be armed with this computer's key.
    #[error(
        "Could not give the Server this computer's key: ployzd arm-first-key exited with {status}. Upgrade ployz on the Server, or run ployz server add --command and the line it prints there."
    )]
    FirstKeyFailed { status: std::process::ExitStatus },
    /// Remote bootstrap cleanup could not be run.
    #[error("Could not remove the remote bootstrap.")]
    Cleanup(#[source] io::Error),
    /// Remote bootstrap cleanup exited unsuccessfully.
    #[error("remote bootstrap cleanup exited with {status}")]
    CleanupFailed { status: std::process::ExitStatus },
    /// Setup failed and the subsequent remote cleanup also failed. The causes
    /// are the setup's; the cleanup's failure goes in `details.cleanup` and
    /// `remove` becomes the `next:` line.
    #[error("{}; the remote bootstrap was not cleaned up.", .primary.to_string().trim_end_matches('.'))]
    CleanupAfter {
        primary: Box<ProvisionError>,
        #[source]
        causes: Option<ployz_core::error_chain::JoinedChain>,
        cleanup: Box<ProvisionError>,
        /// The command that removes what was left behind.
        remove: String,
    },
    #[error("run this command with sudo")]
    NotRoot,
    /// ZFS was selected while installation was explicitly disabled.
    #[error("zfs storage preparation requires the installer; remove --no-install")]
    ZfsWithoutInstaller,
}

impl Coded for ProvisionError {
    fn code(&self) -> RpcErrorCode {
        match self {
            ProvisionError::CleanupAfter { primary, .. } => primary.code(),
            ProvisionError::MissingDestination
            | ProvisionError::RemoteTransport(_)
            | ProvisionError::Connection(_)
            | ProvisionError::ZfsWithoutInstaller => RpcErrorCode::InvalidArgument,
            ProvisionError::NotRoot | ProvisionError::SudoRequired { .. } => {
                RpcErrorCode::Unauthenticated
            }
            ProvisionError::UnsupportedOs
            | ProvisionError::UnsupportedArchitecture(_)
            | ProvisionError::BootstrapUnsigned(_) => RpcErrorCode::Unsupported,
            ProvisionError::SshClientMissing
            | ProvisionError::Whoami(_)
            | ProvisionError::WhoamiFailed(_)
            | ProvisionError::Sudo(_)
            | ProvisionError::Platform(_)
            | ProvisionError::PlatformFailed(_)
            | ProvisionError::BootstrapDownload { .. }
            | ProvisionError::Transfer(_)
            | ProvisionError::TransferFailed { .. } => RpcErrorCode::Unavailable,
            ProvisionError::WhoamiUtf8
            | ProvisionError::EmptyUser
            | ProvisionError::PlatformUtf8
            | ProvisionError::BootstrapIo { .. }
            | ProvisionError::BootstrapCommand { .. }
            | ProvisionError::BootstrapVerification(_)
            | ProvisionError::BootstrapSignature(_)
            | ProvisionError::Install(_)
            | ProvisionError::InstallFailed { .. }
            | ProvisionError::InstallRefused { .. }
            | ProvisionError::FirstKeyFailed { .. }
            | ProvisionError::Cleanup(_)
            | ProvisionError::CleanupFailed { .. } => RpcErrorCode::Internal,
        }
    }

    fn evidence(&self) -> Option<Value> {
        let Self::CleanupAfter { cleanup, .. } = self else {
            return None;
        };
        let mut lines = vec![cleanup.to_string()];
        lines.extend(error_chain::causes(cleanup.as_ref()));
        Some(serde_json::json!({ "cleanup": lines }))
    }

    fn hints(&self) -> Vec<Hint> {
        if let Self::CleanupAfter { remove, .. } = self {
            return vec![Hint::Next(remove.to_string())];
        }
        Vec::new()
    }
}

/// Why `--storage none` is not recommended; said once when a Server starts without ZFS.
const DOCKER_ONLY: &str = "Docker only (not recommended): Volumes on this Server get no size limits, and it won't get backups, Server moves or zero-downtime migrations as they arrive.";

pub(crate) fn announce_storage(storage: &StoragePreparation) {
    if storage == &StoragePreparation::None {
        crate::ui::warn(DOCKER_ONLY);
    }
}

/// What enrollment asks its installer to do on this host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Installation {
    /// Prepare the host and install the daemon, with `storage`.
    Host(StoragePreparation),
    /// Replace only the daemon binary with this CLI's version.
    Daemon,
}

enum Preparation<'user> {
    Host {
        storage: &'user StoragePreparation,
        group_user: Option<&'user str>,
    },
    SoftwareOnly,
}

fn install_arguments(version: &str, preparation: Preparation<'_>) -> Vec<OsString> {
    let mut arguments = vec!["install".into(), "--version".into(), version.into()];
    match preparation {
        Preparation::Host {
            storage,
            group_user,
        } => {
            arguments.extend(["--storage".into(), storage.to_string().into()]);
            if let Some(user) = group_user {
                arguments.extend(["--group-user".into(), user.into()]);
            }
        }
        Preparation::SoftwareOnly => arguments.push("--software-only".into()),
    }
    arguments
}

struct Remote {
    destination: SshDestination,
    key: PathBuf,
    timeout: std::time::Duration,
    version_timeout: std::time::Duration,
    control_path: Option<PathBuf>,
    ssh: PathBuf,
    scp: PathBuf,
}

/// What the remote preflight learned before any bootstrap is acquired.
struct RemoteHost {
    user: String,
    architecture: String,
}

/// Where `provision` installs and how it reaches it, as the command line gave them.
pub(crate) struct RemoteTarget {
    destination: Option<String>,
    ssh_key: Option<String>,
    timeout: std::time::Duration,
}

impl RemoteTarget {
    pub(crate) fn new(
        destination: Option<String>,
        ssh_key: Option<String>,
        timeout: std::time::Duration,
    ) -> Self {
        Self {
            destination,
            ssh_key,
            timeout,
        }
    }
}

impl Remote {
    fn from_target(target: &RemoteTarget) -> Result<Self, ProvisionError> {
        let value = target
            .destination
            .as_ref()
            .ok_or(ProvisionError::MissingDestination)?;
        let connection = value.parse::<Connection>()?;
        let Transport::Ssh { destination, .. } = connection.transport() else {
            return Err(ProvisionError::RemoteTransport(connection.shown()));
        };
        Ok(Self {
            destination: destination.clone(),
            key: ssh_key(target.ssh_key.as_deref().expect("ssh-key has a default")),
            timeout: target.timeout,
            version_timeout: bootstrap::VERSION_TIMEOUT,
            control_path: crate::connect::control_path(),
            ssh: "ssh".into(),
            scp: "scp".into(),
        })
    }

    fn ssh(&self) -> Command {
        let mut command = Command::new(&self.ssh);
        command.args(self.common_arguments());
        if let Some(port) = self.destination.port() {
            command.args([OsString::from("-p"), port.to_string().into()]);
        }
        // Provisioning may wait for human authentication; only network setup is timed.
        command.args(["-tt", self.destination.target()]);
        command.stdin(Stdio::inherit());
        // Remote installer chatter is progress, not the result: stdout stays for `--json`.
        #[expect(
            clippy::disallowed_methods,
            reason = "ssh writes the installer's chatter itself"
        )]
        command.stdout(crate::ui::child_stderr());
        command
    }

    fn scp(&self) -> Command {
        let mut command = Command::new(&self.scp);
        command.args(self.common_arguments());
        if let Some(port) = self.destination.port() {
            command.args([OsString::from("-P"), port.to_string().into()]);
        }
        command
    }

    fn common_arguments(&self) -> Vec<OsString> {
        let mut arguments: Vec<OsString> = crate::connect::ssh_control_args(
            self.control_path.as_deref(),
            crate::connect::SshMaster::Share,
        )
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
        command.arg("uname -s; uname -m").stdout(Stdio::piped());
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

    async fn stage(&self, bootstrap: &Bootstrap, directory: &str) -> Result<(), ProvisionError> {
        let mut create = self.ssh();
        create.arg(format!("mkdir -m 700 -- {}", shell::quote(directory)));
        transfer_status(tokio::process::Command::from(create).status().await)?;

        let mut transfer = self.scp();
        transfer.arg(bootstrap.daemon());
        transfer.arg(format!("{}:{}/", self.destination.copy_target(), directory));
        transfer_status(tokio::process::Command::from(transfer).status().await)
    }

    async fn verify(&self, daemon: &str) -> Result<(), ProvisionError> {
        let mut command = self.ssh();
        command.arg(format!(
            "chmod 700 {path} && {path} version",
            path = shell::quote(daemon)
        ));
        bootstrap::verify_command_version(
            "preflight remote bootstrap daemon",
            command,
            self.version_timeout,
        )
        .await
    }

    async fn install(
        &self,
        daemon: &str,
        arguments: &[OsString],
        via_sudo: bool,
    ) -> Result<(), ProvisionError> {
        let mut command = self.ssh();
        command.arg(remote_command(
            daemon,
            arguments,
            via_sudo,
            &ployz_core::release_mirror_overrides(),
        ));
        installer_status(tokio::process::Command::from(command).status().await)
    }

    async fn cleanup(&self, directory: &str) -> Result<(), ProvisionError> {
        let mut cleanup = self.ssh();
        cleanup.arg(format!("rm -rf -- {}", shell::quote(directory)));
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
        let mut command = Command::new(&self.ssh);
        command.args(crate::connect::ssh_control_args(
            Some(control_path),
            crate::connect::SshMaster::Share,
        ));
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

    /// Confirm the remote user can install and report its architecture.
    async fn preflight(&self) -> Result<RemoteHost, ProvisionError> {
        let mut whoami = self.ssh();
        whoami.arg("whoami").stdout(Stdio::piped());
        let output = tokio::process::Command::from(whoami)
            .output()
            .await
            .map_err(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    ProvisionError::SshClientMissing
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
        let user = user.trim().to_owned();
        if user.is_empty() {
            return Err(ProvisionError::EmptyUser);
        }

        if user != "root" {
            let mut sudo = self.ssh();
            sudo.arg("sudo true");
            let status = tokio::process::Command::from(sudo)
                .status()
                .await
                .map_err(ProvisionError::Sudo)?;
            if !status.success() {
                return Err(ProvisionError::SudoRequired { user });
            }
        }

        let architecture = self.platform().await?;
        Ok(RemoteHost { user, architecture })
    }

    /// Stage, verify, and run `bootstrap` on the host, then remove the stage.
    async fn install_bootstrap(
        &self,
        host: &RemoteHost,
        bootstrap: &Bootstrap,
        version: &str,
        storage: &StoragePreparation,
    ) -> Result<(), ProvisionError> {
        let via_sudo = host.user != "root";
        let remote_directory = format!("/tmp/ployz-bootstrap-{}", Uuid::new_v4());
        let remote_daemon = format!("{remote_directory}/ployzd");
        let arguments = install_arguments(
            version,
            Preparation::Host {
                storage,
                group_user: via_sudo.then_some(host.user.as_str()),
            },
        );
        let primary = async {
            self.stage(bootstrap, &remote_directory).await?;
            self.verify(&remote_daemon).await?;
            self.install(&remote_daemon, &arguments, via_sudo).await
        }
        .await;
        let cleanup = self.cleanup(&remote_directory).await;
        // Host preparation may add the SSH user to the ployz group. A multiplexed
        // session authenticated before installation retains its old group list.
        self.close_control_master().await;
        let remove = remove_command(&self.destination, &self.key, &remote_directory);
        finish_remote(primary, cleanup, remove)
    }
}

fn remove_command(destination: &SshDestination, key: &Path, directory: &str) -> String {
    let port = destination
        .port()
        .map(|port| format!("-p {port} "))
        .unwrap_or_default();
    format!(
        "ssh {port}-i {} {} rm -rf -- {}",
        shell::quote(&key.to_string_lossy()),
        destination.target(),
        shell::quote(directory)
    )
}

fn ssh_key(key: &str) -> PathBuf {
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

/// The remote installer downloads the release again, so it gets this CLI's mirror
/// overrides: neither ssh nor sudo carries them.
fn remote_command(
    program: &str,
    arguments: &[OsString],
    via_sudo: bool,
    environment: &[(&str, String)],
) -> String {
    let mut command = String::new();
    if via_sudo {
        command.push_str("sudo ");
    }
    if !environment.is_empty() {
        command.push_str("env ");
        for (name, value) in environment {
            command.push_str(&shell::quote(&format!("{name}={value}")));
            command.push(' ');
        }
    }
    command.push_str(&shell::quote(program));
    for argument in arguments {
        command.push(' ');
        command.push_str(&shell::quote(&argument.to_string_lossy()));
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
    remove: String,
) -> Result<(), ProvisionError> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(primary), Err(cleanup)) => Err(ProvisionError::CleanupAfter {
            causes: error_chain::JoinedChain::of(error_chain::causes(&primary)),
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
            remove,
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
pub(crate) async fn provision_target(
    target: &RemoteTarget,
    version: &str,
    storage: &StoragePreparation,
) -> Result<(), ProvisionError> {
    let remote = Remote::from_target(target)?;
    let host = remote.preflight().await?;
    let bootstrap = Bootstrap::acquire(&host.architecture).await?;
    remote
        .install_bootstrap(&host, &bootstrap, version, storage)
        .await
}

/// Arm `line` as the First Key of the remote Server `target` names, unless its disk already
/// holds one, so a Cluster it founds through Ployz Cloud trusts this computer's key.
///
/// # Errors
///
/// Returns a [`ProvisionError`] when the SSH target or privilege preflight is invalid, or the
/// Server's `ployzd` can't arm it.
pub(crate) async fn arm_first_key(
    target: &RemoteTarget,
    line: &ployz_core::first_key::LineKey,
) -> Result<(), ProvisionError> {
    let remote = Remote::from_target(target)?;
    let host = remote.preflight().await?;
    let sudo = if host.user == "root" { "" } else { "sudo " };
    let mut command = remote.ssh();
    command.arg(format!(
        "{sudo}/usr/local/bin/ployzd arm-first-key {}",
        shell::quote(&line.to_string())
    ));
    let status = tokio::process::Command::from(command)
        .status()
        .await
        .map_err(ProvisionError::Install)?;
    if status.success() {
        Ok(())
    } else {
        Err(ProvisionError::FirstKeyFailed { status })
    }
}

/// Install and start local `ployzd` through a verified temporary daemon.
///
/// # Errors
///
/// Returns [`ProvisionError::NotRoot`] without local root privileges. It also
/// reports unsupported platforms, release acquisition and filesystem failures,
/// checksum or executable-version verification failures, and installer spawn or
/// non-zero exit failures.
pub async fn provision_local(
    version: &str,
    storage: &StoragePreparation,
) -> Result<(), ProvisionError> {
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
    install_local(&bootstrap, version, preparation).await
}

async fn install_local(
    bootstrap: &Bootstrap,
    version: &str,
    preparation: Preparation<'_>,
) -> Result<(), ProvisionError> {
    bootstrap.verify_local_version().await?;
    run_installer(bootstrap.daemon(), version, preparation).await
}

async fn run_installer(
    daemon: &Path,
    version: &str,
    preparation: Preparation<'_>,
) -> Result<(), ProvisionError> {
    use tokio::io::AsyncBufReadExt;

    #[expect(
        clippy::disallowed_methods,
        reason = "the installer writes its progress itself"
    )]
    let progress = crate::ui::child_stderr();
    let mut child = tokio::process::Command::new(daemon)
        .args(install_arguments(version, preparation))
        .stdout(progress)
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ProvisionError::Install)?;
    let mut last = None;
    if let Some(stderr) = child.stderr.take() {
        let mut reader = tokio::io::BufReader::new(stderr);
        let mut bytes = Vec::new();
        while reader
            .read_until(b'\n', &mut bytes)
            .await
            .map_err(ProvisionError::Install)?
            != 0
        {
            let line = String::from_utf8_lossy(&bytes);
            crate::ui::note_inline(&line);
            if !line.trim().is_empty() {
                last = Some(line.trim_end().to_owned());
            }
            bytes.clear();
        }
    }
    match (installer_status(child.wait().await), last) {
        (Err(ProvisionError::InstallFailed { .. }), Some(reason)) => {
            Err(ProvisionError::InstallRefused { reason })
        }
        (result, _) => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn root_is_what_id_reports() {
        let output = Command::new("id").arg("-u").output().unwrap();
        let reported = String::from_utf8(output.stdout).unwrap();
        assert_eq!(process_is_root(), reported.trim() == "0");
    }

    #[test]
    fn a_remote_install_carries_the_release_mirror_overrides() {
        let arguments = [
            OsString::from("install"),
            OsString::from("--version"),
            OsString::from("0.3.4"),
        ];
        assert_eq!(
            remote_command("/tmp/b/ployzd", &arguments, true, &[]),
            "sudo '/tmp/b/ployzd' 'install' '--version' '0.3.4'"
        );
        assert_eq!(
            remote_command(
                "/tmp/b/ployzd",
                &arguments,
                true,
                &[("PLOYZ_GITHUB_URL", "https://staging.test/it's".to_owned())]
            ),
            "sudo env 'PLOYZ_GITHUB_URL=https://staging.test/it'\"'\"'s' '/tmp/b/ployzd' 'install' '--version' '0.3.4'"
        );
        assert_eq!(
            remote_command(
                "/tmp/b/ployzd",
                &arguments,
                false,
                &[("PLOYZ_FALLBACK_URL", "https://fallback.test".to_owned())]
            ),
            "env 'PLOYZ_FALLBACK_URL=https://fallback.test' '/tmp/b/ployzd' 'install' '--version' '0.3.4'"
        );
    }

    #[tokio::test]
    async fn installer_drains_non_utf8_stderr_and_keeps_the_last_nonempty_reason() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = dir.path().join("installer");
        std::fs::write(&daemon, b"#!/bin/sh\nprintf '\\377\\n' >&2\nhead -c 131072 /dev/zero >&2\nprintf '\\nStorage refused by this host\\n\\n' >&2\nexit 7\n").unwrap();
        std::fs::set_permissions(&daemon, std::fs::Permissions::from_mode(0o755)).unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_installer(&daemon, "1.2.3", Preparation::SoftwareOnly),
        )
        .await
        .unwrap();
        assert!(
            matches!(result, Err(ProvisionError::InstallRefused { reason }) if reason == "Storage refused by this host")
        );
    }

    #[test]
    fn provisioning_ssh_allows_interactive_authentication_with_shared_options() {
        for (extra, seconds) in [(vec![], "20"), (vec!["--ssh-timeout", "17"], "17")] {
            let mut args = vec!["ployz", "server", "add", "root@host"];
            args.extend(extra);
            let root = crate::cli::command().try_get_matches_from(args).unwrap();
            let matches = root
                .subcommand_matches("server")
                .unwrap()
                .subcommand_matches("add")
                .unwrap();
            let remote =
                Remote::from_target(&crate::enrollment::plan::remote_target(matches)).unwrap();
            let ssh = remote.ssh();
            let scp = remote.scp();
            for command in [&ssh, &scp] {
                let args: Vec<_> = command.get_args().collect();
                assert!(args.contains(&std::ffi::OsStr::new("BatchMode=no")));
                for arg in crate::connect::ssh_control_args(
                    crate::connect::control_path().as_deref(),
                    crate::connect::SshMaster::Share,
                ) {
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
                    storage: &StoragePreparation::Zfs { device: None },
                    group_user: Some("deploy"),
                },
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
            install_arguments("1.2.3", Preparation::SoftwareOnly),
            ["install", "--version", "1.2.3", "--software-only"]
                .map(OsString::from)
                .to_vec()
        );
    }

    #[test]
    fn the_cleanup_command_uses_the_key_the_session_used() {
        let destination = SshDestination::parse("root@host").unwrap();
        assert_eq!(
            remove_command(
                &destination,
                Path::new("/home/me/my keys/id"),
                "/tmp/ployz-bootstrap-1"
            ),
            "ssh -i '/home/me/my keys/id' root@host rm -rf -- '/tmp/ployz-bootstrap-1'"
        );
    }

    #[test]
    fn a_failed_setup_heads_the_error_and_the_cleanup_names_what_to_remove() {
        let remove = "ssh root@host rm -rf -- '/tmp/ployz-bootstrap-1'";
        let cases = [
            (
                ProvisionError::Install(io::ErrorKind::BrokenPipe.into()),
                "Could not run the Ployz installer; the remote bootstrap was not cleaned up.",
                vec!["broken pipe"],
            ),
            (
                ProvisionError::BootstrapIo {
                    stage: "copy the bootstrap release",
                    source: io::ErrorKind::StorageFull.into(),
                },
                "Could not copy the bootstrap release; the remote bootstrap was not cleaned up.",
                vec!["no storage space"],
            ),
            (
                ProvisionError::BootstrapVerification("bad version".into()),
                "bootstrap verification: bad version; the remote bootstrap was not cleaned up.",
                vec![],
            ),
        ];
        for (primary, headline, causes) in cases {
            let cleanup = ProvisionError::Cleanup(io::Error::other("ssh failed"));
            let failure = crate::failure::Failure::from(
                finish_remote(Err(primary), Err(cleanup), remove.into()).unwrap_err(),
            );
            assert_eq!(failure.to_string(), headline);
            assert_eq!(failure.causes(), causes, "{headline}");
            assert_eq!(failure.hints(), [Hint::Next(remove.into())], "{headline}");
            assert_eq!(
                failure.json().pointer("/details/cleanup"),
                Some(&serde_json::json!([
                    "Could not remove the remote bootstrap.",
                    "ssh failed"
                ]))
            );
        }
    }

    #[tokio::test]
    async fn local_provision_requires_root() {
        if process_is_root() {
            return;
        }
        assert!(matches!(
            provision_local(env!("CARGO_PKG_VERSION"), &StoragePreparation::None).await,
            Err(ProvisionError::NotRoot)
        ));
    }
}
