//! Bounded local installation of a Ployz Machine release.

mod host;
mod release;
mod storage;
pub mod upgrade;

use std::{
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use ployz_core::{MachineUpgradeStage, StorageChoice};
use thiserror::Error;

use crate::mutation;

use self::{
    host::{
        create_user_and_directories, install_docker, install_prerequisites, install_systemd,
        verify_running_daemon, verify_software_prerequisites,
    },
    release::{install_binaries, resolve_release},
    storage::prepare_storage,
};

pub use self::release::{ReleaseRequest, ReleaseSource};

const PLOYZ_USER: &str = "ployz";
const DEFAULT_BIN_DIR: &str = "/usr/local/bin";
/// Machine-owned helper, independent of the separately installed CLI helper.
pub(crate) const TAILCAT_HELPER_PROGRAM: &str = "/usr/local/bin/ployzd-tailcat";
const DEFAULT_SYSTEMD_DIR: &str = "/etc/systemd/system";
const DEFAULT_RUN_DIR: &str = "/run/ployz";
/// Unix socket installed systemd services use for the local Machine API.
pub const DEFAULT_SOCKET_PATH: &str = "/run/ployz/ployz.sock";
/// Mutually exclusive host work for one Machine installation attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallMode {
    /// Write the release and units without starting systemd or preparing the host.
    InstallationOnly,
    /// Replace the daemon only after confirming the Machine is already prepared.
    SoftwareOnly,
    /// Prepare the host and optionally configure its requested storage.
    PrepareHost {
        /// Explicit storage preparation for a fresh Machine.
        storage: StorageChoice,
        /// Existing operator to add to the Ployz service group during host preparation.
        group_user: Option<String>,
    },
}

/// Choices for one local Machine installation attempt.
#[derive(Clone, Debug)]
pub struct InstallRequest {
    /// A fixed version or a channel resolved once before host mutation.
    pub release: ReleaseRequest,
    /// Trusted release source. Published releases never accept caller-provided URLs.
    pub source: ReleaseSource,
    /// Installation-only, software-only replacement, or full host preparation.
    pub mode: InstallMode,
}

/// What installation directly observed after activation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Readiness {
    /// Files and unit definitions were installed, but systemd was deliberately not contacted.
    InstallationOnly,
    /// systemd reported the daemon active and its process is the verified target executable.
    Running,
}

/// The bounded outcome of one local installation attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallOutcome {
    /// Exact release selected before host mutation.
    pub target: String,
    /// Readiness observed before the command returned.
    pub readiness: Readiness,
}

/// Installation failures name the first stage that did not complete.
#[derive(Debug, Error)]
pub enum Error {
    #[error("run this command with sudo")]
    NotRoot,
    #[error("another Ployz installation is active")]
    Busy,
    #[error("nightly is not a supported release channel")]
    Nightly,
    #[error("invalid Ployz release version '{value}'; expected X.Y.Z or X.Y.Z-beta.N")]
    InvalidVersion { value: String },
    #[error("Ployz Machine must be Linux")]
    UnsupportedOs,
    #[error("unsupported architecture: {0}")]
    UnsupportedArchitecture(String),
    #[error("Ployz requires systemd")]
    SystemdRequired,
    #[error("{0}")]
    NonstandardPaths(String),
    #[error("release selection: {0}")]
    ReleaseSelection(String),
    #[error("artifact verification: {0}")]
    Verification(String),
    #[error("{stage}: {message}")]
    Command { stage: String, message: String },
    #[error("{stage}: {source}")]
    Io {
        stage: &'static str,
        #[source]
        source: io::Error,
    },
}

#[derive(Clone, Debug)]
pub(super) struct InstallPaths {
    pub(super) bin_dir: PathBuf,
    pub(super) systemd_dir: PathBuf,
    pub(super) data_dir: PathBuf,
    pub(super) run_dir: PathBuf,
    pub(super) docker_config: PathBuf,
    pub(super) modprobe_dir: PathBuf,
}

