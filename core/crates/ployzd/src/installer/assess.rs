//! What the host is, read once without changing it, and what Ployz can do on it.

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
use super::{Error, InstallPaths, command_exists, run_host};

const GIB: u64 = 1024 * 1024 * 1024;
const MINIMUM_DISK_BYTES: u64 = 30 * GIB;
/// The scratch file ZFS validation allocates on the host root.
pub(super) const ZFS_SMOKE_BYTES: u64 = 128 * 1024 * 1024;
pub(super) const ROOT_RESERVE: &str = "root_reserve";
const DOCKER_STAGE: &str = "checking Docker";
const ROOT_STAGE: &str = "inspect host-root capacity for ZFS storage preparation";

#[derive(Debug, Serialize)]
pub(super) struct HostFacts {
    pub(super) os: OsRelease,
    pub(super) kernel: String,
    pub(super) arch: String,
    pub(super) virt: Virtualization,
    /// `None` when neither the EFI variable nor `mokutil` could say.
    pub(super) secure_boot: Option<bool>,
    pub(super) cgroup: Option<CgroupVersion>,
    pub(super) systemd: bool,
    pub(super) cpu_count: Option<u32>,
    pub(super) memory_total_bytes: Option<u64>,
    pub(super) root_filesystem: Filesystem,
    /// The root disk and every whole disk with nothing on it; `None` when `lsblk` failed.
    pub(super) disks: Option<Vec<Disk>>,
    pub(super) docker: DockerFacts,
    pub(super) zpool_present: bool,
}

