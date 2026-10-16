use std::{
    fs, io,
    os::unix::fs::FileTypeExt,
    path::{Path, PathBuf},
    process::Command,
};

use ployz_build::{MINIMUM_BUILDX, MINIMUM_DOCKER_API, MINIMUM_DOCKER_RELEASE};
use serde::Serialize;
use serde_json::Value;

use super::os_release::OsRelease;
use super::storage::ZfsRoute;
use super::{Error, InstallPaths, command_exists, run_command, run_host};

const GIB: u64 = 1024 * 1024 * 1024;
const MINIMUM_DISK_BYTES: u64 = 30 * GIB;
pub(super) const ZFS_SMOKE_BYTES: u64 = 128 * 1024 * 1024;
const ROOT_RESERVE: &str = "root_reserve";
const DOCKER_STAGE: &str = "checking Docker";
const ROOT_STAGE: &str = "inspect host-root capacity for ZFS storage preparation";
const DISKS_STAGE: &str = "list disks";

#[derive(Debug, Serialize)]
pub(super) struct HostFacts {
    pub(super) os: OsRelease,
    pub(super) kernel: String,
    pub(super) arch: String,
    pub(super) virt: Virtualization,
    /// `None` when the EFI variable couldn't be read.
    pub(super) secure_boot: Option<bool>,
    pub(super) cgroup: Option<CgroupVersion>,
    pub(super) systemd: bool,
    pub(super) cpu_count: Option<u32>,
    pub(super) memory_total_bytes: Option<u64>,
    pub(super) root_filesystem: Filesystem,
    /// The root disk and every whole disk with nothing on it; `None` when `lsblk` failed.
    pub(super) disks: Option<Vec<Disk>>,
    pub(super) docker: DockerFacts,
    /// `None` when the ZFS module is loaded but `zpool list` failed.
    pub(super) zpool_present: Option<bool>,
    /// Why a fact above that could have been read is `null` or cut short.
    pub(super) unread: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) enum Virtualization {
    BareMetal,
    Vm {
        kind: String,
    },
    OpenVz,
    /// `privileged` is `None` when the container's user namespace map couldn't be read.
    Lxc {
        privileged: Option<bool>,
    },
    Container {
        kind: String,
    },
    Unknown,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
pub(super) enum CgroupVersion {
    V1,
    V2,
    Hybrid,
}

#[derive(Debug, Serialize, Clone, Copy)]
pub(super) struct Filesystem {
    pub(super) size_bytes: u64,
    pub(super) available_bytes: u64,
}

impl Filesystem {
    pub(super) fn read_root() -> Result<Self, Error> {
        let (size_bytes, available_bytes) = crate::host_capacity::filesystem_space(Path::new("/"))
            .map_err(|source| Error::Io {
                stage: ROOT_STAGE,
                source,
            })?;
        Ok(Self {
            size_bytes,
            available_bytes,
        })
    }
}

#[derive(Debug, Serialize)]
pub(super) struct Disk {
    pub(super) path: String,
    pub(super) by_id: Option<String>,
    pub(super) size_bytes: u64,
    pub(super) role: DiskRole,
}

#[derive(Debug, Serialize)]
pub(super) enum DiskRole {
    Root { available: u64 },
    Empty,
}

/// Docker as the installer sees it. Reading stops at the first check Docker fails or can't
/// answer, so a later field stays `None` exactly when the installer never got to ask.
#[derive(Debug, Serialize)]
pub(super) enum DockerFacts {
    Absent,
    Stopped,
    Present {
        version: String,
        api: String,
        buildx: Option<String>,
        containerd_store: Option<bool>,
        /// The failed command that stopped reading, as the installer would refuse with it.
        error: Option<String>,
    },
}

#[derive(Debug, Serialize)]
pub(super) struct Assessment {
    pub(super) blockers: Vec<Blocker>,
    pub(super) warnings: Vec<Blocker>,
    pub(super) zfs: ZfsEligibility,
}

/// A stable `code` for machines and an English `message` for people.
#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub(super) struct Blocker {
    pub(super) code: &'static str,
    pub(super) message: String,
}

