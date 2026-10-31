//! Local Machine record persistence and Live Observation operations.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use ipnet::Ipv4Net;
use ployz_core::{
    DeviceCode, DeviceKey, LocalMachinePhase, Machine, MachineId, MachineRuntime,
    MachineStorageObservation, ManagementClientLabel, PinnedAddresses, RecoverySeed,
    SelectedEndpoint,
};
use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::machine_pool;
use crate::management::ManagementSecret;
use crate::network::WireGuardPrivateKey;

mod record_wire;
use record_wire::LocalMachineRecordWire;

mod ingress;
mod local_machine;
mod record_owner;
mod store;

pub use record_owner::{RecordOwner, RecordOwnerStopped};
#[cfg(test)]
use store::PENDING_RESET_FILE_NAME;
pub use store::{EnrollPlan, LocalMachineStore, StoreError};
pub(crate) use store::{InterruptedReset, Opened};

#[cfg(test)]
pub(crate) use local_machine::PeerInspect;
pub(crate) use local_machine::{CutRefusal, RecoveryError, RuntimeWatchTelemetry, SealingError};
#[cfg(test)]
pub(crate) use local_machine::{EnsurePause, EnsurePausePoint};
pub use local_machine::{Error as LocalMachineError, LocalMachine};

#[cfg(test)]
mod register_tests;

pub const DEFAULT_DATA_DIR: &str = "/var/lib/ployz";
pub const PREPARE_DIR_NAME: &str = "prepare";
const DOCKER_VERSION_TIMEOUT: Duration = Duration::from_secs(2);
const STORAGE_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(2);

/// Founder-only seed for immutable replicated Cluster values.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FoundingCluster {
    /// Cluster IPv4 pool selected by the founder.
    pub network: Ipv4Net,
    /// The Organization's Sealing Key this founder recovers rather than generates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<RecoverySeed>,
}

/// Whether a participating Machine founded the Cluster or joined known peers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum ParticipationOrigin {
    /// This Machine founded the Cluster with these immutable values.
    Founder {
        /// Cluster values selected by this founder.
        cluster: FoundingCluster,
    },
    /// This Machine joined through these bootstrap Machines.
    Join {
        /// Machines retained as bootstrap peers.
        bootstrap: Vec<Machine>,
    },
}

impl ParticipationOrigin {
    fn cluster_network(&self) -> Option<Ipv4Net> {
        match self {
            Self::Founder { cluster } => Some(cluster.network),
            Self::Join { .. } => None,
        }
    }

    fn recovery_seed(&self) -> Option<&RecoverySeed> {
        match self {
            Self::Founder { cluster } => cluster.recovery.as_ref(),
            Self::Join { .. } => None,
        }
    }

    fn bootstrap(&self) -> &[Machine] {
        match self {
            Self::Founder { .. } => &[],
            Self::Join { bootstrap } => bootstrap,
        }
    }
}

/// On-disk Local Machine Phase. Each variant owns only that phase's fields.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum LocalMachineBody {
    Uninitialized {
        id: MachineId,
    },
    Joining {
        machine: Machine,
        bootstrap: Vec<Machine>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        min_store_version: BTreeMap<String, i64>,
    },
    Participating {
        machine: Machine,
        /// Founder or join authority, as one non-contradictory value.
        #[serde(flatten)]
        origin: ParticipationOrigin,
    },
    Resetting {
        prior: Box<LocalMachinePrior>,
    },
}

/// Live body wrapped by Resetting. Cannot itself be Resetting.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum LocalMachinePrior {
    Uninitialized {
        id: MachineId,
    },
    Joining {
        machine: Machine,
        bootstrap: Vec<Machine>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        min_store_version: BTreeMap<String, i64>,
    },
    Participating {
        machine: Machine,
        /// Founder or join authority, as one non-contradictory value.
        #[serde(flatten)]
        origin: ParticipationOrigin,
    },
}

