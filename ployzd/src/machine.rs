use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

use ipnet::Ipv4Net;
use ployz_core::{LocalMachinePhase, Machine, MachineId, SelectedEndpoint};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::network::WireGuardPrivateKey;
use crate::network::{allocate_machine_subnet, management_address};

pub const DEFAULT_DATA_DIR: &str = "/var/lib/ployz";
const STATE_FILE_NAME: &str = "machine.json";
const TEMPORARY_FILE_NAME: &str = ".machine.json.tmp";
const LOCK_FILE_NAME: &str = ".ployzd.lock";

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

        let store = Self {
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
        Ok(store)
    }

    #[must_use]
    pub fn record(&self) -> &LocalMachineRecord {
        &self.record
    }

    pub fn begin_reset(&mut self) -> Result<(), StoreError> {
        if self.record.phase == LocalMachinePhase::Resetting {
            return Err(StoreError::AlreadyResetting);
        }
        let mut resetting = self.record.clone();
        resetting.phase = LocalMachinePhase::Resetting;
        save(&self.data_dir, &resetting)?;
        self.record = resetting;
        Ok(())
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
            advertised_endpoints,
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
        assigned_machine: Machine,
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
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer_pretty(&mut file, record)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    File::open(data_dir)?.sync_all()?;
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
    #[error("another daemon already owns data directory {0:?}")]
    AlreadyRunning(PathBuf),
    #[error("refusing to clear broad data directory {0:?}")]
    UnsafeDataDirectory(PathBuf),
    #[error("refusing to claim nonempty data directory {0:?}")]
    UnownedDataDirectory(PathBuf),
    #[error("local Machine record changed before clearing data directory {0:?}")]
    OwnershipLost(PathBuf),
}
