//! Explicit Machine host preparation and systemd lifecycle management.

use std::{
    fs,
    io::Write,
    os::unix::fs::MetadataExt,
    path::Path,
    process::{Command, Stdio},
};

use tonic::transport::Endpoint;

use crate::filesystem::atomic_write;
use ployz_core::{DescribeContractRequest, MachineRpcClient, MachineVersion, op};

use super::release::{fetch, installed_release};
use super::{Error, InstallPaths, PLOYZ_USER, command_exists, run_apt, run_host, systemctl};

const DOCKER_DAEMON_CONFIG: &str = r#"{
  "features": { "containerd-snapshotter": true },
  "live-restore": true,
  "log-driver": "json-file",
  "log-opts": { "max-size": "10m", "max-file": "3" }
}"#;

pub(super) fn install_prerequisites() -> Result<(), Error> {
    if command_exists("curl") {
        return Ok(());
    }
    if command_exists("apt-get") {
        run_apt(
            "refresh packages for prerequisites",
            ["update", "-qq"],
            None,
        )?;
        run_apt(
            "install prerequisites",
            ["install", "-y", "-qq", "curl", "ca-certificates"],
            None,
        )?;
    } else if command_exists("dnf") {
        run_host(
            "install prerequisites",
            "dnf",
            ["install", "-y", "curl", "ca-certificates"],
        )?;
    } else if command_exists("yum") {
        run_host(
            "install prerequisites",
            "yum",
            ["install", "-y", "curl", "ca-certificates"],
        )?;
    } else if command_exists("pacman") {
        run_host(
            "install prerequisites",
            "pacman",
            ["-Sy", "--noconfirm", "curl", "ca-certificates"],
        )?;
    } else if command_exists("zypper") {
        run_host(
            "install prerequisites",
            "zypper",
            ["--non-interactive", "install", "curl", "ca-certificates"],
        )?;
    } else {
        return Err(Error::Command {
            stage: "install prerequisites".into(),
            message: "curl is required and no supported package manager was found".into(),
        });
    }
    Ok(())
}

pub(super) fn create_user_and_directories(
    group_user: Option<&str>,
    paths: &InstallPaths,
) -> Result<(), Error> {
    let mut exists = Command::new("id");
    exists
        .arg(PLOYZ_USER)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if !exists
        .status()
        .map_err(|source| Error::Io {
            stage: "inspect Ployz service account",
            source,
        })?
        .success()
    {
        run_host(
            "create Ployz service account",
            "useradd",
            [
                "--system",
                "--home-dir",
                "/nonexistent",
                "--shell",
                "/usr/sbin/nologin",
                "--user-group",
                PLOYZ_USER,
            ],
        )?;
    }
    if let Some(user) = group_user {
        run_host(
            "add operator to Ployz group",
            "gpasswd",
            ["--add", user, PLOYZ_USER],
        )?;
    }
    let data = paths.data_dir.to_string_lossy();
    let run = paths.run_dir.to_string_lossy();
    run_host(
        "create Ployz directories",
        "install",
        [
            "-d", "-m", "0750", "-o", PLOYZ_USER, "-g", PLOYZ_USER, &data, &run,
        ],
    )?;
    Ok(())
}

pub(super) fn create_tailcat_state_directory(paths: &InstallPaths) -> Result<(), Error> {
    // Called only after the daemon has claimed its data directory.
    let directory = paths.data_dir.join("tailcat");
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err(Error::Verification(
                "Tailcat state directory is not a directory".into(),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(Error::Io {
                stage: "inspect Tailcat state directory",
                source,
            });
        }
    }
    run_host(
        "create private Tailcat state directory",
        "install",
        [
            "-d",
            "-m",
            "0700",
            "-o",
            PLOYZ_USER,
            "-g",
            PLOYZ_USER,
            &directory.to_string_lossy(),
        ],
    )?;
    Ok(())
}

