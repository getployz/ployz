//! Durable machine-local evidence for resumable machine join.

use std::fmt;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use defguard_wireguard_rs::key::Key;
use ployz_core::ids::{ClusterId, MachineRowId};
use ployz_core::join::{
    JOIN_RESET_COMMAND, JoinDoorCertFingerprint, JoinMachineSubstrate, MachineJoinRequest,
};
use ployz_core::network::WireGuardPublicKey;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{FileMode, write_durable_file};

const IDENTITY_FILE: &str = "join-identity.json";
const REQUEST_FILE: &str = "join-request.json";
const STORAGE_INVENTORY_FILE: &str = "join-storage-inventory.json";
const ACCEPTANCE_FILE: &str = "join-acceptance.json";
const COMPLETE_FILE: &str = "join-complete";
const LOCK_FILE: &str = ".join.lock";
const MILESTONE_DIRECTORY: &str = "machine-join";
pub const JOIN_SUBSTRATE_FILE: &str = "join-substrate.json";
pub const JOIN_SUBSTRATE_ENV: &str = "PLOYZ_API_JOIN_SUBSTRATE_PATH";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineJoinStateDirectory(PathBuf);

impl MachineJoinStateDirectory {
    pub fn initialize(path: impl Into<PathBuf>) -> Result<Self, MachineJoinStateError> {
        let path = path.into();
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|error| state_error("create machine join state", error))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| state_error("protect machine join state", error))?;
        }
        Self::open(path)
    }

    pub fn initialize_host_default() -> Result<Self, MachineJoinStateError> {
        Self::initialize("/var/lib/ployz")
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self, MachineJoinStateError> {
        let path = path.into();
        let metadata = std::fs::metadata(&path)
            .map_err(|error| state_error("open machine join state", error))?;
        if !metadata.is_dir() {
            return Err(MachineJoinStateError::NotDirectory { path });
        }
        Ok(Self(path))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn try_lock(&self) -> Result<MachineJoinLock, MachineJoinStateError> {
        let path = self.0.join(LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| state_error("open machine join lock", error))?;
        match file.try_lock() {
            Ok(()) => Ok(MachineJoinLock { file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(MachineJoinStateError::JoinInProgress),
            Err(std::fs::TryLockError::Error(error)) => {
                Err(state_error("lock machine join state", error))
            }
        }
    }

    pub fn inspect(
        &self,
        requested: &JoinDoorCertFingerprint,
    ) -> Result<MachineJoinInspection, MachineJoinStateError> {
        if self.has_founding_evidence() {
            return Ok(refused());
        }
        let identity = self.read_identity()?;
        let accepted = self.acceptance_path().exists();
        let complete = self.0.join(COMPLETE_FILE).exists();
        let Some(identity) = identity else {
            if accepted
                || complete
                || self.0.join("cluster-id").exists()
                || door_material_exists(&self.0)
            {
                return Ok(refused());
            }
            return Ok(MachineJoinInspection::Clean);
        };
        if identity.door_fingerprint != *requested {
            return Ok(refused());
        }
        match (accepted, complete) {
            (false, false) => Ok(MachineJoinInspection::ReadyToRedeem { identity }),
            (true, false) => Ok(MachineJoinInspection::ReadyToActivate { identity }),
            (true, true) => Ok(MachineJoinInspection::NoOp { identity }),
            (false, true) => Ok(refused()),
        }
    }

    pub fn ensure_identity(
        &self,
        requested: &JoinDoorCertFingerprint,
    ) -> Result<MachineJoinIdentity, MachineJoinStateError> {
        match self.inspect(requested)? {
            MachineJoinInspection::ReadyToRedeem { identity } => return Ok(identity),
            MachineJoinInspection::Clean => {}
            MachineJoinInspection::ReadyToActivate { .. }
            | MachineJoinInspection::NoOp { .. }
            | MachineJoinInspection::Refused { .. } => {
                return Err(MachineJoinStateError::IdentityNotWritable);
            }
        }
        let persisted = PersistedMachineJoinIdentity {
            machine_id: MachineRowId::generate(),
            private_key: Key::generate().to_string(),
            door_fingerprint: requested.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&persisted)
            .map_err(|error| state_error("encode machine join identity", error))?;
        write_durable_file(&self.0, IDENTITY_FILE, FileMode::Secret0600, &bytes)
            .map_err(|error| state_error("persist machine join identity", error))?;
        self.read_identity()?
            .ok_or_else(|| MachineJoinStateError::Failed {
                message: "persisted machine join identity disappeared".to_owned(),
            })
    }

    pub(crate) fn persist_acceptance(
        &self,
        accepted: &impl Serialize,
    ) -> Result<(), MachineJoinStateError> {
        let bytes = serde_json::to_vec(accepted)
            .map_err(|error| state_error("encode machine join acceptance", error))?;
        match std::fs::read(self.acceptance_path()) {
            Ok(existing) if existing == bytes => return Ok(()),
            Ok(_) => return Err(MachineJoinStateError::AcceptanceAlreadyPersisted),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(state_error("read machine join acceptance", error)),
        }
        write_durable_file(&self.0, ACCEPTANCE_FILE, FileMode::Secret0600, &bytes)
            .map_err(|error| state_error("persist machine join acceptance", error))
    }

    pub(crate) fn persist_request(
        &self,
        request: &MachineJoinRequest,
    ) -> Result<(), MachineJoinStateError> {
        let bytes = serde_json::to_vec(request)
            .map_err(|error| state_error("encode machine join request", error))?;
        let path = self.0.join(REQUEST_FILE);
        match std::fs::read(&path) {
            Ok(existing) if existing == bytes => return Ok(()),
            Ok(_) => return Err(MachineJoinStateError::RequestAlreadyPersisted),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(state_error("read machine join request", error)),
        }
        write_durable_file(&self.0, REQUEST_FILE, FileMode::Secret0600, &bytes)
            .map_err(|error| state_error("persist machine join request", error))
    }

    pub(crate) fn read_request(&self) -> Result<Option<MachineJoinRequest>, MachineJoinStateError> {
        let bytes = match std::fs::read(self.0.join(REQUEST_FILE)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(state_error("read machine join request", error)),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| state_error("decode machine join request", error))
    }

    pub(crate) fn persist_storage_inventory(
        &self,
        inventory: &impl Serialize,
    ) -> Result<(), MachineJoinStateError> {
        let bytes = serde_json::to_vec(inventory)
            .map_err(|error| state_error("encode machine join storage inventory", error))?;
        let path = self.0.join(STORAGE_INVENTORY_FILE);
        match std::fs::read(&path) {
            Ok(existing) if existing == bytes => return Ok(()),
            Ok(_) => return Err(MachineJoinStateError::StorageInventoryAlreadyPersisted),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(state_error("read machine join storage inventory", error)),
        }
        write_durable_file(
            &self.0,
            STORAGE_INVENTORY_FILE,
            FileMode::Secret0600,
            &bytes,
        )
        .map_err(|error| state_error("persist machine join storage inventory", error))
    }

    pub(crate) fn read_storage_inventory<T: DeserializeOwned>(
        &self,
    ) -> Result<Option<T>, MachineJoinStateError> {
        let bytes = match std::fs::read(self.0.join(STORAGE_INVENTORY_FILE)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(state_error("read machine join storage inventory", error)),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| state_error("decode machine join storage inventory", error))
    }

    pub(crate) fn read_acceptance<T: DeserializeOwned>(
        &self,
    ) -> Result<Option<T>, MachineJoinStateError> {
        let bytes = match std::fs::read(self.acceptance_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(state_error("read machine join acceptance", error)),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| state_error("decode machine join acceptance", error))
    }

    /// Persists the accepted cluster identity after the full canonical acceptance.
    pub(crate) fn persist_cluster_anchor(
        &self,
        cluster_id: &ClusterId,
    ) -> Result<(), MachineJoinStateError> {
        let path = self.0.join("cluster-id");
        match std::fs::read_to_string(&path) {
            Ok(value) => {
                let persisted = ClusterId::try_new(value.trim_end_matches(['\r', '\n']))
                    .map_err(|error| state_error("decode joined cluster identity", error))?;
                if persisted == *cluster_id {
                    return Ok(());
                }
                return Err(MachineJoinStateError::ClusterAnchorMismatch);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(state_error("read joined cluster identity", error)),
        }
        crate::lifecycle::founding::FoundingStateDirectory::open(&self.0)
            .map_err(|error| state_error("open cluster identity state", error))?
            .persist_cluster_id_exclusive(cluster_id)
            .map_err(|error| state_error("persist joined cluster identity", error))
    }

    pub fn persist_join_substrate(
        &self,
        substrate: &JoinMachineSubstrate,
    ) -> Result<(), MachineJoinStateError> {
        let bytes = serde_json::to_vec(substrate)
            .map_err(|error| state_error("encode join substrate", error))?;
        let path = self.join_substrate_path();
        match std::fs::read(&path) {
            Ok(existing) if existing == bytes => return Ok(()),
            Ok(_) => return Err(MachineJoinStateError::SubstrateAlreadyPersisted),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(state_error("read join substrate", error)),
        }
        write_durable_file(&self.0, JOIN_SUBSTRATE_FILE, FileMode::Plain, &bytes)
            .map_err(|error| state_error("persist join substrate", error))
    }

    #[must_use]
    pub fn join_substrate_path(&self) -> PathBuf {
        self.0.join(JOIN_SUBSTRATE_FILE)
    }

    pub fn milestone_complete(
        &self,
        milestone: MachineJoinMilestone,
    ) -> Result<bool, MachineJoinStateError> {
        let path = self.milestone_path(milestone);
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => Ok(true),
            Ok(_) => Err(MachineJoinStateError::InvalidMilestone { path }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(state_error("read machine join milestone", error)),
        }
    }

    pub fn record_milestone(
        &self,
        milestone: MachineJoinMilestone,
    ) -> Result<(), MachineJoinStateError> {
        let directory = self.0.join(MILESTONE_DIRECTORY);
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder
            .create(&directory)
            .map_err(|error| state_error("create machine join milestone directory", error))?;
        write_durable_file(
            &directory,
            milestone.file_name(),
            FileMode::Plain,
            b"complete\n",
        )
        .map_err(|error| state_error("persist machine join milestone", error))
    }

    pub fn mark_complete(&self) -> Result<(), MachineJoinStateError> {
        write_durable_file(&self.0, COMPLETE_FILE, FileMode::Plain, b"complete\n")
            .map_err(|error| state_error("persist machine join completion", error))
    }

    fn read_identity(&self) -> Result<Option<MachineJoinIdentity>, MachineJoinStateError> {
        let bytes = match std::fs::read(self.identity_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(state_error("read machine join identity", error)),
        };
        let persisted: PersistedMachineJoinIdentity = serde_json::from_slice(&bytes)
            .map_err(|error| state_error("decode machine join identity", error))?;
        let private = Key::try_from(persisted.private_key.as_str())
            .map_err(|error| state_error("parse machine join private key", error))?;
        let public_key = WireGuardPublicKey::try_new(private.public_key().to_string())
            .map_err(|error| state_error("derive machine join public key", error))?;
        Ok(Some(MachineJoinIdentity {
            machine_id: persisted.machine_id,
            private_key: persisted.private_key,
            public_key,
            door_fingerprint: persisted.door_fingerprint,
        }))
    }

    #[must_use]
    pub(crate) fn identity_path(&self) -> PathBuf {
        self.0.join(IDENTITY_FILE)
    }

    #[must_use]
    fn acceptance_path(&self) -> PathBuf {
        self.0.join(ACCEPTANCE_FILE)
    }

    fn milestone_path(&self, milestone: MachineJoinMilestone) -> PathBuf {
        self.0.join(MILESTONE_DIRECTORY).join(milestone.file_name())
    }

    fn has_founding_evidence(&self) -> bool {
        self.0.join("founding-complete").exists()
            || self.0.join("founding-request.json").exists()
            || self.0.join("founding").exists()
    }
}

fn refused() -> MachineJoinInspection {
    MachineJoinInspection::Refused {
        refusal: MachineJoinLocalRefusal::ForeignState,
    }
}

fn door_material_exists(path: &Path) -> bool {
    ["door.key", "door.crt", "door.fingerprint"]
        .iter()
        .any(|name| path.join(name).exists())
}

#[derive(Clone, PartialEq, Eq)]
pub struct MachineJoinIdentity {
    machine_id: MachineRowId,
    private_key: String,
    public_key: WireGuardPublicKey,
    door_fingerprint: JoinDoorCertFingerprint,
}

impl MachineJoinIdentity {
    #[must_use]
    pub const fn machine_id(&self) -> &MachineRowId {
        &self.machine_id
    }

    #[must_use]
    pub const fn public_key(&self) -> &WireGuardPublicKey {
        &self.public_key
    }

    #[must_use]
    pub const fn door_fingerprint(&self) -> &JoinDoorCertFingerprint {
        &self.door_fingerprint
    }

    /// Explicitly exposes the retained private key to the privileged host executor.
    #[must_use]
    pub fn expose_private_key(&self) -> &str {
        &self.private_key
    }
}

impl fmt::Debug for MachineJoinIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MachineJoinIdentity")
            .field("machine_id", &self.machine_id)
            .field("private_key", &"[REDACTED]")
            .field("public_key", &self.public_key)
            .field("door_fingerprint", &self.door_fingerprint)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineJoinInspection {
    Clean,
    ReadyToRedeem { identity: MachineJoinIdentity },
    ReadyToActivate { identity: MachineJoinIdentity },
    NoOp { identity: MachineJoinIdentity },
    Refused { refusal: MachineJoinLocalRefusal },
}

impl MachineJoinInspection {
    #[must_use]
    pub const fn refusal(&self) -> Option<&MachineJoinLocalRefusal> {
        match self {
            Self::Refused { refusal } => Some(refusal),
            Self::Clean
            | Self::ReadyToRedeem { .. }
            | Self::ReadyToActivate { .. }
            | Self::NoOp { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineJoinLocalRefusal {
    ForeignState,
}

impl MachineJoinLocalRefusal {
    #[must_use]
    pub const fn repair_command(self) -> &'static str {
        match self {
            Self::ForeignState => JOIN_RESET_COMMAND,
        }
    }
}

impl fmt::Display for MachineJoinLocalRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "local state belongs to a different machine journey; run `{}`",
            self.repair_command()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineJoinMilestone {
    Artifacts,
    Storage,
    Docker,
    DoorMaterial,
    Configuration,
    BootstrapWireguard,
    UnitsInstalled,
    CorrosionStarted,
    RosterConverged,
    KeeperStarted,
    ApiStarted,
    Ready,
    BootstrapCleaned,
}

impl MachineJoinMilestone {
    const fn file_name(self) -> &'static str {
        match self {
            Self::Artifacts => "01-artifacts",
            Self::Storage => "02-storage",
            Self::Docker => "03-docker",
            Self::DoorMaterial => "04-door-material",
            Self::Configuration => "05-configuration",
            Self::BootstrapWireguard => "06-bootstrap-wireguard",
            Self::UnitsInstalled => "07-units-installed",
            Self::CorrosionStarted => "08-corrosion-started",
            Self::RosterConverged => "09-roster-converged",
            Self::KeeperStarted => "10-keeper-started",
            Self::ApiStarted => "11-api-started",
            Self::Ready => "12-ready",
            Self::BootstrapCleaned => "13-bootstrap-cleaned",
        }
    }
}

#[derive(Serialize, Deserialize)]
struct PersistedMachineJoinIdentity {
    machine_id: MachineRowId,
    private_key: String,
    door_fingerprint: JoinDoorCertFingerprint,
}

#[derive(Debug)]
pub struct MachineJoinLock {
    file: std::fs::File,
}

impl Drop for MachineJoinLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MachineJoinStateError {
    #[error("machine join state path {path} is not a directory", path = path.display())]
    NotDirectory { path: PathBuf },
    #[error("another ployz machine join is already running")]
    JoinInProgress,
    #[error("machine join identity cannot be written in the observed local state")]
    IdentityNotWritable,
    #[error("a different canonical machine join acceptance is already persisted")]
    AcceptanceAlreadyPersisted,
    #[error("persisted cluster identity disagrees with the accepted machine join")]
    ClusterAnchorMismatch,
    #[error("a different canonical machine join request is already persisted")]
    RequestAlreadyPersisted,
    #[error("a different machine join storage inventory is already persisted")]
    StorageInventoryAlreadyPersisted,
    #[error("a different join substrate contract is already persisted")]
    SubstrateAlreadyPersisted,
    #[error("machine join milestone {} is not a file", path.display())]
    InvalidMilestone { path: PathBuf },
    #[error("machine join state failed: {message}")]
    Failed { message: String },
}

fn state_error(subject: &str, error: impl fmt::Display) -> MachineJoinStateError {
    MachineJoinStateError::Failed {
        message: format!("{subject}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ployz_core::join::{JOIN_RESET_COMMAND, JoinDoorCertFingerprint};

    use super::*;

    fn fingerprint(byte: &str) -> JoinDoorCertFingerprint {
        JoinDoorCertFingerprint::try_new(byte.repeat(32)).expect("fingerprint")
    }

    #[test]
    fn first_join_attempt_durably_retains_identity_before_network_work() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
        let expected = fingerprint("ab");

        assert_eq!(
            state.inspect(&expected).expect("clean inspection"),
            MachineJoinInspection::Clean
        );

        let first = state.ensure_identity(&expected).expect("identity persists");
        let second = state.ensure_identity(&expected).expect("identity resumes");

        assert_eq!(first.machine_id(), second.machine_id());
        assert_eq!(first.public_key(), second.public_key());
        assert!(!format!("{first:?}").contains(first.expose_private_key()));
        assert!(matches!(
            state.inspect(&expected).expect("prepared inspection"),
            MachineJoinInspection::ReadyToRedeem { identity } if identity == first
        ));
    }

    #[test]
    fn foreign_or_founding_state_refuses_without_changing_local_files() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
        let expected = fingerprint("ab");
        state.ensure_identity(&expected).expect("identity persists");
        let before = fs::read(state.identity_path()).expect("identity bytes");

        let foreign_inspection = state
            .inspect(&fingerprint("cd"))
            .expect("foreign inspection");
        let refusal = foreign_inspection.refusal().expect("foreign state refuses");
        assert_eq!(refusal.repair_command(), JOIN_RESET_COMMAND);
        assert_eq!(
            fs::read(state.identity_path()).expect("identity remains"),
            before
        );

        let founding_directory = tempfile::tempdir().expect("tempdir");
        let founding =
            MachineJoinStateDirectory::initialize(founding_directory.path()).expect("state");
        fs::write(
            founding_directory.path().join("founding-complete"),
            b"complete\n",
        )
        .expect("founding marker");
        let founding_inspection = founding.inspect(&expected).expect("founding inspection");
        let refusal = founding_inspection
            .refusal()
            .expect("founding state refuses");
        assert_eq!(refusal.repair_command(), JOIN_RESET_COMMAND);
    }

    #[test]
    fn acceptance_and_milestones_are_durable_and_idempotent() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = MachineJoinStateDirectory::initialize(directory.path()).expect("state");
        let fixture = serde_json::json!({"accepted": true});

        state.persist_acceptance(&fixture).expect("acceptance");
        state.persist_acceptance(&fixture).expect("same acceptance");
        assert_eq!(
            state.read_acceptance::<serde_json::Value>().expect("read"),
            Some(fixture)
        );
        assert!(
            !state
                .milestone_complete(MachineJoinMilestone::Artifacts)
                .expect("observe")
        );
        state
            .record_milestone(MachineJoinMilestone::Artifacts)
            .expect("record");
        assert!(
            state
                .milestone_complete(MachineJoinMilestone::Artifacts)
                .expect("observe")
        );
    }
}
