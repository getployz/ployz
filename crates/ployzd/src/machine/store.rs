//! Durable Local Machine record ownership and persistence.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    net::IpAddr,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use ployz_core::{
    CloudPairing, LocalMachinePhase, Machine, MachineId, MachineUpdate, MachineUpdateError,
    SelectedEndpoint, apply_machine_update,
};
use thiserror::Error;

use super::{
    FoundingCluster, LocalMachineBody, LocalMachineRecord, ParticipationOrigin, local_runtime,
};
use crate::network::{WireGuardPrivateKey, allocate_machine_subnet};

const STATE_FILE_NAME: &str = "machine.json";
const TEMPORARY_FILE_NAME: &str = ".machine.json.tmp";
pub(super) const PENDING_RESET_FILE_NAME: &str = ".machine.reset.pending";
const LOCK_FILE_NAME: &str = ".ployzd.lock";

pub struct LocalMachineStore {
    pub(super) data_dir: PathBuf,
    pub(super) run_dir: PathBuf,
    record: LocalMachineRecord,
    _lock: File,
    // Lock order: admission, then publication/ingress/Docker, then short store locks.
    // Reset waits for admitted operations, including Docker streams with no total deadline.
    // ponytail: serialize local creates; use shared admission reads if throughput requires it.
    pub(super) admission_lock: Arc<tokio::sync::Mutex<()>>,
    pub(super) mutation_gate: crate::mutation::MutationGate,
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
        let data_dir = data_dir.as_ref();
        Self::open_with_admission(data_dir, data_dir.join(".run"))
    }

    pub(crate) fn open_with_admission(
        data_dir: impl AsRef<Path>,
        run_dir: impl AsRef<Path>,
    ) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref().to_owned();
        let run_dir = run_dir.as_ref().to_owned();
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
                    body: LocalMachineBody::Uninitialized {
                        id: MachineId::random(),
                    },
                    wireguard_private_key: WireGuardPrivateKey::generate(),
                    wireguard_mtu: None,
                    cloud_pairing: None,
                    selected_endpoints: BTreeMap::new(),
                };
                save(&data_dir, &record)?;
                record
            }
            Err(error) => return Err(error.into()),
        };

        let mut store = Self {
            data_dir: data_dir.clone(),
            record,
            _lock: lock,
            admission_lock: Arc::new(tokio::sync::Mutex::new(())),
            run_dir: run_dir.clone(),
            mutation_gate: crate::mutation::MutationGate::new(&run_dir, &data_dir),
        };
        if store.record.phase() == LocalMachinePhase::Resetting {
            let data_dir = store.data_dir.clone();
            store.complete_reset()?;
            drop(store);
            return Self::open_with_admission(data_dir, run_dir);
        }
        store.refresh_runtime()?;
        Ok(store)
    }

    #[must_use]
    pub fn record(&self) -> &LocalMachineRecord {
        &self.record
    }

    fn refresh_runtime(&mut self) -> Result<(), StoreError> {
        let runtime = local_runtime();
        let stale = match &self.record.body {
            LocalMachineBody::Joining { machine, .. }
            | LocalMachineBody::Participating { machine, .. } => machine.runtime != runtime,
            LocalMachineBody::Uninitialized { .. } | LocalMachineBody::Resetting { .. } => {
                return Ok(());
            }
        };
        if !stale {
            return Ok(());
        }
        let mut refreshed = self.record.clone();
        match &mut refreshed.body {
            LocalMachineBody::Joining { machine, .. }
            | LocalMachineBody::Participating { machine, .. } => machine.runtime = runtime,
            LocalMachineBody::Uninitialized { .. } | LocalMachineBody::Resetting { .. } => {
                return Ok(());
            }
        }
        save(&self.data_dir, &refreshed)?;
        self.record = refreshed;
        Ok(())
    }

    pub fn begin_reset(&mut self) -> Result<(), StoreError> {
        let prepared = self.prepare_reset()?;
        prepared.commit(self)
    }

    pub(crate) fn prepare_reset(&self) -> Result<PreparedReset, StoreError> {
        if self.record.phase() == LocalMachinePhase::Resetting {
            return Err(StoreError::AlreadyResetting);
        }
        let resetting = self.record.clone().into_resetting();
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
        founding_cluster: FoundingCluster,
        public_ip: Option<IpAddr>,
        advertised_endpoints: Vec<ployz_core::AdvertisedEndpoint>,
        wireguard_mtu: Option<u32>,
        cloud_pairing: Option<CloudPairing>,
    ) -> Result<Machine, StoreError> {
        self.require_uninitialized()?;
        if advertised_endpoints.is_empty() {
            return Err(StoreError::MissingEndpoints);
        }
        let public_key = self.record.wireguard_private_key.public_key();
        let machine = Machine {
            id: self.record.id(),
            name,
            subnet: allocate_machine_subnet(founding_cluster.network, [])
                .map_err(|error| StoreError::InvalidNetwork(error.to_string()))?,
            public_key,
            public_ip,
            advertised_endpoints,
            runtime: local_runtime(),
        };
        let mut initialized = self.record.clone();
        initialized.body = LocalMachineBody::Participating {
            machine: machine.clone(),
            origin: ParticipationOrigin::Founder {
                cluster: founding_cluster,
            },
        };
        initialized.wireguard_mtu = wireguard_mtu;
        initialized.cloud_pairing = cloud_pairing;
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
        cloud_pairing: Option<CloudPairing>,
    ) -> Result<(), StoreError> {
        self.require_uninitialized()?;
        if visible_peers.is_empty() {
            return Err(StoreError::MissingPeers);
        }
        if self.record.wireguard_private_key.public_key() != assigned_machine.public_key {
            return Err(StoreError::KeyMismatch);
        }
        if assigned_machine.advertised_endpoints.is_empty() {
            return Err(StoreError::MissingEndpoints);
        }
        assigned_machine.runtime = local_runtime();
        let mut joining = self.record.clone();
        joining.body = LocalMachineBody::Joining {
            machine: assigned_machine,
            bootstrap: visible_peers,
            min_store_version: target_versions,
        };
        joining.wireguard_mtu = wireguard_mtu;
        joining.cloud_pairing = cloud_pairing;
        save(&self.data_dir, &joining)?;
        self.record = joining;
        Ok(())
    }

    pub fn update(
        &mut self,
        update: MachineUpdate,
        visible: &[Machine],
    ) -> Result<Machine, StoreError> {
        let mut record = self.record.clone();
        let LocalMachineBody::Participating { machine, .. } = &mut record.body else {
            return Err(StoreError::NotParticipating);
        };
        let updated = apply_machine_update(machine, visible, update)?;
        *machine = updated.clone();
        save(&self.data_dir, &record)?;
        self.record = record;
        Ok(updated)
    }

    fn require_uninitialized(&self) -> Result<(), StoreError> {
        if matches!(self.record.body, LocalMachineBody::Uninitialized { .. }) {
            Ok(())
        } else {
            Err(StoreError::AlreadyInitialized)
        }
    }

    pub fn complete_catch_up(&mut self) -> Result<(), StoreError> {
        let mut participating = self.record.clone();
        let LocalMachineBody::Joining {
            machine, bootstrap, ..
        } = participating.body
        else {
            return Err(StoreError::NotJoining);
        };
        participating.body = LocalMachineBody::Participating {
            machine,
            origin: ParticipationOrigin::Join { bootstrap },
        };
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

    pub fn persist_cloud_pairing(
        &mut self,
        pairing: Option<CloudPairing>,
    ) -> Result<(), StoreError> {
        let mut updated = self.record.clone();
        updated.cloud_pairing = pairing;
        save(&self.data_dir, &updated)?;
        self.record = updated;
        Ok(())
    }

    pub fn complete_reset(&self) -> Result<(), StoreError> {
        if self.record.phase() != LocalMachinePhase::Resetting {
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

fn display_data_directory(path: &Path) -> String {
    let mut text = String::new();
    for chunk in path.as_os_str().as_encoded_bytes().utf8_chunks() {
        text.extend(chunk.valid().escape_debug());
        text.extend(chunk.invalid().escape_ascii().map(char::from));
    }
    text
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("local Machine record I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("local Machine record JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("machine is not participating")]
    NotParticipating,
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
    #[error("assigned public key does not match this Machine")]
    KeyMismatch,
    #[error("invalid Cluster network: {0}")]
    InvalidNetwork(String),
    #[error(transparent)]
    MachineUpdate(#[from] MachineUpdateError),
    #[error("another daemon already owns data directory {}", display_data_directory(.0))]
    AlreadyRunning(PathBuf),
    #[error("refusing to clear broad data directory {}", display_data_directory(.0))]
    UnsafeDataDirectory(PathBuf),
    #[error("refusing to claim nonempty data directory {}", display_data_directory(.0))]
    UnownedDataDirectory(PathBuf),
    #[error("local Machine record changed before clearing data directory {}", display_data_directory(.0))]
    OwnershipLost(PathBuf),
    #[error("local Machine record changed before prepared reset was committed in {}", display_data_directory(.0))]
    ResetPreparationLost(PathBuf),
}