pub(super) fn verify_software_prerequisites(paths: &InstallPaths) -> Result<(), Error> {
    if !command_exists("dockerd") {
        return Err(Error::Command {
            stage: "software-only preflight".into(),
            message: "Docker is not installed; run ployzd install without --software-only first"
                .into(),
        });
    }
    let service_account = Command::new("id")
        .arg(PLOYZ_USER)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|source| Error::Io {
            stage: "software-only preflight",
            source,
        })?;
    if !service_account.success() {
        return Err(Error::Command {
            stage: "software-only preflight".into(),
            message: "the Ployz service account is missing; run ployzd install without --software-only first".into(),
        });
    }
    for path in [&paths.data_dir, &paths.run_dir] {
        if !path.is_dir() {
            return Err(Error::Command {
                stage: "software-only preflight".into(),
                message: format!(
                    "{} is missing; run ployzd install without --software-only first",
                    path.display()
                ),
            });
        }
    }
    Ok(())
}

pub(super) fn install_systemd(paths: &InstallPaths, install_only: bool) -> Result<(), Error> {
    fs::create_dir_all(&paths.systemd_dir).map_err(|source| Error::Io {
        stage: "create systemd unit directory",
        source,
    })?;
    let bin = paths.bin_dir.display();
    write_file_atomically(
        &paths.systemd_dir.join("ployz.service"),
        &format!(
            "[Unit]\nDescription=Ployz Machine daemon\nAfter=network-online.target docker.service\nWants=network-online.target\n\n[Service]\nType=notify\nExecStart={bin}/ployzd\n# Set PLOYZ_LOG=debug in /etc/default/ployz to raise verbosity.\nEnvironmentFile=-/etc/default/ployz\nTimeoutStartSec=20\nTimeoutStopSec=15\nRestart=always\nRestartPreventExitStatus=78\nRestartSec=2\nNoNewPrivileges=true\nProtectSystem=full\nProtectControlGroups=true\nProtectHome=read-only\nProtectKernelTunables=true\nPrivateTmp=true\nRestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK\nRestrictNamespaces=true\n\n[Install]\nWantedBy=multi-user.target\n"
        ),
        "write systemd unit",
    )?;
    write_file_atomically(
        &paths.systemd_dir.join("ployz-volume-plugin.socket"),
        "[Unit]\nDescription=Ployz Docker Volume plugin socket\nBefore=docker.service\n\n[Socket]\nListenStream=/run/docker/plugins/ployz.sock\nSocketMode=0660\nDirectoryMode=0755\nAccept=no\nService=ployz-volume-plugin.service\n\n[Install]\nWantedBy=sockets.target\n",
        "write systemd unit",
    )?;
    write_file_atomically(
        &paths.systemd_dir.join("ployz-volume-plugin.service"),
        &format!(
            "[Unit]\nDescription=Ployz Docker Volume plugin\nBefore=docker.service\nAfter=zfs-import.target zfs-mount.service ployz-volume-plugin.socket\nRequires=ployz-volume-plugin.socket docker.service\n\n[Service]\nType=simple\nExecStart={bin}/ployzd volume-plugin\nSockets=ployz-volume-plugin.socket\nEnvironmentFile=-/etc/default/ployz\nRestart=on-failure\nRestartSec=2\nNoNewPrivileges=true\nRestrictAddressFamilies=AF_UNIX\nRestrictNamespaces=true\n"
        ),
        "write systemd unit",
    )?;
    write_file_atomically(
        &paths.systemd_dir.join("ployz-tailcat.service"),
        &format!(
            "[Unit]\nDescription=Ployz Tailcat management endpoint\nConditionPathExists={}/machine.json\nAfter=network-online.target ployz.service\nWants=network-online.target\n\n[Service]\nType=notify\nNotifyAccess=main\nTimeoutStartSec=30\nUser=ployz\nGroup=ployz\nExecStart={bin}/ployzd-tailcat serve --state {}/tailcat/state.json\nRestart=on-failure\nRestartSec=2\nTimeoutStopSec=15\nUMask=0077\nNoNewPrivileges=true\nCapabilityBoundingSet=\nProtectSystem=strict\nReadWritePaths={}/tailcat\nProtectHome=true\nProtectControlGroups=true\nProtectKernelTunables=true\nPrivateTmp=true\nPrivateDevices=true\nRestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK\nRestrictNamespaces=true\n\n[Install]\nWantedBy=multi-user.target\n",
            paths.data_dir.display(),
            paths.data_dir.display(),
            paths.data_dir.display()
        ),
        "write Tailcat systemd unit",
    )?;
    if !install_only {
        systemctl("reload systemd units", ["daemon-reload"])?;
        systemctl("enable daemon", ["enable", "ployz.service"])?;
        systemctl(
            "enable Tailcat endpoint",
            ["enable", "ployz-tailcat.service"],
        )?;
        systemctl(
            "enable volume plugin socket",
            ["enable", "--now", "ployz-volume-plugin.socket"],
        )?;
    }
    Ok(())
}