impl Blocker {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) enum ZfsEligibility {
    Eligible {
        targets: Vec<ZfsTarget>,
        prepare_minutes: u32,
    },
    Ineligible {
        reason: Blocker,
    },
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(super) enum ZfsTarget {
    MainDisk,
    Disk { by_id: String },
}

pub(super) fn read_facts(paths: &InstallPaths) -> Result<HostFacts, Error> {
    let os = OsRelease::read(&paths.os_release)?;
    let uts = nix::sys::utsname::uname().map_err(|errno| Error::Io {
        stage: "read kernel and architecture",
        source: io::Error::from(errno),
    })?;
    let root_filesystem = Filesystem::read_root()?;
    let mut unread = Vec::new();
    let mut keep = |error: Option<Error>| unread.extend(error.map(|error| error.to_string()));
    let (virt, error) = read_virtualization(&paths.proc_dir);
    keep(error);
    // Install reads only this variable, so the probe agrees with it rather than asking mokutil.
    let secure_boot = match read_secure_boot_variable(&paths.secure_boot) {
        Ok(enabled) => Some(enabled),
        Err(error) => {
            keep(Some(error));
            None
        }
    };
    let cgroup = keep_ok(&mut keep, read_cgroup(&paths.proc_dir));
    let cpu_count = keep_ok(&mut keep, read_cpu_count(&paths.proc_dir));
    let memory_total_bytes = keep_ok(&mut keep, read_memory_total(&paths.proc_dir));
    let (disks, errors) = read_disks(root_filesystem);
    for error in errors {
        keep(Some(error));
    }
    // The Docker error lives in `docker.error` and becomes a blocker there.
    let docker = if docker_waits_on_its_socket() {
        DockerFacts::Stopped
    } else {
        read_docker().0
    };
    let (zpool_present, error) = read_zpool_present(Path::new("/sys/module/zfs"));
    keep(error);
    Ok(HostFacts {
        os,
        kernel: uts.release().to_string_lossy().into_owned(),
        arch: uts.machine().to_string_lossy().into_owned(),
        virt,
        secure_boot,
        cgroup,
        systemd: Path::new("/run/systemd/system").is_dir(),
        cpu_count,
        memory_total_bytes,
        root_filesystem,
        disks,
        docker,
        zpool_present,
        unread,
    })
}

fn keep_ok<T>(keep: &mut impl FnMut(Option<Error>), result: Result<T, Error>) -> Option<T> {
    result.map_err(|error| keep(Some(error))).ok()
}

/// Whether `docker.service` is stopped while `docker.socket` listens, so any `docker` command
/// would start the daemon, and with it every container whose restart policy brings it back.
fn docker_waits_on_its_socket() -> bool {
    let active = |unit| {
        Command::new("systemctl")
            .args(["is-active", "--quiet", unit])
            .status()
            .is_ok_and(|status| status.success())
    };
    command_exists("dockerd") && active("docker.socket") && !active("docker.service")
}

fn unreadable(stage: &str, message: impl Into<String>) -> Error {
    Error::Command {
        stage: stage.into(),
        message: message.into(),
    }
}

fn read_proc(proc_dir: &Path, file: &str, stage: &'static str) -> Result<String, Error> {
    fs::read_to_string(proc_dir.join(file)).map_err(|source| Error::Io { stage, source })
}

fn read_cgroup(proc_dir: &Path) -> Result<CgroupVersion, Error> {
    const STAGE: &str = "read cgroup layout";
    let mounts = read_proc(proc_dir, "self/mounts", STAGE)?;
    cgroup_version(&mounts).ok_or_else(|| unreadable(STAGE, "no cgroup mount at /sys/fs/cgroup"))
}

fn read_cpu_count(proc_dir: &Path) -> Result<u32, Error> {
    const STAGE: &str = "read CPU count";
    let cpuinfo = read_proc(proc_dir, "cpuinfo", STAGE)?;
    let count = cpuinfo
        .lines()
        .filter(|line| line.split(':').next().map(str::trim) == Some("processor"))
        .count();
    u32::try_from(count)
        .ok()
        .filter(|count| *count > 0)
        .ok_or_else(|| unreadable(STAGE, "no processor lines in cpuinfo"))
}

fn read_memory_total(proc_dir: &Path) -> Result<u64, Error> {
    const STAGE: &str = "read memory size";
    let meminfo = read_proc(proc_dir, "meminfo", STAGE)?;
    meminfo
        .lines()
        .find_map(|line| {
            line.strip_prefix("MemTotal:")?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        })
        .map(|kib| kib.saturating_mul(1024))
        .ok_or_else(|| unreadable(STAGE, "no MemTotal line in meminfo"))
}

/// Whether any pool is imported. `zpool` loads the ZFS module when it isn't loaded, so it runs
/// only once the module is, and an unloaded module means no pool.
pub(super) fn read_zpool_present(module: &Path) -> (Option<bool>, Option<Error>) {
    if !module.is_dir() {
        return (Some(false), None);
    }
    match run_host("list ZFS pools", "zpool", ["list", "-H"]) {
        Ok(output) => (Some(!output.stdout.trim_ascii().is_empty()), None),
        Err(error) => (None, Some(error)),
    }
}

/// Container kinds `systemd-detect-virt` names; anything else it names is a VM.
const CONTAINERS: [&str; 9] = [
    "docker",
    "podman",
    "rkt",
    "systemd-nspawn",
    "lxc-libvirt",
    "proot",
    "pouch",
    "wsl",
    "container-other",
];

/// The virtualization this Machine runs under, and why an LXC container's privilege is unknown.
pub(super) fn read_virtualization(proc_dir: &Path) -> (Virtualization, Option<Error>) {
    // OpenVZ hosts expose /proc/bc; its containers have only /proc/vz.
    if proc_dir.join("vz").is_dir() && !proc_dir.join("bc").is_dir() {
        return (Virtualization::OpenVz, None);
    }
    // It exits 1 when it prints `none`, so its output is the answer either way.
    let kind = Command::new("systemd-detect-virt")
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();
    let virt = match kind.as_str() {
        "" => Virtualization::Unknown,
        "none" => Virtualization::BareMetal,
        "openvz" => Virtualization::OpenVz,
        "lxc" => {
            return match lxc_is_unprivileged(proc_dir) {
                Ok(unprivileged) => (
                    Virtualization::Lxc {
                        privileged: Some(!unprivileged),
                    },
                    None,
                ),
                Err(error) => (Virtualization::Lxc { privileged: None }, Some(error)),
            };
        }
        kind if CONTAINERS.contains(&kind) => Virtualization::Container { kind: kind.into() },
        kind => Virtualization::Vm { kind: kind.into() },
    };
    (virt, None)
}

fn lxc_is_unprivileged(proc_dir: &Path) -> Result<bool, Error> {
    let map = fs::read_to_string(proc_dir.join("self/uid_map")).map_err(|source| Error::Io {
        stage: "inspect LXC user namespace",
        source,
    })?;
    let mut fields = map.split_whitespace();
    Ok(matches!(
        (fields.next(), fields.next()),
        (Some("0"), Some(outside)) if outside != "0"
    ))
}

/// Firmware Secure Boot state from its EFI variable. A Machine without EFI variables boots with
/// it off.
pub(super) fn read_secure_boot_variable(variable: &Path) -> Result<bool, Error> {
    match fs::read(variable) {
        // The variable is 4 attribute bytes followed by one data byte; 1 means on.
        Ok(variable) => Ok(variable.last() == Some(&1)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(Error::Io {
            stage: "read Secure Boot state",
            source,
        }),
    }
}

pub(super) fn cgroup_version(mounts: &str) -> Option<CgroupVersion> {
    // A later mount over the same target hides the earlier ones.
    let fstype = |target: &str| {
        mounts.lines().rev().find_map(|line| {
            let mut fields = line.split_whitespace().skip(1);
            (fields.next()? == target).then(|| fields.next()).flatten()
        })
    };
    match fstype("/sys/fs/cgroup")? {
        "cgroup2" => Some(CgroupVersion::V2),
        "tmpfs" if fstype("/sys/fs/cgroup/unified") == Some("cgroup2") => {
            Some(CgroupVersion::Hybrid)
        }
        "tmpfs" => Some(CgroupVersion::V1),
        _ => None,
    }
}

/// The disks `lsblk` lists, and why a disk or the whole listing couldn't be read.
fn read_disks(root: Filesystem) -> (Option<Vec<Disk>>, Vec<Error>) {
    // util-linux before 2.37 has only MOUNTPOINT, one mount per device.
    let mut failure = None;
    let output = ["MOUNTPOINTS", "MOUNTPOINT"]
        .into_iter()
        .find_map(|mounts| {
            let mut command = Command::new("lsblk");
            command.args([
                "-J",
                "-b",
                "-o",
                &format!("NAME,PATH,SIZE,TYPE,FSTYPE,PTTYPE,{mounts},PKNAME"),
            ]);
            run_command(DISKS_STAGE, &mut command)
                .map_err(|error| failure = Some(error))
                .ok()
        });
    let Some(output) = output else {
        return (None, failure.into_iter().collect());
    };
    let listing: Value = match serde_json::from_slice(&output.stdout) {
        Ok(listing) => listing,
        Err(error) => return (None, vec![unreadable(DISKS_STAGE, error.to_string())]),
    };
    let by_id = disk_ids(Path::new("/dev/disk/by-id"));
    let (disks, errors) = disks_from_lsblk(&listing, root, &by_id);
    (Some(disks), errors)
}

/// The root disk and every empty whole disk, and an error for each such disk whose size didn't
/// parse.
pub(super) fn disks_from_lsblk(
    listing: &Value,
    root: Filesystem,
    by_id: &[(PathBuf, String)],
) -> (Vec<Disk>, Vec<Error>) {
    let devices = listing["blockdevices"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let mut errors = Vec::new();
    let disks = devices
        .iter()
        .filter(|device| device["type"] == "disk")
        .filter_map(|device| {
            let path = device["path"].as_str()?;
            let role = if mounts_root(device) {
                DiskRole::Root {
                    available: root.available_bytes,
                }
            } else if device["fstype"].is_null()
                && device["pttype"].is_null()
                && device["children"].as_array().is_none_or(Vec::is_empty)
                && mounts(device).all(Value::is_null)
            {
                DiskRole::Empty
            } else {
                return None;
            };
            let size_bytes = match &device["size"] {
                Value::String(size) => size.parse().ok(),
                Value::Number(size) => size.as_u64(),
                Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => None,
            };
            let Some(size_bytes) = size_bytes else {
                errors.push(unreadable(
                    DISKS_STAGE,
                    format!("{path} has size {}", device["size"]),
                ));
                return None;
            };
            Some(Disk {
                path: path.to_owned(),
                by_id: by_id
                    .iter()
                    .find(|(target, _)| target == Path::new(path))
                    .map(|(_, name)| name.clone()),
                size_bytes,
                role,
            })
        })
        .collect();
    (disks, errors)
}

/// `mountpoints` from util-linux 2.37 on, `mountpoint` before it.
fn mounts(device: &Value) -> impl Iterator<Item = &Value> {
    match device["mountpoints"].as_array() {
        Some(mounts) => mounts.iter(),
        None => std::slice::from_ref(&device["mountpoint"]).iter(),
    }
}

fn mounts_root(device: &Value) -> bool {
    mounts(device).any(|mount| mount == "/")
        || device["children"]
            .as_array()
            .is_some_and(|children| children.iter().any(mounts_root))
}

fn disk_ids(directory: &Path) -> Vec<(PathBuf, String)> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut ids: Vec<(PathBuf, String)> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_symlink()))
        .filter_map(|entry| {
            let target = fs::canonicalize(entry.path()).ok()?;
            fs::metadata(&target)
                .is_ok_and(|metadata| metadata.file_type().is_block_device())
                .then(|| (target, entry.file_name().to_string_lossy().into_owned()))
        })
        .collect();
    ids.sort_by_key(|(_, name)| (is_hex_serial_id(name), name.clone()));
    ids
}

