//! Explicit Machine host preparation and systemd lifecycle management.

use std::{
    fs,
    io::Write,
    os::unix::fs::MetadataExt,
    path::Path,
    process::{Command, Stdio},
};

use tonic::transport::Endpoint;

use crate::filesystem::{MACHINE_API_SOCKET_MODE, PLOYZ_DIR_MODE, atomic_write};
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
    // ployz.socket recreates run_dir on boot, but the installer's MutationGate needs it before the units exist.
    let run = paths.run_dir.to_string_lossy();
    let mode = format!("{PLOYZ_DIR_MODE:04o}");
    run_host(
        "create Ployz directories",
        "install",
        [
            "-d", "-m", &mode, "-o", PLOYZ_USER, "-g", PLOYZ_USER, &data, &run,
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
    write_file_atomically(
        &paths.systemd_dir.join("ployz.service"),
        &machine_daemon_service_unit(&paths.bin_dir),
        "write Machine daemon service unit",
    )?;
    write_file_atomically(
        &paths.systemd_dir.join("ployz.socket"),
        &machine_api_socket_unit(&paths.run_dir),
        "write Machine API socket unit",
    )?;
    write_file_atomically(
        &paths.systemd_dir.join("ployz-volume-plugin.socket"),
        volume_plugin_socket_unit(),
        "write Volume plugin socket unit",
    )?;
    write_file_atomically(
        &paths.systemd_dir.join("ployz-volume-plugin.service"),
        &volume_plugin_service_unit(&paths.bin_dir),
        "write Volume plugin service unit",
    )?;
    if !install_only {
        systemctl("reload systemd units", ["daemon-reload"])?;
        systemctl("enable daemon", ["enable", "ployz.service"])?;
        systemctl(
            "enable Machine API socket",
            ["enable", "--now", "ployz.socket"],
        )?;
        systemctl(
            "enable volume plugin socket",
            ["enable", "--now", "ployz-volume-plugin.socket"],
        )?;
    }
    Ok(())
}

fn machine_daemon_service_unit(bin_dir: &Path) -> String {
    let bin = bin_dir.display();
    format!(
        "\
[Unit]
Description=Ployz Machine daemon
After=network-online.target docker.service ployz.socket
Wants=network-online.target
Requires=ployz.socket

[Service]
Type=notify
ExecStart={bin}/ployzd
# Set PLOYZ_LOG=debug in /etc/default/ployz to raise verbosity.
EnvironmentFile=-/etc/default/ployz
TimeoutStartSec=20
TimeoutStopSec=15
Restart=always
RestartPreventExitStatus=78
RestartSec=2
NoNewPrivileges=true
ProtectSystem=full
ProtectControlGroups=true
ProtectHome=read-only
ProtectKernelTunables=true
PrivateTmp=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK
RestrictNamespaces=true

[Install]
WantedBy=multi-user.target
"
    )
}

/// Any connect starts `ployz.service`; stopping the daemon on purpose means
/// stopping this socket too. `ExecStartPre` restores the `ployz` group on the
/// runtime directory, which systemd would otherwise create as root-only on boot.
fn machine_api_socket_unit(run_dir: &Path) -> String {
    let run = run_dir.display();
    format!(
        "\
[Unit]
Description=Ployz Machine API socket

[Socket]
ExecStartPre=/usr/bin/install -d -m {PLOYZ_DIR_MODE:04o} -o {PLOYZ_USER} -g {PLOYZ_USER} {run}
ListenStream={run}/ployz.sock
SocketMode={MACHINE_API_SOCKET_MODE:04o}
SocketGroup={PLOYZ_USER}
Accept=no

[Install]
WantedBy=sockets.target
"
    )
}

fn volume_plugin_socket_unit() -> &'static str {
    "\
[Unit]
Description=Ployz Docker Volume plugin socket
Before=docker.service

[Socket]
ListenStream=/run/docker/plugins/ployz.sock
SocketMode=0660
DirectoryMode=0755
Accept=no
Service=ployz-volume-plugin.service

[Install]
WantedBy=sockets.target
"
}

fn volume_plugin_service_unit(bin_dir: &Path) -> String {
    let bin = bin_dir.display();
    format!(
        "\
[Unit]
Description=Ployz Docker Volume plugin
Before=docker.service
After=zfs-import.target zfs-mount.service ployz-volume-plugin.socket
Requires=ployz-volume-plugin.socket docker.service

[Service]
Type=simple
ExecStart={bin}/ployzd volume-plugin
Sockets=ployz-volume-plugin.socket
EnvironmentFile=-/etc/default/ployz
Restart=on-failure
RestartSec=2
NoNewPrivileges=true
RestrictAddressFamilies=AF_UNIX
RestrictNamespaces=true
"
    )
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
    let installed = fs::metadata(paths.daemon()).map_err(|source| Error::Io {
        stage: "inspect installed daemon executable",
        source,
    })?;
    if (running.dev(), running.ino()) != (installed.dev(), installed.ino()) {
        return Err(Error::Verification(
            "ployz.service is active but does not run the activated daemon executable".into(),
        ));
    }
    match installed_release(&paths.daemon()).await? {
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
    fn machine_api_socket_unit_listens_where_the_daemon_serves() {
        let unit = machine_api_socket_unit(Path::new(super::super::DEFAULT_RUN_DIR));
        let listen = unit
            .lines()
            .find_map(|line| line.strip_prefix("ListenStream="))
            .unwrap();
        assert_eq!(listen, super::super::DEFAULT_SOCKET_PATH);
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