/// Persisted local Machine record: a phase-tagged body plus the key material
/// every phase requires.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "LocalMachineRecordWire")]
pub struct LocalMachineRecord {
    /// Phase-tagged lifecycle body.
    body: LocalMachineBody,
    /// WireGuard private key; required in every phase.
    wireguard_private_key: WireGuardPrivateKey,
    /// Management Identity secret; minted when the record is born.
    management_secret: ManagementSecret,
    /// Management Client slots by label. An absent label has no slot. Always
    /// written, even when empty, because readers require it.
    management_clients: BTreeMap<ManagementClientLabel, ManagementClientSlot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard_mtu: Option<u32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub selected_endpoints: BTreeMap<MachineId, SelectedEndpoint>,
    /// Always written, so only a record from 0.3.0-beta.7 or earlier lacks it.
    pinned_addresses: PinnedAddresses,
    /// Where the observe process pushes crash events. Absent means buffer only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    push_target: Option<Url>,
    /// Whether every live `device-` slot was written through Device approval. Absent on
    /// records from before Devices were Full-only, when any client could name one.
    #[serde(default)]
    device_slots_vetted: bool,
}

/// Public keys held by one Management Client slot. No secret is persisted.
///
/// Unlike the rest of the record this stays strict: an unrecognized field here
/// is key or secret material this daemon cannot judge, so the record fails closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum ManagementClientSlot {
    /// Tombstone left by Clear, keeping the cleared slot's public keys. They are
    /// never admitted again, so a redial with one learns its removal was confirmed.
    Cleared { was: LiveSlot },
    #[serde(untagged)]
    Live(LiveSlot),
}

/// A slot whose keys the management transport admits.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum LiveSlot {
    Pending {
        pending: [u8; 32],
    },
    Active {
        accepted: [u8; 32],
    },
    Rotating {
        accepted: [u8; 32],
        pending: [u8; 32],
    },
}

impl LiveSlot {
    fn accepted(self) -> Option<[u8; 32]> {
        match self {
            Self::Active { accepted } | Self::Rotating { accepted, .. } => Some(accepted),
            Self::Pending { .. } => None,
        }
    }

    fn pending(self) -> Option<[u8; 32]> {
        match self {
            Self::Pending { pending } | Self::Rotating { pending, .. } => Some(pending),
            Self::Active { .. } => None,
        }
    }

    fn holds(self, remote: &[u8; 32]) -> bool {
        self.accepted().as_ref() == Some(remote) || self.pending().as_ref() == Some(remote)
    }
}

impl ManagementClientSlot {
    fn live(self) -> Option<LiveSlot> {
        match self {
            Self::Live(slot) => Some(slot),
            Self::Cleared { .. } => None,
        }
    }

    fn accepted(self) -> Option<[u8; 32]> {
        self.live().and_then(LiveSlot::accepted)
    }

    fn pending(self) -> Option<[u8; 32]> {
        self.live().and_then(LiveSlot::pending)
    }

    /// Whether this tombstone holds `remote`.
    fn clears(self, remote: &[u8; 32]) -> bool {
        matches!(self, Self::Cleared { was } if was.holds(remote))
    }
}

impl LocalMachineRecord {
    /// Whether any Management Client slot holds an accepted or pending key.
    #[must_use]
    pub fn has_management_clients(&self) -> bool {
        self.management_clients().next().is_some()
    }

    /// Labels of the Management Client slots holding an accepted or pending key;
    /// Cleared tombstones are not listed.
    pub fn management_clients(&self) -> impl Iterator<Item = &ManagementClientLabel> {
        self.management_clients
            .iter()
            .filter(|(_, slot)| slot.live().is_some())
            .map(|(label, _)| label)
    }

