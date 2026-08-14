use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use ipnet::Ipv4Net;
use ployz_core::{
    LocalMachinePhase, Machine, MachineId, MachineRuntime, MachineUpdate, MachineUpdateError,
    SelectedEndpoint, apply_machine_update,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::network::WireGuardPrivateKey;
use crate::network::{allocate_machine_subnet, management_address};

pub const DEFAULT_DATA_DIR: &str = "/var/lib/ployz";
const STATE_FILE_NAME: &str = "machine.json";
const TEMPORARY_FILE_NAME: &str = ".machine.json.tmp";
const PENDING_RESET_FILE_NAME: &str = ".machine.reset.pending";
const LOCK_FILE_NAME: &str = ".ployzd.lock";
const DOCKER_VERSION_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocalMachineRecord {
    pub id: MachineId,
    pub phase: LocalMachinePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<Machine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard_private_key: Option<WireGuardPrivateKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard_mtu: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_network: Option<Ipv4Net>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bootstrap_machines: Vec<Machine>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub selected_endpoints: BTreeMap<MachineId, SelectedEndpoint>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub min_store_version: BTreeMap<String, i64>,
}

pub struct LocalMachineStore {
    data_dir: PathBuf,
    record: LocalMachineRecord,
    _lock: File,
}

pub(crate) struct PreparedReset {
    data_dir: PathBuf,
    original: LocalMachineRecord,
    resetting: LocalMachineRecord,
}

impl PreparedReset {
    pub(crate) fn commit(self, store: &mut LocalMachineStore) -> Result<(), StoreError> {
        let mut current = store.record.clone();
        current
            .selected_endpoints
            .clone_from(&self.original.selected_endpoints);
        if store.data_dir != self.data_dir || current != self.original {
            return Err(StoreError::ResetPreparationLost(store.data_dir.clone()));
        }
        fs::rename(
            self.data_dir.join(PENDING_RESET_FILE_NAME),
            self.data_dir.join(STATE_FILE_NAME),
        )?;
        store.record = self.resetting.clone();
        if let Err(error) = File::open(&self.data_dir).and_then(|directory| directory.sync_all()) {
            eprintln!("failed to sync committed local Machine reset: {error}");
        }
        Ok(())
    }
}

impl Drop for PreparedReset {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.data_dir.join(PENDING_RESET_FILE_NAME));
    }
}

impl Drop for LocalMachineStore {
    fn drop(&mut self) {
        // A concurrently forked child can briefly inherit the flock before exec closes the fd.
        let _ = fs2::FileExt::unlock(&self._lock);
    }
}

