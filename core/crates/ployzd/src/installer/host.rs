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

use super::release::{fetch, installed_release, verify_checksum};
use super::storage::OsRelease;
use super::{
    Error, InstallPaths, PLOYZ_USER, command_exists, run_apt, run_command, run_host, systemctl,
};

const DOCKER_DAEMON_CONFIG: &str = r#"{
  "features": { "containerd-snapshotter": true },
  "live-restore": true,
  "log-driver": "json-file",
  "log-opts": { "max-size": "10m", "max-file": "3" }
}"#;

/// The buildx Amazon Linux gets in place of the 0.12 its `docker` package bundles, which lacks
/// options Server builds use. The version get.docker.com installs; bumping it is deliberate.
const BUILDX_VERSION: &str = "0.37.1";
/// Per `std::env::consts::ARCH`: the release's platform and its binary's SHA-256.
const BUILDX_BINARIES: [(&str, &str, &str); 2] = [
    (
        "x86_64",
        "amd64",
        "9447199cdb435f25880548343c128a4b6650e8891ee598905d8d29d39a8e359b",
    ),
    (
        "aarch64",
        "arm64",
        "e5cc9fe3bbff5cbc91230981f7860e06076110730a2db997082652199042a1f2",
    ),
];

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
    let amazon = OsRelease::read(&paths.os_release).is_ok_and(|os| os.id == "amzn");
    install_docker_engine(paths, amazon).await?;
    if amazon {
        let arch = std::env::consts::ARCH;
        let Some((_, platform, sha256)) = BUILDX_BINARIES.iter().find(|(name, ..)| *name == arch)
        else {
            return Err(Error::UnsupportedArchitecture(arch.into()));
        };
        install_buildx(&paths.docker_plugins_dir, platform, sha256)?;
    }
    Ok(())
}