#[derive(Debug, Serialize)]
pub(super) enum Virtualization {
    BareMetal,
    Vm { kind: String },
    OpenVz,
    Lxc { privileged: bool },
    Container { kind: String },
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

/// Docker as the installer sees it. Reading stops at the first check Docker fails, so a later
/// field stays `None` exactly when the installer never asked for it.
#[derive(Debug, Serialize)]
pub(super) enum DockerFacts {
    Absent,
    Stopped,
    Present {
        version: String,
        api: String,
        buildx: Option<String>,
        containerd_store: Option<bool>,
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
        build_minutes: u32,
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

/// # Errors
///
/// Fails where the installer always failed: an unreadable os-release, root filesystem or LXC
/// user namespace, or a running Docker that won't report buildx or its storage driver.
pub(super) fn read_facts(paths: &InstallPaths) -> Result<HostFacts, Error> {
    let os = OsRelease::read(&paths.os_release)?;
    let uts = nix::sys::utsname::uname().map_err(|errno| Error::Io {
        stage: "read kernel and architecture",
        source: io::Error::from(errno),
    })?;
    let root_filesystem = Filesystem::read_root()?;
    let disks = read_disks(root_filesystem);
    Ok(HostFacts {
        os,
        kernel: uts.release().to_string_lossy().into_owned(),
        arch: uts.machine().to_string_lossy().into_owned(),
        virt: read_virtualization(&paths.proc_dir)?,
        secure_boot: read_secure_boot(&paths.secure_boot),
        cgroup: fs::read_to_string(paths.proc_dir.join("self/mounts"))
            .ok()
            .and_then(|mounts| cgroup_version(&mounts)),
        systemd: Path::new("/run/systemd/system").is_dir(),
        cpu_count: fs::read_to_string(paths.proc_dir.join("cpuinfo"))
            .ok()
            .map(|cpuinfo| {
                cpuinfo
                    .lines()
                    .filter(|line| line.split(':').next().map(str::trim) == Some("processor"))
                    .count()
            })
            .filter(|count| *count > 0)
            .and_then(|count| u32::try_from(count).ok()),
        memory_total_bytes: fs::read_to_string(paths.proc_dir.join("meminfo"))
            .ok()
            .and_then(|meminfo| {
                meminfo.lines().find_map(|line| {
                    line.strip_prefix("MemTotal:")?
                        .split_whitespace()
                        .next()?
                        .parse::<u64>()
                        .ok()
                })
            })
            .map(|kib| kib.saturating_mul(1024)),
        root_filesystem,
        disks,
        docker: read_docker()?,
        zpool_present: command_exists("zpool")
            && Command::new("zpool")
                .args(["list", "-H"])
                .output()
                .is_ok_and(|output| {
                    output.status.success() && !output.stdout.trim_ascii().is_empty()
                }),
    })
}

/// Container kinds `systemd-detect-virt` names; anything else it names is a VM.
const CONTAINERS: [&str; 8] = [
    "docker",
    "podman",
    "rkt",
    "systemd-nspawn",
    "lxc-libvirt",
    "proot",
    "pouch",
    "wsl",
];

fn read_virtualization(proc_dir: &Path) -> Result<Virtualization, Error> {
    // OpenVZ hosts expose /proc/bc; its containers have only /proc/vz.
    if proc_dir.join("vz").is_dir() && !proc_dir.join("bc").is_dir() {
        return Ok(Virtualization::OpenVz);
    }
    // It exits 1 when it prints `none`, so its output is the answer either way.
    let kind = Command::new("systemd-detect-virt")
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();
    Ok(match kind.as_str() {
        "" => Virtualization::Unknown,
        "none" => Virtualization::BareMetal,
        "openvz" => Virtualization::OpenVz,
        "lxc" => Virtualization::Lxc {
            privileged: !lxc_is_unprivileged(proc_dir)?,
        },
        kind if CONTAINERS.contains(&kind) => Virtualization::Container { kind: kind.into() },
        kind => Virtualization::Vm { kind: kind.into() },
    })
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

fn read_secure_boot(variable: &Path) -> Option<bool> {
    match fs::read(variable) {
        // The variable is 4 attribute bytes followed by one data byte; 1 means on.
        Ok(variable) => Some(variable.last() == Some(&1)),
        // A Machine without EFI variables boots with Secure Boot off.
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some(false),
        Err(_) if command_exists("mokutil") => {
            let output = Command::new("mokutil").arg("--sb-state").output().ok()?;
            let state = String::from_utf8_lossy(&output.stdout);
            if state.contains("SecureBoot enabled") {
                Some(true)
            } else if state.contains("SecureBoot disabled") {
                Some(false)
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

/// The cgroup layout from `/proc/self/mounts`.
fn cgroup_version(mounts: &str) -> Option<CgroupVersion> {
    let fstype = |target: &str| {
        mounts.lines().find_map(|line| {
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

fn read_disks(root: Filesystem) -> Option<Vec<Disk>> {
    let output = Command::new("lsblk")
        .args([
            "-J",
            "-b",
            "-o",
            "NAME,PATH,SIZE,TYPE,FSTYPE,MOUNTPOINTS,PKNAME",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let listing: Value = serde_json::from_slice(&output.stdout).ok()?;
    let by_id = disk_ids(Path::new("/dev/disk/by-id"));
    Some(disks_from_lsblk(&listing, root, &by_id))
}

/// The root disk and every whole disk with no partitions, holders, filesystem signature or mount.
pub(super) fn disks_from_lsblk(
    listing: &Value,
    root: Filesystem,
    by_id: &[(PathBuf, String)],
) -> Vec<Disk> {
    let devices = listing["blockdevices"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    devices
        .iter()
        .filter(|device| device["type"] == "disk")
        .filter_map(|device| {
            let path = device["path"].as_str()?;
            let role = if mounts_root(device) {
                DiskRole::Root {
                    available: root.available_bytes,
                }
            } else if device["fstype"].is_null()
                && device["children"].as_array().is_none_or(Vec::is_empty)
                && device["mountpoints"]
                    .as_array()
                    .is_none_or(|mounts| mounts.iter().all(Value::is_null))
            {
                DiskRole::Empty
            } else {
                return None;
            };
            Some(Disk {
                path: path.to_owned(),
                by_id: by_id
                    .iter()
                    .find(|(target, _)| target == Path::new(path))
                    .map(|(_, name)| name.clone()),
                size_bytes: match &device["size"] {
                    Value::String(size) => size.parse().ok()?,
                    Value::Number(size) => size.as_u64()?,
                    Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => {
                        return None;
                    }
                },
                role,
            })
        })
        .collect()
}

fn mounts_root(device: &Value) -> bool {
    device["mountpoints"]
        .as_array()
        .is_some_and(|mounts| mounts.iter().any(|mount| mount == "/"))
        || device["children"]
            .as_array()
            .is_some_and(|children| children.iter().any(mounts_root))
}

/// Each `/dev/disk/by-id` link's target with its name, the most stable name first per target.
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
    // WWN and EUI names are hex serials; the model-and-serial name is the one a person reads.
    ids.sort_by_key(|(_, name)| {
        (
            name.starts_with("wwn-") || name.starts_with("nvme-eui."),
            name.clone(),
        )
    });
    ids
}

/// # Errors
///
/// Fails when a running Docker won't report its buildx or storage driver.
pub(super) fn read_docker() -> Result<DockerFacts, Error> {
    if !command_exists("dockerd") {
        return Ok(DockerFacts::Absent);
    }
    // The CLI can't read the server's version only when it can't reach the daemon.
    let Ok(output) = docker_output(["version", "--format", "json"]) else {
        return Ok(DockerFacts::Stopped);
    };
    let Ok(version) = serde_json::from_str::<Value>(&output) else {
        return Ok(DockerFacts::Stopped);
    };
    let Some(server) = version.get("Server").filter(|server| server.is_object()) else {
        return Ok(DockerFacts::Stopped);
    };
    let field = |name: &str| server[name].as_str().unwrap_or_default().to_owned();
    let (version, api) = (field("Version"), field("ApiVersion"));
    if !at_least(&api, MINIMUM_DOCKER_API) {
        return Ok(DockerFacts::Present {
            version,
            api,
            buildx: None,
            containerd_store: None,
        });
    }
    // e.g. `github.com/docker/buildx v0.37.1 c8d4ec2`
    let output = docker_output(["buildx", "version"])?;
    let buildx = output
        .split_whitespace()
        .nth(1)
        .unwrap_or(&output)
        .to_owned();
    if !at_least(buildx.trim_start_matches('v'), MINIMUM_BUILDX) {
        return Ok(DockerFacts::Present {
            version,
            api,
            buildx: Some(buildx),
            containerd_store: None,
        });
    }
    let containerd_store = docker_output(["info", "-f", "{{.DriverStatus}}"])?
        .contains("io.containerd.snapshotter.v1");
    Ok(DockerFacts::Present {
        version,
        api,
        buildx: Some(buildx),
        containerd_store: Some(containerd_store),
    })
}

fn docker_output<const N: usize>(args: [&str; N]) -> Result<String, Error> {
    let output = run_host(DOCKER_STAGE, "docker", args)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Whether `version`'s first two numbers (`1.53`, `0.18.0+ds1`) are at least `minimum`.
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
            "This Server doesn't run systemd. Ployz needs systemd to run its daemon.",
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
                "Disk is {} GB. Ployz needs {} GB.",
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

/// The refusal for a Docker Ployz can't use, in the order the installer checks it.
pub(super) fn docker_blocker(docker: &DockerFacts) -> Option<Blocker> {
    let DockerFacts::Present {
        version,
        api,
        buildx,
        containerd_store,
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

/// Whether ZFS can be prepared here, refusing in the order the installer always has.
fn zfs_eligibility(facts: &HostFacts) -> ZfsEligibility {
    let ineligible = |code, message: String| ZfsEligibility::Ineligible {
        reason: Blocker::new(code, message),
    };
    let os = &facts.os;
    let Some(route) = ZfsRoute::for_os(os) else {
        return ineligible(
            "unsupported_os",
            format!(
                "Managed volumes need Ubuntu LTS, Debian 12–13 or Amazon Linux 2023; this Server runs {}. Use one of those, or add `--storage none`.",
                os.display()
            ),
        );
    };
    // Unsigned modules this Machine builds can't load under Secure Boot.
    if route.builds_module() {
        match facts.secure_boot {
            Some(false) => {}
            Some(true) => {
                return ineligible(
                    "secure_boot",
                    format!(
                        "Secure Boot is on, so this Server can't load the ZFS module Ployz builds for {}. Turn Secure Boot off, use Ubuntu, or add `--storage none`.",
                        os.name
                    ),
                );
            }
            None => {
                return ineligible(
                    "secure_boot_unknown",
                    format!(
                        "Couldn't tell whether Secure Boot is on, and under it this Server can't load the ZFS module Ployz builds for {}. Turn Secure Boot off, use Ubuntu, or add `--storage none`.",
                        os.name
                    ),
                );
            }
        }
    }
    match facts.virt {
        Virtualization::OpenVz => {
            return ineligible(
                "openvz",
                "OpenVZ does not allow this Machine to load the host ZFS kernel module".into(),
            );
        }
        Virtualization::Lxc { privileged: false } => {
            return ineligible(
                "unprivileged_lxc",
                "Unprivileged LXC does not allow this Machine to load the host ZFS kernel module"
                    .into(),
            );
        }
        Virtualization::BareMetal
        | Virtualization::Vm { .. }
        | Virtualization::Lxc { privileged: true }
        | Virtualization::Container { .. }
        | Virtualization::Unknown => {}
    }
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
        build_minutes: route.build_minutes(),
    }
}

/// The host-root reserve ZFS validation must leave untouched.
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
