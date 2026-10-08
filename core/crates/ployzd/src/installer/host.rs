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

use super::os_release::OsRelease;
use super::release::{fetch, installed_release};
use super::{
    Error, InstallPaths, PLOYZ_USER, command_exists, refuse, run_apt, run_host, systemctl,
};
use ployz_build::{MINIMUM_BUILDX, MINIMUM_DOCKER_API, MINIMUM_DOCKER_RELEASE};

const DOCKER_DAEMON_CONFIG: &str = r#"{
  "features": { "containerd-snapshotter": true },
  "live-restore": true,
  "log-driver": "json-file",
  "log-opts": { "max-size": "10m", "max-file": "3" }
}"#;

/// Docker's RHEL 9 repository, for Amazon Linux: get.docker.com refuses it and its own Docker
/// is too old. `$releasever` is pinned because Amazon Linux's is 2023.
const DOCKER_CE_REPO: &str = "[docker-ce-stable]
name=Docker CE Stable - $basearch
baseurl=https://download.docker.com/linux/rhel/9/$basearch/stable
enabled=1
gpgcheck=1
gpgkey=https://download.docker.com/linux/rhel/gpg
";

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
    write_file_atomically(
        &paths.systemd_dir.join("ployz-observe.service"),
        &observe_service_unit(&paths.bin_dir, &paths.run_dir),
        "write Log Store service unit",
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
        systemctl(
            "enable Log Store",
            ["enable", "--now", "ployz-observe.service"],
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
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
RestrictNamespaces=true
"
    )
}