    /// Keys of the approved Devices: accepted slots whose `device-` label names their key.
    pub fn devices(&self) -> impl Iterator<Item = DeviceKey> + '_ {
        self.management_clients.iter().filter_map(|(label, slot)| {
            let key = DeviceKey::from_bytes(slot.accepted()?);
            (DeviceCode::named_by(label) == Some(key.code())).then_some(key)
        })
    }

    /// Whether `remote` is a key some Clear left in a tombstone.
    #[must_use]
    pub fn clears_management_client(&self, remote: &[u8; 32]) -> bool {
        self.management_clients
            .values()
            .any(|slot| slot.clears(remote))
    }

    /// Public key `label` currently has accepted by the management transport.
    #[must_use]
    pub fn accepted_client(&self, label: &ManagementClientLabel) -> Option<[u8; 32]> {
        self.management_clients
            .get(label)
            .and_then(|slot| slot.accepted())
    }

    /// Public key `label` has awaiting an authenticated handover.
    #[must_use]
    pub fn pending_client(&self, label: &ManagementClientLabel) -> Option<[u8; 32]> {
        self.management_clients
            .get(label)
            .and_then(|slot| slot.pending())
    }

    /// Whether `remote` is any slot's accepted key, so it may run operational RPCs.
    #[must_use]
    pub fn accepts_management_client(&self, remote: &[u8; 32]) -> bool {
        self.management_clients
            .values()
            .any(|slot| slot.accepted().as_ref() == Some(remote))
    }

    /// Label of the slot holding `remote` as its pending key.
    #[must_use]
    pub fn pending_label(&self, remote: &[u8; 32]) -> Option<&ManagementClientLabel> {
        self.management_clients
            .iter()
            .find(|(_, slot)| slot.pending().as_ref() == Some(remote))
            .map(|(label, _)| label)
    }

    /// Label of the slot holding `remote` as its accepted key.
    #[must_use]
    pub fn accepted_label(&self, remote: &[u8; 32]) -> Option<&ManagementClientLabel> {
        self.management_clients
            .iter()
            .find(|(_, slot)| slot.accepted().as_ref() == Some(remote))
            .map(|(label, _)| label)
    }

    /// What `remote` may call: the widest scope of any slot holding it as its accepted
    /// or pending key, or `None` when no slot holds it.
    #[must_use]
    pub fn management_scope(&self, remote: &[u8; 32]) -> Option<ployz_core::ManagementScope> {
        self.management_clients
            .iter()
            .filter(|(_, slot)| {
                slot.accepted().as_ref() == Some(remote) || slot.pending().as_ref() == Some(remote)
            })
            .map(|(label, _)| ployz_core::ManagementScope::of(label, remote))
            .max()
    }

    /// Whether `remote` is any slot's accepted or pending key, so it may connect.
    #[must_use]
    pub fn admits_management_client(&self, remote: &[u8; 32]) -> bool {
        self.accepts_management_client(remote) || self.pending_label(remote).is_some()
    }
}

static EMPTY_STORE_VERSION: BTreeMap<String, i64> = BTreeMap::new();

impl LocalMachinePrior {
    fn id(&self) -> MachineId {
        match self {
            Self::Uninitialized { id } => *id,
            Self::Joining { machine, .. } | Self::Participating { machine, .. } => machine.id,
        }
    }

    fn machine(&self) -> Option<&Machine> {
        match self {
            Self::Uninitialized { .. } => None,
            Self::Joining { machine, .. } | Self::Participating { machine, .. } => Some(machine),
        }
    }

    fn cluster_network(&self) -> Option<Ipv4Net> {
        match self {
            Self::Participating { origin, .. } => origin.cluster_network(),
            Self::Uninitialized { .. } | Self::Joining { .. } => None,
        }
    }

    fn bootstrap(&self) -> &[Machine] {
        match self {
            Self::Uninitialized { .. } => &[],
            Self::Joining { bootstrap, .. } => bootstrap,
            Self::Participating { origin, .. } => origin.bootstrap(),
        }
    }

    fn min_store_version(&self) -> &BTreeMap<String, i64> {
        match self {
            Self::Joining {
                min_store_version, ..
            } => min_store_version,
            Self::Uninitialized { .. } | Self::Participating { .. } => &EMPTY_STORE_VERSION,
        }
    }
}

impl LocalMachineBody {
    fn id(&self) -> MachineId {
        match self {
            Self::Uninitialized { id } => *id,
            Self::Joining { machine, .. } | Self::Participating { machine, .. } => machine.id,
            Self::Resetting { prior } => prior.id(),
        }
    }

    fn phase(&self) -> LocalMachinePhase {
        match self {
            Self::Uninitialized { .. } => LocalMachinePhase::Uninitialized,
            Self::Joining { .. } => LocalMachinePhase::Joining,
            Self::Participating { .. } => LocalMachinePhase::Participating,
            Self::Resetting { .. } => LocalMachinePhase::Resetting,
        }
    }

    fn machine(&self) -> Option<&Machine> {
        match self {
            Self::Uninitialized { .. } => None,
            Self::Joining { machine, .. } | Self::Participating { machine, .. } => Some(machine),
            Self::Resetting { prior } => prior.machine(),
        }
    }