pub(super) fn write_file_atomically(
    path: &Path,
    content: &str,
    stage: &'static str,
) -> Result<(), Error> {
    atomic_write(path, content.as_bytes(), 0o644).map_err(|source| Error::Io { stage, source })
}

pub(super) async fn install_docker(paths: &InstallPaths) -> Result<(), Error> {
    if command_exists("dockerd") {
        let mut command = Command::new("docker");
        command.args(["info", "-f", "{{ .DriverStatus }}"]);
        let snapshotter = command.output().ok().is_some_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("io.containerd.snapshotter")
        });
        if !snapshotter {
            eprintln!(
                "WARNING: Docker is retained unchanged; enable its containerd image store for best results"
            );
        }
        return Ok(());
    }
    let script = fetch("https://get.docker.com", "download Docker installer").await?;
    let mut command = Command::new("bash");
    command.args(["-o", "pipefail"]);
    command.stdin(Stdio::piped());
    let mut child = command.spawn().map_err(|source| Error::Io {
        stage: "start Docker installer",
        source,
    })?;
    child
        .stdin
        .take()
        .ok_or_else(|| {
            Error::Verification("Docker installer standard input was unavailable".into())
        })?
        .write_all(&script)
        .map_err(|source| Error::Io {
            stage: "send Docker installer",
            source,
        })?;
    let status = child.wait().map_err(|source| Error::Io {
        stage: "wait for Docker installer",
        source,
    })?;
    if !status.success() {
        return Err(Error::Command {
            stage: "install Docker".into(),
            message: format!("exited with {status}"),
        });
    }
    let parent = paths.docker_config.parent().ok_or_else(|| {
        Error::Verification(format!(
            "Docker config path {} has no parent",
            paths.docker_config.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|source| Error::Io {
        stage: "create Docker configuration directory",
        source,
    })?;
    write_file_atomically(
        &paths.docker_config,
        DOCKER_DAEMON_CONFIG,
        "write Docker configuration",
    )?;
    systemctl("restart Docker", ["restart", "docker"])?;
    Ok(())
}

pub(super) async fn verify_running_daemon(
    paths: &InstallPaths,
    target: &MachineVersion,
) -> Result<(), Error> {
    systemctl(
        "check daemon readiness",
        ["is-active", "--quiet", "ployz.service"],
    )?;
    let output = systemctl(
        "inspect running daemon",
        ["show", "--property=MainPID", "--value", "ployz.service"],
    )?;
    let pid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if pid.is_empty() || pid == "0" || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::Verification(format!(
            "ployz.service is active but did not report a daemon process ID ({pid:?})"
        )));
    }
    let running = fs::metadata(format!("/proc/{pid}/exe")).map_err(|source| Error::Io {
        stage: "inspect running daemon executable",
        source,
    })?;
    let installed = fs::metadata(paths.bin_dir.join("ployzd")).map_err(|source| Error::Io {
        stage: "inspect installed daemon executable",
        source,
    })?;
    if (running.dev(), running.ino()) != (installed.dev(), installed.ino()) {
        return Err(Error::Verification(
            "ployz.service is active but does not run the activated daemon executable".into(),
        ));
    }
    match installed_release(&paths.bin_dir.join("ployzd")).await? {
        Some(observed) if &observed == target => {}
        Some(observed) => Err(Error::Verification(format!(
            "activated daemon reported {observed}, expected {target}"
        )))?,
        None => {
            return Err(Error::Verification(
                "activated daemon no longer reports a valid version".into(),
            ));
        }
    }
    verify_daemon_contract(&paths.run_dir.join("ployz.sock"), target).await
}