async fn install_docker_engine(paths: &InstallPaths, amazon: bool) -> Result<(), Error> {
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
    if amazon {
        // get.docker.com refuses Amazon Linux; its own package is Docker 25.
        run_host("install Docker", "dnf", ["install", "-y", "docker"])?;
        systemctl("enable Docker", ["enable", "docker"])?;
    } else {
        run_docker_script().await?;
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

/// Installs [`BUILDX_VERSION`] for `platform` where Docker finds it first, checked against
/// `sha256`; a copy that already matches is kept.
fn install_buildx(plugins_dir: &Path, platform: &str, sha256: &str) -> Result<(), Error> {
    let plugin = plugins_dir.join("docker-buildx");
    if fs::read(&plugin).is_ok_and(|bytes| verify_checksum(&bytes, "docker-buildx", sha256).is_ok())
    {
        return Ok(());
    }
    let release = format!("buildx-v{BUILDX_VERSION}.linux-{platform}");
    let mut download = Command::new("curl");
    download
        .args(["--proto", "=https", "--tlsv1.2", "-fsSL", "--retry", "3"])
        .arg(format!(
            "https://github.com/docker/buildx/releases/download/v{BUILDX_VERSION}/{release}"
        ));
    let bytes = run_command("download buildx", &mut download)?.stdout;
    verify_checksum(&bytes, &release, sha256)?;
    fs::create_dir_all(plugins_dir).map_err(|source| Error::Io {
        stage: "create Docker CLI plugin directory",
        source,
    })?;
    atomic_write(&plugin, &bytes, 0o755).map_err(|source| Error::Io {
        stage: "install buildx",
        source,
    })
}

async fn run_docker_script() -> Result<(), Error> {
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

    struct DockerCase {
        name: &'static str,
        os_release: &'static str,
        /// Whether `dockerd` is already on the Machine.
        docker_present: bool,
    }

    const AMAZON: &str = "NAME=\"Amazon Linux\"\nID=\"amzn\"\nVERSION_ID=\"2023\"\n";
    const FAKE_BUILDX: &str = "fake buildx";

    const DOCKER_CASES: [DockerCase; 3] = [
        DockerCase {
            name: "amazon-fresh",
            os_release: AMAZON,
            docker_present: false,
        },
        DockerCase {
            name: "amazon-retained",
            os_release: AMAZON,
            docker_present: true,
        },
        DockerCase {
            name: "debian-retained",
            os_release: "NAME=\"Debian GNU/Linux\"\nID=debian\nVERSION_ID=\"13\"\n",
            docker_present: true,
        },
    ];

    /// The Docker plan for `case`. On Amazon Linux the fake buildx fails the pinned checksum,
    /// so the child then installs it against its own checksum, twice, to show the rerun skips it.
    #[tokio::test]
    async fn amazon_linux_docker_contract() {
        use super::super::test_support::{
            fixture, run_contract_child_with_environment, write_script,
        };
        use sha2::{Digest, Sha256};
        use std::{env, ffi::OsString, os::unix::fs::PermissionsExt};

        const CASE: &str = "PLOYZ_DOCKER_CONTRACT";
        if let Ok(name) = env::var(CASE) {
            let case = DOCKER_CASES.iter().find(|case| case.name == name).unwrap();
            let root =
                std::path::PathBuf::from(env::var_os("PLOYZ_INSTALLER_CONTRACT_ROOT").unwrap());
            let paths = InstallPaths::at(&root);
            let result = install_docker(&paths).await;
            if case.os_release == AMAZON {
                assert!(
                    matches!(&result, Err(Error::Verification(message)) if message.contains("checksum")),
                    "{result:?}"
                );
                assert!(!paths.docker_plugins_dir.join("docker-buildx").exists());
                let sha256 = hex::encode(Sha256::digest(FAKE_BUILDX));
                install_buildx(&paths.docker_plugins_dir, "amd64", &sha256).unwrap();
                install_buildx(&paths.docker_plugins_dir, "amd64", &sha256).unwrap();
            } else {
                result.unwrap();
            }
            fs::write(root.join("child-completed"), name).unwrap();
            return;
        }

        for case in &DOCKER_CASES {
            let fixture = fixture(case.name);
            let root = fixture.path();
            let commands = root.join("commands");
            fs::create_dir_all(&commands).unwrap();
            fs::write(root.join("os-release"), case.os_release).unwrap();
            for command in ["dnf", "systemctl", "docker"] {
                write_script(
                    &commands.join(command),
                    &format!(r#"echo "$*" >> "$PLOYZ_INSTALLER_CONTRACT_ROOT/{command}.log""#),
                );
            }
            write_script(
                &commands.join("curl"),
                &format!(
                    r#"echo "$*" >> "$PLOYZ_INSTALLER_CONTRACT_ROOT/curl.log"; printf '{FAKE_BUILDX}'"#
                ),
            );
            if case.docker_present {
                write_script(&commands.join("dockerd"), "exit 0");
            }
            let plugin = root.join("cli-plugins/docker-buildx");
            run_contract_child_with_environment(
                "installer::host::tests::amazon_linux_docker_contract",
                root,
                OsString::from(CASE),
                OsString::from(case.name),
                case.name,
                [],
            );

            let name = case.name;
            let log = |command: &str| {
                fs::read_to_string(root.join(format!("{command}.log"))).unwrap_or_default()
            };
            if case.docker_present {
                assert_eq!(log("dnf"), "", "{name}");
                assert_eq!(log("systemctl"), "", "{name}");
                assert!(!root.join("docker/daemon.json").exists(), "{name}");
            } else {
                assert_eq!(log("dnf"), "install -y docker\n", "{name}");
                assert_eq!(
                    log("systemctl"),
                    "enable docker\nrestart docker\n",
                    "{name}"
                );
                assert_eq!(
                    fs::read_to_string(root.join("docker/daemon.json")).unwrap(),
                    DOCKER_DAEMON_CONFIG
                );
            }
            if case.os_release != AMAZON {
                assert_eq!(log("curl"), "", "{name}");
                assert!(!plugin.exists(), "{name}");
                continue;
            }
            assert_eq!(fs::read_to_string(&plugin).unwrap(), FAKE_BUILDX, "{name}");
            assert_eq!(
                fs::metadata(&plugin).unwrap().permissions().mode() & 0o777,
                0o755,
                "{name}"
            );
            let arch = BUILDX_BINARIES
                .iter()
                .find(|(arch, ..)| *arch == std::env::consts::ARCH)
                .unwrap()
                .1;
            let url = |platform: &str| {
                format!(
                    "--proto =https --tlsv1.2 -fsSL --retry 3 https://github.com/docker/buildx/releases/download/v0.37.1/buildx-v0.37.1.linux-{platform}\n"
                )
            };
            assert_eq!(log("curl"), url(arch) + &url("amd64"), "{name}");
        }
    }
}