impl InstallPaths {
    fn system(data_dir: impl Into<PathBuf>, run_dir: impl Into<PathBuf>) -> Self {
        Self {
            bin_dir: PathBuf::from(DEFAULT_BIN_DIR),
            systemd_dir: PathBuf::from(DEFAULT_SYSTEMD_DIR),
            data_dir: data_dir.into(),
            run_dir: run_dir.into(),
            docker_config: PathBuf::from("/etc/docker/daemon.json"),
            modprobe_dir: PathBuf::from("/etc/modprobe.d"),
        }
    }

    #[cfg(test)]
    fn at(root: &Path) -> Self {
        Self {
            bin_dir: root.join("bin"),
            systemd_dir: root.join("systemd"),
            data_dir: root.join("data"),
            run_dir: root.join("run"),
            docker_config: root.join("docker/daemon.json"),
            modprobe_dir: root.join("modprobe"),
        }
    }
}

pub(crate) fn require_standard_machine_paths(data_dir: &Path, socket: &Path) -> Result<(), String> {
    if data_dir == Path::new(crate::machine::DEFAULT_DATA_DIR)
        && socket == Path::new(DEFAULT_SOCKET_PATH)
    {
        return Ok(());
    }
    Err(format!(
        "system installation requires --data-dir {} and --socket {}; received --data-dir {} and --socket {}",
        crate::machine::DEFAULT_DATA_DIR,
        DEFAULT_SOCKET_PATH,
        data_dir.display(),
        socket.display()
    ))
}

/// Install into global host locations for the standard Machine data and socket paths.
///
/// # Errors
///
/// Returns the observed first failed installation stage.
pub async fn install(
    request: InstallRequest,
    data_dir: impl Into<PathBuf>,
    socket: impl Into<PathBuf>,
) -> Result<InstallOutcome, Error> {
    let data_dir = data_dir.into();
    let socket = socket.into();
    require_standard_machine_paths(&data_dir, &socket).map_err(Error::NonstandardPaths)?;
    install_at(request, InstallPaths::system(data_dir, DEFAULT_RUN_DIR)).await
}

async fn install_at(request: InstallRequest, paths: InstallPaths) -> Result<InstallOutcome, Error> {
    require_root()?;
    let admission = mutation::MutationGate::new(&paths.run_dir, &paths.data_dir);
    let lock = admission.try_installation().map_err(map_admission_error)?;
    upgrade::reconcile_for_install(&admission, &paths.data_dir)
        .await
        .map_err(map_upgrade_reconciliation)?;
    install_locked(request, paths, lock, |_| Ok(())).await
}