fn is_hex_serial_id(name: &str) -> bool {
    name.starts_with("wwn-") || name.starts_with("nvme-eui.")
}

/// Docker's facts, and the failed command that stopped reading them.
pub(super) fn read_docker() -> (DockerFacts, Option<Error>) {
    if !command_exists("dockerd") {
        return (DockerFacts::Absent, None);
    }
    // The CLI can't read the server's version only when it can't reach the daemon.
    let Ok(output) = docker_output([
        "version",
        "--format",
        "{{.Server.Version}} {{.Server.APIVersion}}",
    ]) else {
        return (DockerFacts::Stopped, None);
    };
    let (version, api) = output.split_once(' ').unwrap_or((&output, ""));
    let present = |buildx, containerd_store, error: Option<&Error>| DockerFacts::Present {
        version: version.to_owned(),
        api: api.to_owned(),
        buildx,
        containerd_store,
        error: error.map(Error::to_string),
    };
    if !at_least(api, MINIMUM_DOCKER_API) {
        return (present(None, None, None), None);
    }
    // e.g. `github.com/docker/buildx v0.37.1 c8d4ec2`
    let output = match docker_output(["buildx", "version"]) {
        Ok(output) => output,
        Err(error) => return (present(None, None, Some(&error)), Some(error)),
    };
    let buildx = output
        .split_whitespace()
        .nth(1)
        .unwrap_or(&output)
        .to_owned();
    if !at_least(buildx.trim_start_matches('v'), MINIMUM_BUILDX) {
        return (present(Some(buildx), None, None), None);
    }
    match docker_output(["info", "-f", "{{.DriverStatus}}"]) {
        Ok(status) => {
            let containerd_store = status.contains("io.containerd.snapshotter.v1");
            (present(Some(buildx), Some(containerd_store), None), None)
        }
        Err(error) => (present(Some(buildx), None, Some(&error)), Some(error)),
    }
}

