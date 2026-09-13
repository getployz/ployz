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
    CloudPairing, LocalMachinePhase, Machine, MachineId, MachineRuntime, MachineStorageObservation,
    SelectedEndpoint,
};
use serde::{Deserialize, Serialize};

use crate::machine_pool;
use crate::network::WireGuardPrivateKey;

mod record_wire;
use record_wire::LocalMachineRecordWire;

mod ingress;
mod local_machine;
mod store;

#[cfg(test)]
use store::PENDING_RESET_FILE_NAME;
pub use store::{LocalMachineStore, StoreError};

pub use local_machine::{Error as LocalMachineError, LocalMachine};
pub(crate) use local_machine::{MutationAdmission, RuntimeWatchTelemetry};

#[cfg(test)]
mod register_tests;

pub const DEFAULT_DATA_DIR: &str = "/var/lib/ployz";
const DOCKER_VERSION_TIMEOUT: Duration = Duration::from_secs(2);
const STORAGE_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(2);

/// Founder-only seed for immutable replicated Cluster values.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FoundingCluster {
    /// Cluster IPv4 pool selected by the founder.
    pub network: Ipv4Net,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard_mtu: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_pairing: Option<CloudPairing>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub selected_endpoints: BTreeMap<MachineId, SelectedEndpoint>,
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
            wireguard_mtu: None,
            cloud_pairing: None,
            selected_endpoints: BTreeMap::new(),
        })
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

    /// Cluster IPv4 pool when this participating Machine has one.
    #[must_use]
    pub fn cluster_network(&self) -> Option<Ipv4Net> {
        self.body.cluster_network()
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
            wireguard_mtu: self.wireguard_mtu,
            cloud_pairing: self.cloud_pairing,
            selected_endpoints: self.selected_endpoints,
        }
    }
}

#[must_use]
pub fn local_runtime() -> MachineRuntime {
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