async fn install_locked(
    request: InstallRequest,
    paths: InstallPaths,
    _lock: mutation::InstallationGuard,
    mut progress: impl FnMut(MachineUpgradeStage) -> Result<(), Error>,
) -> Result<InstallOutcome, Error> {
    let installation_only = matches!(request.mode, InstallMode::InstallationOnly);
    verify_system(installation_only)?;
    let target = resolve_release(&request.release, &request.source).await?;

    progress(MachineUpgradeStage::Preparing)?;
    match &request.mode {
        InstallMode::InstallationOnly => {}
        InstallMode::SoftwareOnly => verify_software_prerequisites(&paths)?,
        InstallMode::PrepareHost {
            storage,
            group_user,
        } => {
            prepare_storage(*storage, &paths)?;
            install_prerequisites()?;
            let inherited_group = sudo_user();
            create_user_and_directories(
                group_user.as_deref().or(inherited_group.as_deref()),
                &paths,
            )?;
        }
    }

    let mut restart_required = !paths.systemd_dir.join("ployz.service").is_file()
        || !paths.systemd_dir.join("ployz-tailcat.service").is_file();
    restart_required |= install_binaries(&request.source, &paths, &target, &mut progress).await?;
    install_systemd(&paths, installation_only)?;
    if matches!(request.mode, InstallMode::PrepareHost { .. }) {
        install_docker(&paths).await?;
    }

    if !installation_only && !restart_required {
        // A prior attempt may have activated these files without starting them.
        restart_required = verify_running_daemon(&paths, &target).await.is_err();
    }
    let readiness = if installation_only {
        Readiness::InstallationOnly
    } else {
        if restart_required {
            progress(MachineUpgradeStage::Restarting)?;
            systemctl("restart daemon", ["restart", "ployz.service"])?;
            systemctl(
                "restart volume plugin",
                ["try-restart", "ployz-volume-plugin.service"],
            )?;
        }
        verify_running_daemon(&paths, &target).await?;
        host::create_tailcat_state_directory(&paths)?;
        // Reapply the installed unit and executable even when a previous attempt
        // activated files but failed before restarting this service.
        systemctl(
            "restart Tailcat endpoint",
            ["restart", "ployz-tailcat.service"],
        )?;
        progress(MachineUpgradeStage::Readiness)?;
        host::verify_running_tailcat(&paths, &target).await?;
        verify_running_daemon(&paths, &target).await?;
        Readiness::Running
    };
    Ok(InstallOutcome {
        target: target.to_string(),
        readiness,
    })
}

fn map_admission_error(error: mutation::Error) -> Error {
    match error {
        mutation::Error::Busy => Error::Busy,
        mutation::Error::Io(source) => Error::Io {
            stage: "claim Machine installation admission",
            source,
        },
    }
}

fn map_upgrade_reconciliation(error: upgrade::Error) -> Error {
    if matches!(error, upgrade::Error::Busy) {
        Error::Busy
    } else {
        Error::Command {
            stage: "reconcile previous Machine upgrade".into(),
            message: error.to_string(),
        }
    }
}

fn require_root() -> Result<(), Error> {
    let mut command = Command::new("id");
    command.arg("-u");
    let output = run_command("check installation privilege", &mut command)?;
    if String::from_utf8_lossy(&output.stdout).trim() == "0" {
        Ok(())
    } else {
        Err(Error::NotRoot)
    }
}

fn verify_system(install_only: bool) -> Result<(), Error> {
    if std::env::consts::OS != "linux" {
        return Err(Error::UnsupportedOs);
    }
    daemon_archive()?;
    if !command_exists("tar") {
        return Err(Error::Command {
            stage: "preflight daemon archive extraction".into(),
            message: "tar is required to extract Ployz release archives".into(),
        });
    }
    if !install_only && !Path::new("/run/systemd/system").is_dir() {
        return Err(Error::SystemdRequired);
    }
    Ok(())
}

pub(super) fn daemon_archive() -> Result<&'static str, Error> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("ployzd_linux_amd64.tar.gz"),
        "aarch64" => Ok("ployzd_linux_arm64.tar.gz"),
        architecture => Err(Error::UnsupportedArchitecture(architecture.into())),
    }
}

fn sudo_user() -> Option<String> {
    std::env::var("SUDO_USER")
        .ok()
        .filter(|user| !user.is_empty())
}

pub(super) fn command_exists(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| {
            let candidate = directory.join(name);
            candidate.is_file()
                && candidate
                    .metadata()
                    .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
        })
    })
}

pub(super) fn run_command(
    stage: impl Into<String>,
    command: &mut Command,
) -> Result<Output, Error> {
    let stage = stage.into();
    let output = command.output().map_err(|source| Error::Io {
        stage: "start host command",
        source,
    })?;
    if output.status.success() {
        Ok(output)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(Error::Command {
            stage,
            message: if stderr.is_empty() {
                format!("exited with {}", output.status)
            } else {
                stderr
            },
        })
    }
}