fn docker_output<const N: usize>(args: [&str; N]) -> Result<String, Error> {
    let output = run_host(DOCKER_STAGE, "docker", args)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub(super) fn at_least(version: &str, minimum: (u32, u32)) -> bool {
    let mut parts = version
        .split(|c: char| !c.is_ascii_digit())
        .map(str::parse::<u32>);
    match (parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor))) => (major, minor) >= minimum,
        _ => false,
    }
}

pub(super) fn assess(facts: &HostFacts) -> Assessment {
    let mut blockers = Vec::new();
    blockers.extend(docker_blocker(&facts.docker));
    if !facts.systemd {
        blockers.push(Blocker::new(
            "no_systemd",
            Error::SystemdRequired.to_string(),
        ));
    }
    let root_disk = facts
        .disks
        .iter()
        .flatten()
        .find(|disk| matches!(disk.role, DiskRole::Root { .. }));
    if let Some(disk) = root_disk.filter(|disk| disk.size_bytes < MINIMUM_DISK_BYTES) {
        blockers.push(Blocker::new(
            "disk_too_small",
            format!(
                "Disk is {} GiB. Ployz needs {} GiB.",
                disk.size_bytes / GIB,
                MINIMUM_DISK_BYTES / GIB
            ),
        ));
    }
    let mut warnings = Vec::new();
    if matches!(
        facts.cgroup,
        Some(CgroupVersion::V1 | CgroupVersion::Hybrid)
    ) {
        warnings.push(Blocker::new(
            "cgroup_v1",
            "This Server uses cgroup v1. Ployz expects cgroup v2, so container limits may not apply.",
        ));
    }
    Assessment {
        blockers,
        warnings,
        zfs: zfs_eligibility(facts),
    }
}