pub(super) async fn verify_running_tailcat(
    paths: &InstallPaths,
    target: &MachineVersion,
) -> Result<(), Error> {
    systemctl(
        "check Tailcat readiness",
        ["is-active", "--quiet", "ployz-tailcat.service"],
    )?;
    let output = systemctl(
        "inspect Tailcat endpoint",
        [
            "show",
            "--property=MainPID",
            "--value",
            "ployz-tailcat.service",
        ],
    )?;
    let pid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if pid.is_empty() || pid == "0" || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::Verification(
            "Tailcat endpoint has no running process".into(),
        ));
    }
    let running = fs::metadata(format!("/proc/{pid}/exe")).map_err(|source| Error::Io {
        stage: "inspect running Tailcat executable",
        source,
    })?;
    let installed =
        fs::metadata(paths.bin_dir.join("ployzd-tailcat")).map_err(|source| Error::Io {
            stage: "inspect installed Tailcat executable",
            source,
        })?;
    if (running.dev(), running.ino()) != (installed.dev(), installed.ino())
        || installed_release(&paths.bin_dir.join("ployzd-tailcat"))
            .await?
            .as_ref()
            != Some(target)
    {
        return Err(Error::Verification(
            "Tailcat endpoint does not run the activated helper version".into(),
        ));
    }
    Ok(())
}

async fn verify_daemon_contract(socket: &Path, target: &MachineVersion) -> Result<(), Error> {
    let endpoint =
        Endpoint::from_shared(format!("unix:{}", socket.display())).map_err(|error| {
            Error::Verification(format!("invalid Machine API socket address: {error}"))
        })?;
    let channel = tokio::time::timeout(std::time::Duration::from_secs(10), endpoint.connect())
        .await
        .map_err(|_| Error::Verification("Machine API readiness timed out after 10s".into()))?
        .map_err(|error| Error::Verification(format!("Machine API is not ready: {error}")))?;
    let payload = op::DescribeContract::into_request(DescribeContractRequest {})
        .encode()
        .map_err(|error| Error::Verification(format!("encode readiness request: {error}")))?;
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        MachineRpcClient::new(channel).describe_contract(tonic::Request::new(payload)),
    )
    .await
    .map_err(|_| Error::Verification("Machine API readiness timed out after 10s".into()))?
    .map_err(|error| Error::Verification(format!("Machine API is not ready: {error}")))?
    .into_inner()
    .decode_response()
    .and_then(|response| response.decode::<op::DescribeContract>())
    .map_err(|error| Error::Verification(format!("decode readiness response: {error}")))?;
    require_running_version(&response.daemon_version, target)
}

fn require_running_version(observed: &str, target: &MachineVersion) -> Result<(), Error> {
    let observed = MachineVersion::parse(observed).map_err(|_| {
        Error::Verification(format!(
            "running Machine API reported invalid daemon version {observed:?}"
        ))
    })?;
    if &observed == target {
        Ok(())
    } else {
        Err(Error::Verification(format!(
            "running Machine API reported {observed}, expected {target}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailcat_state_directory_does_not_follow_a_symlink() {
        let root = tempfile::tempdir().unwrap();
        let paths = InstallPaths::at(root.path());
        fs::create_dir_all(&paths.data_dir).unwrap();
        std::os::unix::fs::symlink(root.path(), paths.data_dir.join("tailcat")).unwrap();
        assert!(matches!(
            create_tailcat_state_directory(&paths),
            Err(Error::Verification(_))
        ));
    }

    #[test]
    fn running_machine_api_version_must_match_the_target() {
        let target = MachineVersion::parse("1.2.3-beta.4").unwrap();
        assert!(require_running_version("1.2.3-beta.4", &target).is_ok());
        assert!(matches!(
            require_running_version("1.2.3-beta.3", &target),
            Err(Error::Verification(message))
                if message == "running Machine API reported 1.2.3-beta.3, expected 1.2.3-beta.4"
        ));
        assert!(matches!(
            require_running_version("not-a-version", &target),
            Err(Error::Verification(message))
                if message
                    == "running Machine API reported invalid daemon version \"not-a-version\""
        ));
    }
}