impl LocalMachineStore {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref().to_owned();
        validate_data_dir(&data_dir)?;
        claim_data_dir(&data_dir)?;
        fs::create_dir_all(&data_dir)?;
        fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o711))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(data_dir.join(LOCK_FILE_NAME))?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                StoreError::AlreadyRunning(data_dir.clone())
            } else {
                StoreError::Io(error)
            }
        })?;
        let path = data_dir.join(STATE_FILE_NAME);
        let record = match fs::read(&path) {
            Ok(data) => serde_json::from_slice(&data)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::remove_file(data_dir.join(TEMPORARY_FILE_NAME)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                let record = LocalMachineRecord {
                    id: MachineId::random(),
                    phase: LocalMachinePhase::Uninitialized,
                    machine: None,
                    wireguard_private_key: Some(WireGuardPrivateKey::generate()),
                    wireguard_mtu: None,
                    cluster_network: None,
                    bootstrap_machines: Vec::new(),
                    selected_endpoints: BTreeMap::new(),
                    min_store_version: BTreeMap::new(),
                };
                save(&data_dir, &record)?;
                record
            }
            Err(error) => return Err(error.into()),
        };

        if matches!(record.phase, LocalMachinePhase::Unrecognized(_)) {
            return Err(StoreError::InvalidPhase);
        }
        if record
            .machine
            .as_ref()
            .is_some_and(|machine| machine.id != record.id)
        {
            return Err(StoreError::MachineIdMismatch);
        }

        let mut store = Self {
            data_dir,
            record,
            _lock: lock,
        };
        if store.record.phase == LocalMachinePhase::Resetting {
            let data_dir = store.data_dir.clone();
            store.complete_reset()?;
            drop(store);
            return Self::open(data_dir);
        }
        store.refresh_runtime()?;
        Ok(store)
    }

    #[must_use]
    pub fn record(&self) -> &LocalMachineRecord {
        &self.record
    }

    fn refresh_runtime(&mut self) -> Result<(), StoreError> {
        let Some(machine) = &self.record.machine else {
            return Ok(());
        };
        let runtime = local_runtime();
        if machine.runtime == runtime {
            return Ok(());
        }
        let mut refreshed = self.record.clone();
        refreshed
            .machine
            .as_mut()
            .expect("Machine presence was checked")
            .runtime = runtime;
        save(&self.data_dir, &refreshed)?;
        self.record = refreshed;
        Ok(())
    }

    pub fn begin_reset(&mut self) -> Result<(), StoreError> {
        let prepared = self.prepare_reset()?;
        prepared.commit(self)
    }

    pub(crate) fn prepare_reset(&self) -> Result<PreparedReset, StoreError> {
        if self.record.phase == LocalMachinePhase::Resetting {
            return Err(StoreError::AlreadyResetting);
        }
        let mut resetting = self.record.clone();
        resetting.phase = LocalMachinePhase::Resetting;
        write_record(&self.data_dir.join(PENDING_RESET_FILE_NAME), &resetting)?;
        File::open(&self.data_dir)?.sync_all()?;
        Ok(PreparedReset {
            data_dir: self.data_dir.clone(),
            original: self.record.clone(),
            resetting,
        })
    }

    pub fn initialize(
        &mut self,
        name: ployz_core::MachineName,
        cluster_network: Ipv4Net,
        advertised_endpoints: Vec<ployz_core::AdvertisedEndpoint>,
        wireguard_mtu: Option<u32>,
    ) -> Result<Machine, StoreError> {
        self.require_uninitialized()?;
        if advertised_endpoints.is_empty() {
            return Err(StoreError::MissingEndpoints);
        }
        let private_key = self
            .record
            .wireguard_private_key
            .clone()
            .ok_or(StoreError::MissingPrivateKey)?;
        let public_key = private_key.public_key();
        let machine = Machine {
            id: self.record.id.clone(),
            name,
            subnet: allocate_machine_subnet(cluster_network, [])
                .map_err(|error| StoreError::InvalidNetwork(error.to_string()))?,
            management_address: management_address(public_key),
            public_key,
            public_ip: None,
            advertised_endpoints,
            runtime: local_runtime(),
        };
        let mut initialized = self.record.clone();
        initialized.phase = LocalMachinePhase::Participating;
        initialized.machine = Some(machine.clone());
        initialized.wireguard_mtu = wireguard_mtu;
        initialized.cluster_network = Some(cluster_network);
        save(&self.data_dir, &initialized)?;
        self.record = initialized;
        Ok(machine)
    }

    pub fn join(
        &mut self,
        mut assigned_machine: Machine,
        visible_peers: Vec<Machine>,
        target_versions: BTreeMap<String, i64>,
        wireguard_mtu: Option<u32>,
    ) -> Result<(), StoreError> {
        self.require_uninitialized()?;
        if visible_peers.is_empty() {
            return Err(StoreError::MissingPeers);
        }
        let private_key = self
            .record
            .wireguard_private_key
            .clone()
            .ok_or(StoreError::MissingPrivateKey)?;
        if private_key.public_key() != assigned_machine.public_key {
            return Err(StoreError::KeyMismatch);
        }
        assigned_machine.runtime = local_runtime();
        let mut joining = self.record.clone();
        joining.id = assigned_machine.id.clone();
        joining.phase = LocalMachinePhase::Joining;
        joining.machine = Some(assigned_machine);
        joining.wireguard_mtu = wireguard_mtu;
        joining.bootstrap_machines = visible_peers;
        joining.min_store_version = target_versions;
        save(&self.data_dir, &joining)?;
        self.record = joining;
        Ok(())
    }

    pub fn update(
        &mut self,
        update: MachineUpdate,
        visible: &[Machine],
    ) -> Result<Machine, StoreError> {
        if self.record.phase != LocalMachinePhase::Participating {
            return Err(StoreError::NotParticipating);
        }
        let machine = self
            .record
            .machine
            .as_ref()
            .ok_or(StoreError::InvalidPhase)?;
        let updated = apply_machine_update(machine, visible, update)?;
        let mut record = self.record.clone();
        record.machine = Some(updated.clone());
        save(&self.data_dir, &record)?;
        self.record = record;
        Ok(updated)
    }

    fn require_uninitialized(&self) -> Result<(), StoreError> {
        if self.record.phase == LocalMachinePhase::Uninitialized {
            Ok(())
        } else {
            Err(StoreError::AlreadyInitialized)
        }
    }

    pub fn complete_catch_up(&mut self) -> Result<(), StoreError> {
        if self.record.phase != LocalMachinePhase::Joining {
            return Err(StoreError::NotJoining);
        }
        let mut participating = self.record.clone();
        participating.phase = LocalMachinePhase::Participating;
        participating.min_store_version.clear();
        save(&self.data_dir, &participating)?;
        self.record = participating;
        Ok(())
    }

    pub fn persist_selected_endpoint(
        &mut self,
        machine_id: MachineId,
        endpoint: SelectedEndpoint,
    ) -> Result<(), StoreError> {
        let mut updated = self.record.clone();
        updated.selected_endpoints.insert(machine_id, endpoint);
        save(&self.data_dir, &updated)?;
        self.record = updated;
        Ok(())
    }

    pub fn complete_reset(&self) -> Result<(), StoreError> {
        if self.record.phase != LocalMachinePhase::Resetting {
            return Err(StoreError::NotResetting);
        }
        let persisted: LocalMachineRecord =
            serde_json::from_slice(&fs::read(self.data_dir.join(STATE_FILE_NAME))?)?;
        if persisted != self.record {
            return Err(StoreError::OwnershipLost(self.data_dir.clone()));
        }
        fs::remove_dir_all(&self.data_dir).map_err(StoreError::Io)
    }
}