pub(super) fn docker_blocker(docker: &DockerFacts) -> Option<Blocker> {
    let DockerFacts::Present {
        version,
        api,
        buildx,
        containerd_store,
        error,
    } = docker
    else {
        return matches!(docker, DockerFacts::Stopped).then(|| {
            Blocker::new(
                "docker_stopped",
                "Docker is installed but not running. Start it (systemctl start docker) and run this again.",
            )
        });
    };
    if !at_least(api, MINIMUM_DOCKER_API) {
        return Some(Blocker::new(
            "docker_too_old",
            format!(
                "Docker {version} is too old: Ployz needs Docker {MINIMUM_DOCKER_RELEASE} or newer. Upgrade Docker, or uninstall it and run this again so Ployz installs a current one."
            ),
        ));
    }
    if let Some(error) = error {
        return Some(Blocker::new("docker_unreadable", error.clone()));
    }
    if let Some(buildx) = buildx.as_deref()
        && !at_least(buildx.trim_start_matches('v'), MINIMUM_BUILDX)
    {
        let (major, minor) = MINIMUM_BUILDX;
        return Some(Blocker::new(
            "buildx_too_old",
            format!(
                "Docker Buildx {buildx} is too old: Ployz needs Buildx {major}.{minor} or newer. Upgrade Docker, or uninstall it and run this again so Ployz installs a current one."
            ),
        ));
    }
    (*containerd_store == Some(false)).then(|| {
        Blocker::new(
            "docker_containerd_store_off",
            r#"Docker isn't using the containerd image store, which Ployz needs to build and move images between Servers. Enable it in /etc/docker/daemon.json ("features": {"containerd-snapshotter": true}) and restart Docker, or uninstall Docker and run this again."#,
        )
    })
}