    fn cluster_network(&self) -> Option<Ipv4Net> {
        match self {
            Self::Participating { origin, .. } => origin.cluster_network(),
            Self::Resetting { prior } => prior.cluster_network(),
            Self::Uninitialized { .. } | Self::Joining { .. } => None,
        }
    }

    fn recovery_seed(&self) -> Option<&RecoverySeed> {
        match self {
            Self::Participating { origin, .. } => origin.recovery_seed(),
            Self::Uninitialized { .. } | Self::Joining { .. } | Self::Resetting { .. } => None,
        }
    }

    fn bootstrap(&self) -> &[Machine] {
        match self {
            Self::Uninitialized { .. } => &[],
            Self::Joining { bootstrap, .. } => bootstrap,
            Self::Participating { origin, .. } => origin.bootstrap(),
            Self::Resetting { prior } => prior.bootstrap(),
        }
    }

    fn min_store_version(&self) -> &BTreeMap<String, i64> {
        match self {
            Self::Joining {
                min_store_version, ..
            } => min_store_version,
            Self::Resetting { prior } => prior.min_store_version(),
            Self::Uninitialized { .. } | Self::Participating { .. } => &EMPTY_STORE_VERSION,
        }
    }

    fn into_prior(self) -> LocalMachinePrior {
        match self {
            Self::Uninitialized { id } => LocalMachinePrior::Uninitialized { id },
            Self::Joining {
                machine,
                bootstrap,
                min_store_version,
            } => LocalMachinePrior::Joining {
                machine,
                bootstrap,
                min_store_version,
            },
            Self::Participating { machine, origin } => {
                LocalMachinePrior::Participating { machine, origin }
            }
            Self::Resetting { .. } => {
                unreachable!("prepare_reset rejects an already resetting Machine")
            }
        }
    }
}

impl LocalMachineRecord {
    /// Admit a local lifecycle body whose advertised identity matches its key.
    ///
    /// # Errors
    ///
    /// Rejects mismatched keys, empty local endpoints, and empty join bootstrap peers,
    /// including a Resetting record's retained prior state.
    pub fn parse(
        body: LocalMachineBody,
        wireguard_private_key: WireGuardPrivateKey,
    ) -> Result<Self, StoreError> {
        if let Some(machine) = body.machine() {
            if machine.public_key != wireguard_private_key.public_key() {
                return Err(StoreError::KeyMismatch);
            }
            if machine.advertised_endpoints.is_empty() {
                return Err(StoreError::MissingEndpoints);
            }
            if body.cluster_network().is_none() && body.bootstrap().is_empty() {
                return Err(StoreError::MissingPeers);
            }
        }
        Ok(Self {
            body,
            wireguard_private_key,
            management_secret: ManagementSecret::generate(),
            management_clients: BTreeMap::new(),
            wireguard_mtu: None,
            selected_endpoints: BTreeMap::new(),
            pinned_addresses: PinnedAddresses::default(),
            push_target: None,
            device_slots_vetted: true,
        })
    }

    #[must_use]
    pub fn pinned_addresses(&self) -> PinnedAddresses {
        self.pinned_addresses
    }

    /// Where the observe process pushes crash events, if Cloud set one.
    #[must_use]
    pub fn push_target(&self) -> Option<&Url> {
        self.push_target.as_ref()
    }

    /// Secret behind this Machine's Management Identity.
    #[must_use]
    pub fn management_secret(&self) -> &ManagementSecret {
        &self.management_secret
    }

    /// Inspect phase-specific fields without changing lifecycle or key identity.
    #[must_use]
    pub fn body(&self) -> &LocalMachineBody {
        &self.body
    }

    /// Private material corresponding to the local advertised public key.
    #[must_use]
    pub fn private_key(&self) -> &WireGuardPrivateKey {
        &self.wireguard_private_key
    }

    /// Durable identity of this local Machine.
    #[must_use]
    pub fn id(&self) -> MachineId {
        self.body.id()
    }

    /// Local Machine Phase of this record.
    #[must_use]
    pub fn phase(&self) -> LocalMachinePhase {
        self.body.phase()
    }

    /// Advertised Machine when this phase owns one.
    #[must_use]
    pub fn machine(&self) -> Option<&Machine> {
        self.body.machine()
    }