/// The Log Store harvester runs as root to link Docker's root-only log files,
/// so its cgroup limits keep a runaway harvester from starving the Machine.
fn observe_service_unit(bin_dir: &Path, run_dir: &Path) -> String {
    let bin = bin_dir.display();
    let run = run_dir.display();
    format!(
        "\
[Unit]
Description=Ployz Log Store
After=docker.service
Wants=docker.service

[Service]
Type=simple
ExecStartPre=/usr/bin/install -d -m {PLOYZ_DIR_MODE:04o} -o {PLOYZ_USER} -g {PLOYZ_USER} {run}
ExecStart={bin}/ployzd observe
EnvironmentFile=-/etc/default/ployz
Restart=always
RestartSec=2
CPUQuota=50%
MemoryMax=256M
IOWeight=10
NoNewPrivileges=true
ProtectSystem=full
ProtectControlGroups=true
ProtectHome=read-only
ProtectKernelTunables=true
PrivateTmp=true
RestrictAddressFamilies=AF_UNIX
RestrictNamespaces=true

[Install]
WantedBy=multi-user.target
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
    // A retained Docker passed `verify_docker` before the install changed anything.
    if command_exists("dockerd") {
        return Ok(());
    }
    if OsRelease::read(&paths.os_release)?.is_amazon_linux() {
        write_file_atomically(
            &paths.yum_repos_dir.join("ployz-docker-ce.repo"),
            DOCKER_CE_REPO,
            "add Docker's repository",
        )?;
        run_host(
            "install Docker",
            "dnf",
            [
                "install",
                "-y",
                "docker-ce",
                "docker-ce-cli",
                "containerd.io",
                "docker-buildx-plugin",
            ],
        )?;
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
    verify_docker()
}

const STAGE: &str = "checking Docker";

/// Refuses a Docker Ployz can't use: too old an Engine API or buildx, or no containerd image
/// store. A Machine without Docker passes.
pub(super) fn verify_docker() -> Result<(), Error> {
    if !command_exists("dockerd") {
        return Ok(());
    }
    // The CLI can't read the server's version only when it can't reach the daemon.
    let output = docker_output([
        "version",
        "--format",
        "{{.Server.Version}} {{.Server.APIVersion}}",
    ])
    .map_err(|_| {
        refuse(
            STAGE,
            "Docker is installed but not running. Start it (systemctl start docker) and run this again.",
        )
    })?;
    let (version, api) = output.split_once(' ').unwrap_or((&output, ""));
    if !at_least(api, MINIMUM_DOCKER_API) {
        return Err(refuse(
            STAGE,
            format!(
                "Docker {version} is too old: Ployz needs Docker {MINIMUM_DOCKER_RELEASE} or newer. Upgrade Docker, or uninstall it and run this again so Ployz installs a current one."
            ),
        ));
    }
    // e.g. `github.com/docker/buildx v0.37.1 c8d4ec2`
    let output = docker_output(["buildx", "version"])?;
    let buildx = output.split_whitespace().nth(1).unwrap_or(&output);
    if !at_least(buildx.trim_start_matches('v'), MINIMUM_BUILDX) {
        let (major, minor) = MINIMUM_BUILDX;
        return Err(refuse(
            STAGE,
            format!(
                "Docker Buildx {buildx} is too old: Ployz needs Buildx {major}.{minor} or newer. Upgrade Docker, or uninstall it and run this again so Ployz installs a current one."
            ),
        ));
    }
    if !docker_output(["info", "-f", "{{.DriverStatus}}"])?.contains("io.containerd.snapshotter.v1")
    {
        return Err(refuse(
            STAGE,
            r#"Docker isn't using the containerd image store, which Ployz needs to build and move images between Servers. Enable it in /etc/docker/daemon.json ("features": {"containerd-snapshotter": true}) and restart Docker, or uninstall Docker and run this again."#,
        ));
    }
    Ok(())
}

fn docker_output<const N: usize>(args: [&str; N]) -> Result<String, Error> {
    let output = run_host(STAGE, "docker", args)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Whether `version`'s first two numbers (`1.53`, `0.18.0+ds1`) are at least `minimum`.
fn at_least(version: &str, minimum: (u32, u32)) -> bool {
    let mut parts = version
        .split(|c: char| !c.is_ascii_digit())
        .map(str::parse::<u32>);
    match (parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor))) => (major, minor) >= minimum,
        _ => false,
    }
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
    let pid = daemon_main_pid()?;
    let running =
        fs::metadata(paths.proc_dir.join(pid.to_string()).join("exe")).map_err(|source| {
            Error::Io {
                stage: "inspect running daemon executable",
                source,
            }
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

/// The process systemd currently runs as `ployz.service`'s main process.
pub(super) fn daemon_main_pid() -> Result<u32, Error> {
    let output = systemctl(
        "inspect running daemon",
        ["show", "--property=MainPID", "--value", "ployz.service"],
    )?;
    let pid = String::from_utf8_lossy(&output.stdout);
    let pid = pid.trim();
    match pid.parse() {
        Ok(0) | Err(_) => Err(Error::Verification(format!(
            "ployz.service did not report a daemon process ID ({pid:?})"
        ))),
        Ok(pid) => Ok(pid),
    }
}

/// Prove the Machine API on `socket` answers as `target`.
pub(super) async fn verify_daemon_contract(
    socket: &Path,
    target: &MachineVersion,
) -> Result<(), Error> {
    let endpoint =
        Endpoint::from_shared(format!("unix:{}", socket.display())).map_err(|error| {
            Error::Verification(format!(
                "invalid Machine API socket address: {error}",
                error = ployz_core::error_chain::inline(&error),
            ))
        })?;
    let channel = tokio::time::timeout(std::time::Duration::from_secs(10), endpoint.connect())
        .await
        .map_err(|_| Error::Verification("Machine API readiness timed out after 10s".into()))?
        .map_err(|error| {
            Error::Verification(format!(
                "Machine API is not ready: {error}",
                error = ployz_core::error_chain::inline(&error),
            ))
        })?;
    let payload = op::DescribeContract::into_request(DescribeContractRequest {})
        .encode()
        .map_err(|error| {
            Error::Verification(format!(
                "encode readiness request: {}",
                ployz_core::error_chain::inline(&error)
            ))
        })?;
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        MachineRpcClient::new(channel).describe_contract(tonic::Request::new(payload)),
    )
    .await
    .map_err(|_| Error::Verification("Machine API readiness timed out after 10s".into()))?
    .map_err(|error| {
        Error::Verification(format!(
            "Machine API is not ready: {error}",
            error = ployz_core::error_chain::inline(&error),
        ))
    })?
    .into_inner()
    .decode_response()
    .and_then(|response| response.decode::<op::DescribeContract>())
    .map_err(|error| {
        Error::Verification(format!(
            "decode readiness response: {}",
            ployz_core::error_chain::inline(&error)
        ))
    })?;
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

    #[test]
    fn docker_and_buildx_must_be_at_least_the_minimum() {
        for (version, minimum, supported) in [
            ("1.44", MINIMUM_DOCKER_API, false),
            ("1.52", MINIMUM_DOCKER_API, false),
            ("1.53", MINIMUM_DOCKER_API, true),
            ("1.54", MINIMUM_DOCKER_API, true),
            ("", MINIMUM_DOCKER_API, false),
            ("0.17.1", MINIMUM_BUILDX, false),
            ("0.18.0", MINIMUM_BUILDX, true),
            ("0.37.1+ds1", MINIMUM_BUILDX, true),
        ] {
            assert_eq!(at_least(version, minimum), supported, "{version}");
        }
    }

    struct DockerCase {
        name: &'static str,
        /// Whether `dockerd` is already there.
        retained: bool,
        /// `docker version`'s server version and API.
        server: &'static str,
        buildx: &'static str,
        driver_status: &'static str,
        refusal: Option<&'static str>,
    }

    const STORE: &str = "[[driver-type io.containerd.snapshotter.v1]]";

    const DOCKER_CASES: [DockerCase; 6] = [
        DockerCase {
            name: "amazon-fresh",
            retained: false,
            server: "29.4.0 1.54",
            buildx: "v0.37.1",
            driver_status: STORE,
            refusal: None,
        },
        DockerCase {
            name: "amazon-fresh-too-old",
            retained: false,
            server: "29.1.3 1.52",
            buildx: "v0.37.1",
            driver_status: STORE,
            refusal: Some(
                "Docker 29.1.3 is too old: Ployz needs Docker 29.2 or newer. Upgrade Docker, or uninstall it and run this again so Ployz installs a current one.",
            ),
        },
        DockerCase {
            name: "retained-too-old",
            retained: true,
            server: "25.0.16 1.44",
            buildx: "v0.12.1",
            driver_status: "[[Backing Filesystem extfs]]",
            refusal: Some(
                "Docker 25.0.16 is too old: Ployz needs Docker 29.2 or newer. Upgrade Docker, or uninstall it and run this again so Ployz installs a current one.",
            ),
        },
        DockerCase {
            name: "retained-old-buildx",
            retained: true,
            server: "29.2.0 1.53",
            buildx: "v0.17.1",
            driver_status: STORE,
            refusal: Some(
                "Docker Buildx v0.17.1 is too old: Ployz needs Buildx 0.18 or newer. Upgrade Docker, or uninstall it and run this again so Ployz installs a current one.",
            ),
        },
        DockerCase {
            name: "retained-stopped",
            retained: true,
            server: "",
            buildx: "v0.37.1",
            driver_status: STORE,
            refusal: Some(
                "Docker is installed but not running. Start it (systemctl start docker) and run this again.",
            ),
        },
        DockerCase {
            name: "retained-without-store",
            retained: true,
            server: "29.2.0 1.53",
            buildx: "v0.18.0",
            driver_status: "[[Backing Filesystem extfs]]",
            refusal: Some(
                r#"Docker isn't using the containerd image store, which Ployz needs to build and move images between Servers. Enable it in /etc/docker/daemon.json ("features": {"containerd-snapshotter": true}) and restart Docker, or uninstall Docker and run this again."#,
            ),
        },
    ];

    /// A retained Docker meets only the preflight; a fresh one is installed and then checked.
    #[tokio::test]
    async fn docker_contract() {
        use super::super::test_support::{
            fixture, run_contract_child_with_environment, write_script,
        };
        use std::{env, ffi::OsString};

        const CASE: &str = "PLOYZ_DOCKER_CONTRACT";
        if let Ok(name) = env::var(CASE) {
            let root =
                std::path::PathBuf::from(env::var_os("PLOYZ_INSTALLER_CONTRACT_ROOT").unwrap());
            let case = DOCKER_CASES.iter().find(|case| case.name == name).unwrap();
            let result = if case.retained {
                verify_docker()
            } else {
                install_docker(&InstallPaths::at(&root)).await
            };
            match case.refusal {
                None => result.unwrap(),
                Some(refusal) => assert!(
                    matches!(&result, Err(Error::Command { stage, message })
                        if stage == "checking Docker" && message == refusal),
                    "{result:?}"
                ),
            }
            fs::write(root.join("child-completed"), name).unwrap();
            return;
        }

        for case in &DOCKER_CASES {
            let name = case.name;
            let fixture = fixture(name);
            let root = fixture.path();
            let commands = root.join("commands");
            fs::create_dir_all(&commands).unwrap();
            fs::create_dir_all(root.join("yum.repos.d")).unwrap();
            fs::write(
                root.join("os-release"),
                "NAME=\"Amazon Linux\"\nID=\"amzn\"\nVERSION_ID=\"2023\"\n",
            )
            .unwrap();
            if case.retained {
                write_script(&commands.join("dockerd"), "exit 0");
            }
            write_script(
                &commands.join("docker"),
                &format!(
                    "case \"$1\" in\nversion) [ -n '{0}' ] || exit 1; echo '{0}' ;;\nbuildx) echo 'github.com/docker/buildx {1} 0000000' ;;\ninfo) echo '{2}' ;;\nesac",
                    case.server, case.buildx, case.driver_status
                ),
            );
            // Installing Docker puts `dockerd` on the PATH.
            write_script(
                &commands.join("dnf"),
                "echo \"$*\" >> \"$PLOYZ_INSTALLER_CONTRACT_ROOT/dnf.log\"\nprintf '#!/bin/sh\\n' > \"$PLOYZ_INSTALLER_CONTRACT_ROOT/commands/dockerd\"\n/bin/chmod 755 \"$PLOYZ_INSTALLER_CONTRACT_ROOT/commands/dockerd\"",
            );
            write_script(
                &commands.join("systemctl"),
                "echo \"$*\" >> \"$PLOYZ_INSTALLER_CONTRACT_ROOT/systemctl.log\"",
            );
            run_contract_child_with_environment(
                "installer::host::tests::docker_contract",
                root,
                OsString::from(CASE),
                OsString::from(name),
                name,
                [],
            );

            let log = |command: &str| {
                fs::read_to_string(root.join(format!("{command}.log"))).unwrap_or_default()
            };
            let repo = fs::read_to_string(root.join("yum.repos.d/ployz-docker-ce.repo"));
            let daemon_config = fs::read_to_string(root.join("docker/daemon.json"));
            if case.retained {
                assert_eq!(log("dnf"), "", "{name}");
                assert_eq!(log("systemctl"), "", "{name}");
                assert!(repo.is_err() && daemon_config.is_err(), "{name}");
            } else {
                // Docker's RHEL 9 repository, whatever Amazon Linux's own release is.
                assert!(
                    repo.unwrap()
                        .lines()
                        .any(|line| line.starts_with("baseurl=") && line.contains("/rhel/9/")),
                    "{name}"
                );
                let dnf = log("dnf");
                let mut words = dnf.split_whitespace();
                assert_eq!(words.next(), Some("install"), "{name}");
                let packages: Vec<_> = words.filter(|word| !word.starts_with('-')).collect();
                for package in [
                    "docker-ce",
                    "docker-ce-cli",
                    "containerd.io",
                    "docker-buildx-plugin",
                ] {
                    assert!(packages.contains(&package), "{name}: {package}");
                }
                assert_eq!(
                    log("systemctl"),
                    "enable docker\nrestart docker\n",
                    "{name}"
                );
                let daemon_config: serde_json::Value =
                    serde_json::from_str(&daemon_config.unwrap()).unwrap();
                assert_eq!(
                    daemon_config.pointer("/features/containerd-snapshotter"),
                    Some(&serde_json::Value::Bool(true)),
                    "{name}"
                );
            }
        }
    }
}