fn zfs_eligibility(facts: &HostFacts) -> ZfsEligibility {
    let route = match zfs_route(facts) {
        Ok(route) => route,
        Err(reason) => return ZfsEligibility::Ineligible { reason },
    };
    if let Some(reason) = root_reserve_blocker(facts.root_filesystem) {
        return ZfsEligibility::Ineligible { reason };
    }
    let mut targets = vec![ZfsTarget::MainDisk];
    targets.extend(
        facts
            .disks
            .iter()
            .flatten()
            .filter(|disk| matches!(disk.role, DiskRole::Empty))
            .filter_map(|disk| disk.by_id.clone())
            .map(|by_id| ZfsTarget::Disk { by_id }),
    );
    ZfsEligibility::Eligible {
        targets,
        prepare_minutes: route.prepare_minutes(),
    }
}

/// The ZFS route for this Machine, or the first reason it has none, in the installer's order.
pub(super) fn zfs_route(facts: &HostFacts) -> Result<ZfsRoute, Blocker> {
    let route = zfs_os_route(&facts.os)?;
    if let Some(reason) = secure_boot_blocker(route, &facts.os, facts.secure_boot) {
        return Err(reason);
    }
    virtualization_blocker(&facts.virt).map_or(Ok(route), Err)
}

pub(super) fn zfs_os_route(os: &OsRelease) -> Result<ZfsRoute, Blocker> {
    ZfsRoute::for_os(os).ok_or_else(|| {
        Blocker::new(
            "unsupported_os",
            format!(
                "Managed volumes need Ubuntu LTS, Debian 12–13 or Amazon Linux 2023; this Server runs {}. Use one of those, or add `--storage none`.",
                os.display()
            ),
        )
    })
}

/// Secure Boot stops only routes that build their own, unsigned module.
pub(super) fn secure_boot_blocker(
    route: ZfsRoute,
    os: &OsRelease,
    secure_boot: Option<bool>,
) -> Option<Blocker> {
    if !route.builds_module() {
        return None;
    }
    match secure_boot {
        Some(false) => None,
        Some(true) => Some(Blocker::new(
            "secure_boot",
            format!(
                "Secure Boot is on, so this Server can't load the ZFS module Ployz builds for {}. Turn Secure Boot off, use Ubuntu, or add `--storage none`.",
                os.name
            ),
        )),
        None => Some(Blocker::new(
            "secure_boot_unknown",
            format!(
                "Couldn't tell whether Secure Boot is on, and under it this Server can't load the ZFS module Ployz builds for {}. Turn Secure Boot off, use Ubuntu, or add `--storage none`.",
                os.name
            ),
        )),
    }
}

pub(super) fn virtualization_blocker(virt: &Virtualization) -> Option<Blocker> {
    match virt {
        Virtualization::OpenVz => Some(Blocker::new(
            "openvz",
            "OpenVZ does not allow this Machine to load the host ZFS kernel module",
        )),
        Virtualization::Lxc {
            privileged: Some(false),
        } => Some(Blocker::new(
            "unprivileged_lxc",
            "Unprivileged LXC does not allow this Machine to load the host ZFS kernel module",
        )),
        Virtualization::Lxc { privileged: None } => Some(Blocker::new(
            "lxc_privilege_unknown",
            "Couldn't read this LXC container's user namespace, so Ployz can't tell whether it may load the host ZFS kernel module",
        )),
        Virtualization::BareMetal
        | Virtualization::Vm { .. }
        | Virtualization::Lxc {
            privileged: Some(true),
        }
        | Virtualization::Container { .. }
        | Virtualization::Unknown => None,
    }
}

pub(super) fn root_reserve_blocker(root: Filesystem) -> Option<Blocker> {
    let Filesystem {
        size_bytes,
        available_bytes,
    } = root;
    let reserve = ployz_core::storage_host_reserve(size_bytes);
    (available_bytes < reserve.saturating_add(ZFS_SMOKE_BYTES)).then(|| {
        Blocker::new(
            ROOT_RESERVE,
            format!(
                "Host root has {available_bytes} bytes available; ZFS validation needs {ZFS_SMOKE_BYTES} bytes while preserving the {reserve}-byte host-root reserve"
            ),
        )
    })
}

#[cfg(test)]
#[path = "assess_tests.rs"]
mod tests;