    /// This Machine's own enrollment facts, as lifecycle refusals report them.
    #[must_use]
    pub fn now(&self) -> ployz_core::MachineNow {
        ployz_core::MachineNow {
            id: self.id(),
            phase: self.phase(),
            machine: self.machine().cloned(),
        }
    }

    /// Cluster IPv4 pool when this participating Machine has one.
    #[must_use]
    pub fn cluster_network(&self) -> Option<Ipv4Net> {
        self.body.cluster_network()
    }

    /// The Sealing Key this founder waits to recover, when it founded without one.
    #[must_use]
    pub fn recovery_seed(&self) -> Option<&RecoverySeed> {
        self.body.recovery_seed()
    }

    /// Bootstrap Machines persisted for join and catch-up.
    #[must_use]
    pub fn bootstrap(&self) -> &[Machine] {
        self.body.bootstrap()
    }

    /// Minimum store versions this joiner must reach before participating.
    #[must_use]
    pub fn min_store_version(&self) -> &BTreeMap<String, i64> {
        self.body.min_store_version()
    }

    fn into_resetting(self) -> Self {
        Self {
            body: LocalMachineBody::Resetting {
                prior: Box::new(self.body.into_prior()),
            },
            wireguard_private_key: self.wireguard_private_key,
            management_secret: self.management_secret,
            management_clients: self.management_clients,
            wireguard_mtu: self.wireguard_mtu,
            selected_endpoints: self.selected_endpoints,
            pinned_addresses: self.pinned_addresses,
            push_target: self.push_target,
            device_slots_vetted: self.device_slots_vetted,
        }
    }
}

#[must_use]
pub fn local_runtime(data_dir: &Path) -> MachineRuntime {
    let store_filesystem = store_filesystem(data_dir);
    MachineRuntime {
        daemon_version: env!("CARGO_PKG_VERSION").into(),
        docker_version: docker_version(Path::new("docker"), DOCKER_VERSION_TIMEOUT),
        hostname: read_trimmed("/etc/hostname"),
        // Kernel architecture preserves distinctions lost by Rust's ARCH,
        // including ppc64/ppc64le and ARM generations.
        architecture: nix::sys::utsname::uname()
            .map(|system| system.machine().to_string_lossy().into_owned())
            .unwrap_or_else(|_| std::env::consts::ARCH.into()),
        os_pretty_name: fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|contents| {
                contents.lines().find_map(|line| {
                    line.strip_prefix("PRETTY_NAME=")
                        .map(|value| value.trim_matches('"').to_owned())
                })
            })
            .unwrap_or_default(),
        kernel_version: read_trimmed("/proc/sys/kernel/osrelease"),
        memory_total_bytes: crate::host_capacity::memory_capacity()
            .ok()
            .map(|(total, _)| total),
        running_builds: 0,
        pool_committable_bytes: None,
        sealing: None,
        // Corrosion syncs each commit; on tmpfs a sync reaches only memory.
        store_syncs_commits: store_filesystem.as_deref().map(|name| name != "tmpfs"),
        store_filesystem,
        capabilities: ployz_core::served_capabilities(),
        management_identity: None,
        leaving: false,
    }
}

fn store_filesystem(data_dir: &Path) -> Option<String> {
    let store = crate::corrosion::CorrosionPaths::data_dir_under(data_dir);
    let stat = match rustix::fs::statfs(&store) {
        Err(rustix::io::Errno::NOENT) => rustix::fs::statfs(data_dir),
        stat => stat,
    };
    // Filesystem magic numbers are 32 bits; f_type's width and sign vary by target.
    stat.ok().map(|stat| filesystem_name(stat.f_type as u32))
}

fn filesystem_name(magic: u32) -> String {
    match magic {
        0x2fc1_2fc1 => "zfs".into(),
        0x9123_683e => "btrfs".into(),
        0x0102_1994 => "tmpfs".into(),
        0x794c_7630 => "overlay".into(),
        0xef53 => "ext4".into(),
        0x5846_5342 => "xfs".into(),
        other => format!("{other:#010x}"),
    }
}

