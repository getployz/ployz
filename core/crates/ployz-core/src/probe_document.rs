//! The host probe document: what `ployzd install --probe --json` prints and the check-in reads.
//!
//! ployzd writes it and every CLI release reads it, and the two can differ by a release, so its
//! shape only grows. Every field a reader needs to run keeps its place, and a field that only
//! some release writes is optional or defaulted here, so a document from any release reads.
//! A closed enum reads only the variants it names: a probe that prints a new one is a probe this
//! release can't read. A blocker code is open: it keeps the text of one this release doesn't name.

use serde::{Deserialize, Serialize};

use crate::value::open_string_enum;

/// `{ assessment, facts }`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProbeDocument {
    pub facts: HostFacts,
    pub assessment: Assessment,
}

impl ProbeDocument {
    /// The document as ployzd prints it. It goes through a JSON value so that every object's keys
    /// print sorted, as they always have.
    ///
    /// # Errors
    ///
    /// Fails when a fact can't be written as JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&serde_json::to_value(self)?)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HostFacts {
    pub os: OsRelease,
    pub kernel: String,
    pub arch: String,
    pub virt: Virtualization,
    /// `None` when the EFI variable couldn't be read.
    pub secure_boot: Option<bool>,
    pub cgroup: Option<CgroupVersion>,
    #[serde(default)]
    pub systemd: bool,
    pub cpu_count: Option<u32>,
    pub memory_total_bytes: Option<u64>,
    #[serde(default)]
    pub root_filesystem: Filesystem,
    /// The root disk and every whole disk with nothing on it; `None` when `lsblk` failed.
    pub disks: Option<Vec<Disk>>,
    pub docker: DockerFacts,
    /// `None` when the ZFS module is loaded but `zpool list` failed.
    pub zpool_present: Option<bool>,
    /// Why a fact above that could have been read is `null` or cut short.
    #[serde(default)]
    pub unread: Vec<String>,
}

/// The os-release fields that pick an install route and name the OS in a refusal.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OsRelease {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub version_id: Option<String>,
}

impl OsRelease {
    #[must_use]
    pub fn is_amazon_linux(&self) -> bool {
        self.id == "amzn"
    }

    /// `NAME VERSION_ID`, e.g. "Amazon Linux 2023"; rolling releases have no version.
    #[must_use]
    pub fn display(&self) -> String {
        match &self.version_id {
            Some(version) => format!("{} {version}", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Virtualization {
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CgroupVersion {
    V1,
    V2,
    Hybrid,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Filesystem {
    pub size_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Disk {
    pub path: String,
    pub by_id: Option<String>,
    pub size_bytes: u64,
    pub role: DiskRole,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DiskRole {
    Root { available: u64 },
    Empty,
}

/// Docker as the installer sees it. Reading stops at the first check Docker fails or can't
/// answer, so a later field stays `None` exactly when the installer never got to ask.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DockerFacts {
    Absent,
    /// Docker doesn't answer.
    Stopped,
    /// `docker.socket` listens while `docker.service` isn't running. The first `docker` command
    /// starts the daemon, so the probe asks nothing more and `unread` says why.
    SocketActivated,
    Present {
        version: String,
        api: String,
        buildx: Option<String>,
        containerd_store: Option<bool>,
        /// The failed command that stopped reading, as the installer would refuse with it.
        error: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Assessment {
    pub blockers: Vec<Blocker>,
    pub warnings: Vec<Blocker>,
    pub zfs: ZfsEligibility,
}

open_string_enum!(BlockerCode, Unknown {
    DockerStopped => "docker_stopped",
    DockerTooOld => "docker_too_old",
    DockerUnreadable => "docker_unreadable",
    DockerContainerdStoreOff => "docker_containerd_store_off",
    BuildxTooOld => "buildx_too_old",
    NoSystemd => "no_systemd",
    DiskTooSmall => "disk_too_small",
    CgroupV1 => "cgroup_v1",
    UnsupportedOs => "unsupported_os",
    SecureBoot => "secure_boot",
    SecureBootUnknown => "secure_boot_unknown",
    OpenVz => "openvz",
    UnprivilegedLxc => "unprivileged_lxc",
    LxcPrivilegeUnknown => "lxc_privilege_unknown",
    RootReserve => "root_reserve",
    DeviceNotEmpty => "device_not_empty",
});

/// A stable `code` for machines and an English `message` for people.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Blocker {
    pub code: BlockerCode,
    pub message: String,
}

impl Blocker {
    #[must_use]
    pub fn new(code: BlockerCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ZfsEligibility {
    Eligible {
        targets: Vec<ZfsTarget>,
        prepare_minutes: u32,
    },
    Ineligible {
        reason: Blocker,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ZfsTarget {
    MainDisk,
    Disk { by_id: String },
}

#[cfg(test)]
#[path = "probe_document_tests.rs"]
mod tests;