pub(super) fn systemctl<const N: usize>(stage: &str, args: [&str; N]) -> Result<Output, Error> {
    let mut command = Command::new("systemctl");
    command.args(args);
    run_command(stage, &mut command)
}

pub(super) fn run_host<const N: usize>(
    stage: &str,
    program: &str,
    args: [&str; N],
) -> Result<Output, Error> {
    let mut command = Command::new(program);
    command.args(args);
    run_command(stage, &mut command)
}

pub(super) fn run_apt<const N: usize>(
    stage: &str,
    args: [&str; N],
    working_directory: Option<&Path>,
) -> Result<Output, Error> {
    let mut command = Command::new("apt-get");
    command
        .args(["-o", "DPkg::Lock::Timeout=300"])
        .args(args)
        .env("DEBIAN_FRONTEND", "noninteractive");
    if let Some(working_directory) = working_directory {
        command.current_dir(working_directory);
    }
    run_command(stage, &mut command)
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        ffi::OsString,
        fs,
        io::Write,
        os::unix::fs::{PermissionsExt, symlink},
        process::Command,
    };

    use ployz_core::MachineVersion;
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn system_install_rejects_nonstandard_machine_paths_before_mutation() {
        let fixture = fixture("nonstandard-paths");
        let request = InstallRequest {
            release: ReleaseRequest::Exact(MachineVersion::parse("1.2.3").unwrap()),
            source: ReleaseSource::Local(fixture.path().join("release")),
            mode: InstallMode::InstallationOnly,
        };

        for (data_dir, socket) in [
            (fixture.path(), Path::new(DEFAULT_SOCKET_PATH)),
            (Path::new(crate::machine::DEFAULT_DATA_DIR), fixture.path()),
        ] {
            let error = install(request.clone(), data_dir, socket)
                .await
                .unwrap_err();
            assert!(matches!(error, Error::NonstandardPaths(_)));
        }
        assert!(fs::read_dir(fixture.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn installation_interface_contract() {
        if let Ok(case) = env::var("PLOYZ_INSTALLER_CONTRACT_CASE") {
            run_installation_case(&case).await;
            let root = PathBuf::from(env::var_os("PLOYZ_INSTALLER_CONTRACT_ROOT").unwrap());
            fs::write(root.join("child-completed"), format!("installation:{case}")).unwrap();
            return;
        }

        for case in [
            "success",
            "install-only",
            "same-target",
            "missing",
            "missing-tar",
            "bad-checksum",
            "corrupt",
            "rejected-executable",
            "hung-executable",
            "bad-helper",
            "busy",
        ] {
            let fixture = fixture(case);
            create_installation_fixture(fixture.path(), case);
            run_contract_child("installation_interface_contract", fixture.path(), case);
        }
    }

    #[test]
    fn software_only_prerequisite_contract() {
        if env::var_os("PLOYZ_SOFTWARE_PREREQUISITE_CONTRACT").is_some() {
            let root = PathBuf::from(env::var_os("PLOYZ_INSTALLER_CONTRACT_ROOT").unwrap());
            let paths = InstallPaths::at(&root);
            assert!(matches!(
                verify_software_prerequisites(&paths),
                Err(Error::Command { stage, message })
                    if stage == "software-only preflight"
                        && message
                            == "Docker is not installed; run ployzd install without --software-only first"
            ));
            fs::write(root.join("child-completed"), "software-prerequisite").unwrap();
            return;
        }

        let fixture = fixture("software-prerequisite");
        fs::create_dir_all(fixture.path().join("commands")).unwrap();
        run_contract_child_with_environment(
            "software_only_prerequisite_contract",
            fixture.path(),
            OsString::from("PLOYZ_SOFTWARE_PREREQUISITE_CONTRACT"),
            OsString::from("1"),
            "software-prerequisite",
            [],
        );
    }

    #[test]
    fn zfs_candidate_download_contract() {
        if env::var_os("PLOYZ_ZFS_CANDIDATE_CONTRACT").is_some() {
            super::storage::install_zfs_packages("test-kernel").unwrap();
            let root = PathBuf::from(env::var_os("PLOYZ_INSTALLER_CONTRACT_ROOT").unwrap());
            fs::write(root.join("child-completed"), "zfs-candidate").unwrap();
            return;
        }

        let fixture = fixture("zfs-candidate");
        let commands = fixture.path().join("commands");
        fs::create_dir_all(&commands).unwrap();
        let marker = fixture.path().join("installed");
        write_script(
            &commands.join("apt-cache"),
            "[ \"$2\" = linux-main-modules-zfs-test-kernel ]",
        );
        write_script(
            &commands.join("dpkg-query"),
            "if [ -f \"$PLOYZ_ZFS_INSTALLED\" ]; then echo /lib/modules/test-kernel/kernel/zfs.ko; exit 0; fi\nexit 1",
        );
        write_script(
            &commands.join("apt-get"),
            "case \"$*\" in\n  *download*) : > module.deb ;;\n  *install*) : > \"$PLOYZ_ZFS_INSTALLED\" ;;\nesac",
        );
        write_script(
            &commands.join("dpkg-deb"),
            "echo /lib/modules/test-kernel/kernel/zfs.ko",
        );
        run_contract_child_with_environment(
            "zfs_candidate_download_contract",
            fixture.path(),
            OsString::from("PLOYZ_ZFS_CANDIDATE_CONTRACT"),
            OsString::from("1"),
            "zfs-candidate",
            [(
                OsString::from("PLOYZ_ZFS_INSTALLED"),
                marker.into_os_string(),
            )],
        );
        assert!(fixture.path().join("installed").is_file());
    }

    async fn run_installation_case(case: &str) {
        let root = PathBuf::from(env::var_os("PLOYZ_INSTALLER_CONTRACT_ROOT").unwrap());
        let paths = InstallPaths::at(&root);
        fs::create_dir_all(&paths.bin_dir).unwrap();
        fs::write(paths.bin_dir.join("ployz-tailcat"), "retained CLI helper").unwrap();
        let existing = match case {
            "success" | "install-only" => None,
            "same-target" => Some(write_existing_daemon(&paths, "1.2.3")),
            _ => Some(write_existing_daemon(&paths, "1.2.2")),
        };
        if case == "install-only" {
            fs::remove_file(root.join("commands/dockerd")).unwrap();
        }
        if case == "missing-tar" {
            fs::remove_file(root.join("commands/tar")).unwrap();
        }

        let request = InstallRequest {
            release: ReleaseRequest::Exact(MachineVersion::parse("1.2.3").unwrap()),
            source: ReleaseSource::Local(root.join("release")),
            mode: InstallMode::InstallationOnly,
        };
        let result = if case == "busy" {
            let admission = mutation::MutationGate::new(&paths.run_dir, &paths.data_dir);
            let _held = admission.try_installation().unwrap();
            install_at(request, paths.clone()).await
        } else {
            install_at(request, paths.clone()).await
        };

        match case {
            "success" | "install-only" => {
                let outcome = result.unwrap();
                assert_eq!(outcome.target, "1.2.3");
                assert_eq!(outcome.readiness, Readiness::InstallationOnly);
                assert!(paths.bin_dir.join("ployzd").is_file());
                assert!(paths.systemd_dir.join("ployz.service").is_file());
                let helper_name = Path::new(TAILCAT_HELPER_PROGRAM).file_name().unwrap();
                assert_eq!(
                    Path::new(TAILCAT_HELPER_PROGRAM).parent(),
                    Some(Path::new(DEFAULT_BIN_DIR))
                );
                assert!(paths.bin_dir.join(helper_name).is_file());
                let unit =
                    fs::read_to_string(paths.systemd_dir.join("ployz-tailcat.service")).unwrap();
                assert!(unit.contains("Type=notify"));
                assert!(unit.contains(&format!(
                    "ConditionPathExists={}/machine.json",
                    paths.data_dir.display()
                )));
                assert!(unit.contains("User=ployz"));
                assert!(unit.contains("CapabilityBoundingSet=\n"));
                assert!(
                    unit.contains("RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK\n")
                );
                assert!(!unit.contains("Requires=ployz.service"));
                assert!(unit.contains(&format!(
                    "ExecStart={} serve --state ",
                    paths.bin_dir.join(helper_name).display()
                )));
                assert!(unit.contains(&format!(
                    "ReadWritePaths={}/tailcat\n",
                    paths.data_dir.display()
                )));
                if case == "install-only" {
                    assert!(!root.join("forbidden-invocation").exists());
                    assert!(!paths.data_dir.exists());
                }
            }
            "same-target" => {
                let outcome = result.unwrap();
                assert_eq!(outcome.target, "1.2.3");
                let existing = existing.as_ref().unwrap();
                assert_ne!(fs::read(paths.bin_dir.join("ployzd")).unwrap(), *existing);
                assert_eq!(
                    fs::read(paths.bin_dir.join("ployzd.previous")).unwrap(),
                    *existing
                );
            }
            "busy" => assert!(matches!(result, Err(Error::Busy))),
            "missing-tar" => assert!(matches!(
                result,
                Err(Error::Command { stage, message })
                    if stage == "preflight daemon archive extraction"
                        && message == "tar is required to extract Ployz release archives"
            )),
            "hung-executable" => {
                assert!(matches!(
                    result,
                    Err(Error::Command { stage, message })
                        if stage == "preflight staged daemon" && message == "timed out after 100ms"
                ));
            }
            "bad-checksum" => assert!(matches!(
                result,
                Err(Error::Verification(message))
                    if message
                        == format!("checksums.txt has no hash for {}", daemon_archive().unwrap())
            )),
            "corrupt" => assert!(matches!(
                result,
                Err(Error::Verification(message))
                    if message.contains(daemon_archive().unwrap())
                        && message.contains("checksum was")
                        && message.contains("expected")
            )),
            "missing" | "rejected-executable" | "bad-helper" => {
                assert!(result.is_err(), "{case} artifact was accepted");
            }
            other => panic!("unknown contract case {other}"),
        }
        assert_eq!(
            fs::read_to_string(paths.bin_dir.join("ployz-tailcat")).unwrap(),
            "retained CLI helper"
        );
        if let Some(existing) = existing.filter(|_| case != "same-target") {
            assert_eq!(fs::read(paths.bin_dir.join("ployzd")).unwrap(), existing);
        }
    }

    fn create_installation_fixture(root: &Path, case: &str) {
        let commands = root.join("commands");
        let release = root.join("release");
        let payload = root.join("payload");
        fs::create_dir_all(&commands).unwrap();
        fs::create_dir_all(&release).unwrap();
        fs::create_dir_all(&payload).unwrap();
        write_script(&commands.join("id"), "if [ \"$1\" = -u ]; then echo 0; fi");
        write_script(&commands.join("dockerd"), "exit 0");
        symlink("/usr/bin/tar", commands.join("tar")).unwrap();
        symlink("/usr/bin/gzip", commands.join("gzip")).unwrap();
        symlink("/usr/bin/sleep", commands.join("sleep")).unwrap();

        let daemon = match case {
            "rejected-executable" => "case \"$1\" in version) echo 9.9.9 ;; esac",
            "hung-executable" => "case \"$1\" in version) sleep 1; echo 1.2.3 ;; esac",
            _ => "case \"$1\" in version) echo 1.2.3 ;; esac",
        };
        write_script(&payload.join("ployzd"), daemon);
        write_script(&payload.join("ployz-uninstall"), "exit 0");
        write_script(
            &payload.join("ployz-tailcat"),
            if case == "bad-helper" {
                "echo 9.9.9"
            } else {
                "echo 1.2.3"
            },
        );
        if case == "install-only" {
            write_script(
                &commands.join("id"),
                "if [ \"$1\" = -u ]; then echo 0; else echo id >> \"$PLOYZ_INSTALLER_FORBIDDEN\"; exit 97; fi",
            );
            for command in [
                "apt-get",
                "dnf",
                "yum",
                "pacman",
                "zypper",
                "bash",
                "docker",
                "systemctl",
            ] {
                write_script(
                    &commands.join(command),
                    "echo \"$0\" >> \"$PLOYZ_INSTALLER_FORBIDDEN\"; exit 97",
                );
            }
        }
        if case != "missing" {
            let archive = release.join(daemon_archive().unwrap());
            let status = Command::new("tar")
                .args(["-czf"])
                .arg(&archive)
                .args(["-C"])
                .arg(&payload)
                .args(["ployzd", "ployz-uninstall", "ployz-tailcat"])
                .status()
                .unwrap();
            assert!(status.success());
            let checksum = String::from_utf8(
                Command::new("sha256sum")
                    .arg(&archive)
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_owned();
            fs::write(
                release.join("checksums.txt"),
                format!("{checksum}  {}\n", daemon_archive().unwrap()),
            )
            .unwrap();
            if case == "corrupt" {
                fs::OpenOptions::new()
                    .append(true)
                    .open(archive)
                    .unwrap()
                    .write_all(b"corrupt")
                    .unwrap();
            }
        } else {
            fs::write(
                release.join("checksums.txt"),
                format!("{}  {}\n", "0".repeat(64), daemon_archive().unwrap()),
            )
            .unwrap();
        }
        if case == "bad-checksum" {
            fs::write(release.join("checksums.txt"), "bad checksum\n").unwrap();
        }
    }

    fn write_existing_daemon(paths: &InstallPaths, version: &str) -> Vec<u8> {
        fs::create_dir_all(&paths.bin_dir).unwrap();
        let existing = format!("#!/bin/sh\n[ \"$1\" = version ] && echo {version}\n").into_bytes();
        fs::write(paths.bin_dir.join("ployzd"), &existing).unwrap();
        fs::set_permissions(
            paths.bin_dir.join("ployzd"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        existing
    }

    fn run_contract_child(test: &str, root: &Path, case: &str) {
        let key = OsString::from("PLOYZ_INSTALLER_CONTRACT_CASE");
        let value = OsString::from(case);
        let completion = format!("installation:{case}");
        if case == "install-only" {
            run_contract_child_with_environment(
                test,
                root,
                key,
                value,
                &completion,
                [(
                    OsString::from("PLOYZ_INSTALLER_FORBIDDEN"),
                    root.join("forbidden-invocation").into_os_string(),
                )],
            );
        } else {
            run_contract_child_with_environment(test, root, key, value, &completion, []);
        }
    }

    fn run_contract_child_with_environment<const N: usize>(
        test: &str,
        root: &Path,
        key: OsString,
        value: OsString,
        completion: &str,
        extra: [(OsString, OsString); N],
    ) {
        let test = format!("installer::tests::{test}");
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .args(["--exact", &test, "--nocapture"])
            .env("PLOYZ_INSTALLER_CONTRACT_ROOT", root)
            .env("PATH", root.join("commands"))
            .env(key, value);
        for (key, value) in extra {
            command.env(key, value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "contract child {test} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert_eq!(
            fs::read_to_string(root.join("child-completed")).unwrap(),
            completion,
            "contract child {test} did not complete its fixture",
        );
    }

    fn write_script(path: &Path, body: &str) {
        fs::write(path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn fixture(name: &str) -> TempDir {
        tempfile::Builder::new()
            .prefix(&format!("ployzd-installer-{name}-"))
            .tempdir()
            .unwrap()
    }
}