async fn local_storage(program: &Path, timeout: Duration) -> Option<MachineStorageObservation> {
    let mut command = tokio::process::Command::new(program);
    command
        .args([
            "list",
            "-Hp",
            "-o",
            "name,size,allocated,free,health,readonly",
        ])
        .kill_on_drop(true);
    match tokio::time::timeout(timeout, command.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            let output = String::from_utf8(output.stdout).ok()?;
            match machine_pool::one_usable(&output) {
                Ok(Some(pool)) => Some(MachineStorageObservation::Pool {
                    size_bytes: pool.size_bytes(),
                    used_bytes: pool.used_bytes(),
                    free_bytes: pool.free_bytes(),
                }),
                Ok(None) => Some(MachineStorageObservation::Ready),
                Err(_) => None,
            }
        }
        Ok(Err(error)) if error.kind() == io::ErrorKind::NotFound => {
            Some(MachineStorageObservation::Stateless)
        }
        Ok(Ok(_)) | Ok(Err(_)) | Err(_) => None,
    }
}

fn docker_version(program: &Path, timeout: Duration) -> String {
    let output_path =
        std::env::temp_dir().join(format!(".ployzd-docker-version-{}", MachineId::random()));
    let Ok(mut output) = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&output_path)
    else {
        return String::new();
    };

    let version = (|| {
        let stdout = output.try_clone().ok()?;
        let mut child = Command::new(program)
            .args(["version", "--format", "{{.Server.Version}}"])
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        };
        if !status.success() {
            return None;
        }
        output.seek(SeekFrom::Start(0)).ok()?;
        let mut version = String::new();
        output.read_to_string(&mut version).ok()?;
        Some(version.trim().to_owned())
    })()
    .unwrap_or_default();

    drop(output);
    let _ = fs::remove_file(output_path);
    version
}