#[must_use]
pub fn local_runtime() -> MachineRuntime {
    MachineRuntime {
        daemon_version: env!("CARGO_PKG_VERSION").into(),
        docker_version: docker_version(Path::new("docker"), DOCKER_VERSION_TIMEOUT),
        hostname: read_trimmed("/etc/hostname"),
        architecture: std::env::consts::ARCH.into(),
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

fn validate_data_dir(path: &Path) -> Result<(), StoreError> {
    if path.file_name().is_none()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(StoreError::UnsafeDataDirectory(path.to_owned()));
    }
    Ok(())
}

fn claim_data_dir(data_dir: &Path) -> Result<(), StoreError> {
    match fs::read_dir(data_dir) {
        Ok(entries) => {
            let names = entries
                .map(|entry| Ok(entry?.file_name()))
                .collect::<io::Result<Vec<_>>>()?;
            let owns_directory = names.iter().any(|name| name == STATE_FILE_NAME)
                || names
                    .iter()
                    .all(|name| name == TEMPORARY_FILE_NAME || name == LOCK_FILE_NAME);
            if owns_directory {
                Ok(())
            } else {
                Err(StoreError::UnownedDataDirectory(data_dir.to_owned()))
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn save(data_dir: &Path, record: &LocalMachineRecord) -> Result<(), StoreError> {
    fs::create_dir_all(data_dir)?;
    fs::set_permissions(data_dir, fs::Permissions::from_mode(0o711))?;

    let path = data_dir.join(STATE_FILE_NAME);
    let temporary = data_dir.join(TEMPORARY_FILE_NAME);
    write_record(&temporary, record)?;
    fs::rename(temporary, path)?;
    File::open(data_dir)?.sync_all()?;
    Ok(())
}

fn write_record(path: &Path, record: &LocalMachineRecord) -> Result<(), StoreError> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer_pretty(&mut file, record)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("local Machine record I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("local Machine record JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("local Machine record contains an unrecognized phase")]
    InvalidPhase,
    #[error("machine is not participating")]
    NotParticipating,
    #[error("local Machine identity does not match its advertised record")]
    MachineIdMismatch,
    #[error("machine is already resetting")]
    AlreadyResetting,
    #[error("machine is not resetting")]
    NotResetting,
    #[error("machine is not joining")]
    NotJoining,
    #[error("machine is already initialized")]
    AlreadyInitialized,
    #[error("at least one advertised endpoint is required")]
    MissingEndpoints,
    #[error("at least one visible peer is required to join")]
    MissingPeers,
    #[error("local WireGuard private key is missing")]
    MissingPrivateKey,
    #[error("assigned public key does not match this Machine")]
    KeyMismatch,
    #[error("invalid Cluster network: {0}")]
    InvalidNetwork(String),
    #[error(transparent)]
    MachineUpdate(#[from] MachineUpdateError),
    #[error("another daemon already owns data directory {0:?}")]
    AlreadyRunning(PathBuf),
    #[error("refusing to clear broad data directory {0:?}")]
    UnsafeDataDirectory(PathBuf),
    #[error("refusing to claim nonempty data directory {0:?}")]
    UnownedDataDirectory(PathBuf),
    #[error("local Machine record changed before clearing data directory {0:?}")]
    OwnershipLost(PathBuf),
    #[error("local Machine record changed before prepared reset was committed in {0:?}")]
    ResetPreparationLost(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_reset_does_not_change_phase_until_commit() {
        let data_dir = std::env::temp_dir().join(format!(
            "ployzd-prepared-reset-{}",
            ployz_core::MachineId::random()
        ));
        let mut store = LocalMachineStore::open(&data_dir).unwrap();
        let prepared = store.prepare_reset().unwrap();

        assert_eq!(store.record().phase, LocalMachinePhase::Uninitialized);
        assert!(data_dir.join(PENDING_RESET_FILE_NAME).exists());
        drop(prepared);
        assert!(!data_dir.join(PENDING_RESET_FILE_NAME).exists());

        let missing = store.prepare_reset().unwrap();
        fs::remove_file(data_dir.join(PENDING_RESET_FILE_NAME)).unwrap();
        assert!(missing.commit(&mut store).is_err());
        assert_eq!(store.record().phase, LocalMachinePhase::Uninitialized);

        let prepared = store.prepare_reset().unwrap();
        store
            .persist_selected_endpoint(
                MachineId::random(),
                SelectedEndpoint(std::net::SocketAddr::from(([192, 0, 2, 4], 51820))),
            )
            .unwrap();
        prepared.commit(&mut store).unwrap();
        assert_eq!(store.record().phase, LocalMachinePhase::Resetting);
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