fn read_trimmed(path: &str) -> String {
    fs::read_to_string(path)
        .map(|value| value.trim().to_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn filesystem_names_cover_the_ones_that_refuse_synchronous_writes() {
        assert_eq!(filesystem_name(0x2fc1_2fc1), "zfs");
        assert_eq!(filesystem_name(0x9123_683e), "btrfs");
        assert_eq!(filesystem_name(0x0102_1994), "tmpfs");
        assert_eq!(filesystem_name(0x794c_7630), "overlay");
        assert_eq!(filesystem_name(0xef53), "ext4");
        assert_eq!(filesystem_name(0x5846_5342), "xfs");
        assert_eq!(filesystem_name(0x1234_abcd), "0x1234abcd");
        assert_eq!(filesystem_name(0x6969), "0x00006969");
    }

    #[test]
    fn store_filesystem_is_named_before_and_after_the_store_exists() {
        let root = tempfile::tempdir().unwrap();
        let expected = filesystem_name(rustix::fs::statfs(root.path()).unwrap().f_type as u32);
        assert_eq!(store_filesystem(root.path()), Some(expected.clone()));
        fs::create_dir(root.path().join("corrosion")).unwrap();
        assert_eq!(store_filesystem(root.path()), Some(expected));
        assert_eq!(store_filesystem(&root.path().join("missing")), None);
    }

    #[tokio::test]
    async fn served_capabilities_match_a_contract_with_every_group() {
        use ployz_core::{MachineRpc, op};

        let root = std::env::temp_dir().join(format!(
            "ployzd-served-capabilities-{}",
            ployz_core::MachineId::random()
        ));
        let store =
            RecordOwner::spawn(LocalMachineStore::open(root.join("data")).unwrap()).unwrap();
        let (containers, _) =
            crate::docker::test_support::fake_runtime_with(Default::default()).await;
        let (replicated, server) = crate::corrosion::fake_cluster::store().await;
        let contract = crate::machine_api::MachineService::with_cluster(
            store,
            Some((
                replicated,
                crate::corrosion::AdminClient::new("/no/such/admin.sock"),
            )),
        )
        .with_optional_containers(Some(containers))
        .describe_contract(tonic::Request::new(
            op::DescribeContract::into_request(ployz_core::DescribeContractRequest {})
                .encode()
                .unwrap(),
        ))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap()
        .decode::<op::DescribeContract>()
        .unwrap();
        server.abort();
        let _ = fs::remove_dir_all(root);

        let advertised: std::collections::BTreeSet<String> = contract
            .capabilities
            .iter()
            .map(|name| name.as_str().to_owned())
            .collect();
        assert_eq!(ployz_core::served_capabilities(), advertised);
    }

    #[tokio::test]
    async fn current_zpool_evidence_distinguishes_states_and_is_not_persisted() {
        let root = std::env::temp_dir().join(format!(
            "ployzd-storage-observation-{}",
            ployz_core::MachineId::random()
        ));
        fs::create_dir_all(&root).unwrap();
        let program = root.join("zpool");

        assert_eq!(
            local_storage(&program, Duration::from_secs(1)).await,
            Some(MachineStorageObservation::Stateless)
        );

        for (script, expected) in [
            ("#!/bin/sh\nexit 1\n", None),
            (
                "#!/bin/sh\nprintf 'tank\\t2147483648\\t1932735283\\t214748365\\tONLINE\\toff\\n'\n",
                Some(MachineStorageObservation::Pool {
                    size_bytes: std::num::NonZeroU64::new(2_147_483_648).unwrap(),
                    used_bytes: 1_932_735_283,
                    free_bytes: 214_748_365,
                }),
            ),
            (
                "#!/bin/sh\nprintf 'tank\\t4294967296\\t3865470566\\t429496730\\tONLINE\\toff\\n'\n",
                Some(MachineStorageObservation::Pool {
                    size_bytes: std::num::NonZeroU64::new(4_294_967_296).unwrap(),
                    used_bytes: 3_865_470_566,
                    free_bytes: 429_496_730,
                }),
            ),
            (
                "#!/bin/sh\nexit 0\n",
                Some(MachineStorageObservation::Ready),
            ),
            (
                "#!/bin/sh\nprintf 'tank\\t2147483648\\t0\\t2147483648\\tONLINE\\ton\\n'\n",
                None,
            ),
            (
                "#!/bin/sh\nprintf 'tank\\t2147483648\\t0\\t2147483648\\tFAULTED\\toff\\n'\n",
                None,
            ),
            (
                "#!/bin/sh\nprintf 'alpha\\t2147483648\\t0\\t2147483648\\tONLINE\\toff\\nbeta\\t2147483648\\t0\\t2147483648\\tDEGRADED\\toff\\n'\n",
                None,
            ),
        ] {
            fs::write(&program, script).unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(
                local_storage(&program, Duration::from_secs(1)).await,
                expected
            );
        }

        fs::write(&program, "#!/bin/sh\nwhile :; do :; done\n").unwrap();
        assert_eq!(
            local_storage(&program, Duration::from_millis(10)).await,
            None
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepared_reset_does_not_change_phase_until_commit() {
        let data_dir = std::env::temp_dir().join(format!(
            "ployzd-prepared-reset-{}",
            ployz_core::MachineId::random()
        ));
        let mut store = LocalMachineStore::open(&data_dir).unwrap();
        let prepared = store.prepare_reset().unwrap();

        assert_eq!(store.record().phase(), LocalMachinePhase::Uninitialized);
        assert!(data_dir.join(PENDING_RESET_FILE_NAME).exists());
        drop(prepared);
        assert!(!data_dir.join(PENDING_RESET_FILE_NAME).exists());

        let missing = store.prepare_reset().unwrap();
        fs::remove_file(data_dir.join(PENDING_RESET_FILE_NAME)).unwrap();
        assert!(missing.commit(&mut store).is_err());
        assert_eq!(store.record().phase(), LocalMachinePhase::Uninitialized);

        let prepared = store.prepare_reset().unwrap();
        store
            .persist_selected_endpoint(
                MachineId::random(),
                SelectedEndpoint(std::net::SocketAddr::from(([192, 0, 2, 4], 51820))),
            )
            .unwrap();
        prepared.commit(&mut store).unwrap();
        assert_eq!(store.record().phase(), LocalMachinePhase::Resetting);
        drop(store);
        fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn docker_runtime_probe_is_bounded() {
        let root = std::env::temp_dir().join(format!(
            "ployzd-docker-version-{}",
            ployz_core::MachineId::random()
        ));
        fs::create_dir_all(&root).unwrap();
        let program = root.join("docker");
        fs::write(&program, "#!/bin/sh\nexec sleep 30\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();

        let started = Instant::now();
        let version = docker_version(&program, Duration::from_millis(25));
        let elapsed = started.elapsed();
        fs::remove_dir_all(root).unwrap();

        assert!(version.is_empty());
        assert!(elapsed < Duration::from_secs(1), "probe took {elapsed:?}");
    }
}
