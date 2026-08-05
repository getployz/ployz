//! Linux implementation of the founding host-effect boundary.

use std::fmt;
use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use defguard_wireguard_rs::key::Key;
use ployz_core::corrosion::{
    AutomaticHostnameMode, ClusterDocument, CorrosionDocumentVersion, CorrosionTimestamp,
    MachineDocument, MachineTransport, MeshProvider, OperationInitiator, OperatorWriteProvenance,
    PeerDocument, PeerTransport, StorageMode, derive_builtin_wireguard_member,
};
use ployz_core::deploy::ZfsPoolName;
use ployz_core::founding::{
    FoundingDriverEnrollment, FoundingRefusal, FoundingRepairCommand, FoundingRequest,
    InitStorageChoice, InitStorageFacts, InitStorageSelectionError, ValidatedFoundingRequest,
    classify_founding_arrival, select_init_storage,
};
use ployz_core::ids::{ClusterId, MachineRowId, PeerId};
use ployz_core::install::ExactPloyzVersion;
use ployz_core::join::{JOIN_DOOR_PORT, JoinMachineSubstrate};
use ployz_core::machine::{MachineLifecycle, MachineName};
use ployz_core::network::{MachineEndpointSupernet, WireGuardPublicKey};
use ployz_core::operation::FailureMessage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ArtifactKind, FileMode, HostPlatformProfile, HostRunnerCommandRunner, PloyzdRole,
    PloyzdRoleEnvironmentFile, PoolSelection, ReleaseArtifacts, SupervisorDirectories,
    SystemHostRunnerCommandRunner, artifact_target, prepare_storage, write_durable_file,
};

use crate::lifecycle::production::{
    CorrosionBootstrap, CorrosionConfig, CorrosionServiceChange, GeneratedSecretPersistence,
    LinuxSubstrate, machine_endpoint_gateway,
    read_or_generate_secret as shared_read_or_generate_secret, render_corrosion_config,
};

use super::{FoundingHostEffects, FoundingMilestone, FoundingStateDirectory};

const MACHINE_SEED_FILE: &str = "machine-seed.json";
const FOUNDING_REQUEST_FILE: &str = "founding-request.json";
const ZFS_POOL_FILE: &str = "founding-zfs-pool";
const WIREGUARD_KEY_FILE: &str = "wireguard.key";
const DOOR_KEY_FILE: &str = "door.key";
const DOOR_CERTIFICATE_FILE: &str = "door.crt";
const DOOR_FINGERPRINT_FILE: &str = "door.fingerprint";
const DOOR_PRIVATE_KEY_PATH: &str = "/var/lib/ployz/door.key";
const DOOR_CERTIFICATE_PATH: &str = "/var/lib/ployz/door.crt";
const DOOR_FINGERPRINT_PATH: &str = "/var/lib/ployz/door.fingerprint";
const JOIN_SUBSTRATE_FILE: &str = "join-substrate.json";
const JOIN_SUBSTRATE_PATH: &str = "/var/lib/ployz/join-substrate.json";
const BOOTSTRAP_CREDENTIAL_FILE: &str = "bootstrap-credential";
const ENV_FILE: &str = "ployzd.env";
const CORROSION_CONFIG_FILE: &str = "corrosion.toml";
const CORROSION_TOKEN_FILE: &str = "corrosion-token";
const API_PORT: u16 = 2_020;
const CORROSION_API_PORT: u16 = 8_080;
const DRIVER_PEER_CONVERGENCE_TIMEOUT: Duration = Duration::from_secs(60);

/// Public driver material carried into the initial peer row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoundingDriverInput {
    OnHost,
    Ssh {
        peer_id: PeerId,
        name: String,
        public_key: WireGuardPublicKey,
    },
    Cloud {
        peer_id: PeerId,
        name: String,
        public_key: WireGuardPublicKey,
    },
}

/// Cluster-fixed and machine-one values supplied by the CLI or Cloud form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxFoundingInput {
    pub cluster_name: String,
    pub machine_name: MachineName,
    pub endpoint: Option<SocketAddr>,
    pub prefix: MachineEndpointSupernet,
    pub hostname_mode: AutomaticHostnameMode,
    pub storage: InitStorageChoice,
    pub driver: FoundingDriverInput,
    pub written_at: CorrosionTimestamp,
    pub acme_directory_url: String,
    pub acme_contact: Option<String>,
}

/// Bootstrap bearer secret retained in memory and rendered only into a 0600 file.
#[derive(Clone, PartialEq, Eq)]
pub struct FoundingBootstrapCredential(String);

impl FoundingBootstrapCredential {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for FoundingBootstrapCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FoundingBootstrapCredential([REDACTED])")
    }
}

/// A request and its matching privileged effect executor.
pub struct PreparedLinuxFounding<R = SystemHostRunnerCommandRunner> {
    pub request: ValidatedFoundingRequest,
    pub effects: LinuxFoundingHostEffects<R>,
    pub bootstrap_credential: FoundingBootstrapCredential,
}

/// Offline arrival result used before release-manifest or artifact access.
#[derive(Debug, Clone)]
pub enum LinuxFoundingPreflight {
    Clean,
    Resume {
        canonical_request: Option<ValidatedFoundingRequest>,
    },
    NoOp {
        canonical_request: ValidatedFoundingRequest,
    },
    Refused {
        refusal: FoundingRefusal,
        cluster_id: Option<ClusterId>,
    },
}

/// Classifies local state without storage probes, downloads, or host mutation.
pub fn inspect_linux_founding(
    state: &FoundingStateDirectory,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<LinuxFoundingPreflight, FoundingPreparationError> {
    require_linux_root(runner)?;
    let arrival = state.observe_arrival().map_err(preparation)?;
    let cluster_id = match &arrival {
        ployz_core::founding::FoundingArrival::Clean => None,
        ployz_core::founding::FoundingArrival::Partial {
            persisted_cluster_id,
        }
        | ployz_core::founding::FoundingArrival::Complete {
            persisted_cluster_id,
        }
        | ployz_core::founding::FoundingArrival::Joined {
            persisted_cluster_id,
        } => Some(persisted_cluster_id.clone()),
    };
    let request = read_persisted_request(state.path())?;
    match read_door_material_state(state) {
        Ok(DoorMaterialState::Complete(_) | DoorMaterialState::Incomplete) => {}
        Err(FoundingPreparationError::Refused(refusal)) => {
            return Ok(LinuxFoundingPreflight::Refused {
                refusal,
                cluster_id,
            });
        }
        Err(error) => return Err(error),
    }
    match arrival {
        ployz_core::founding::FoundingArrival::Clean => Ok(LinuxFoundingPreflight::Clean),
        ployz_core::founding::FoundingArrival::Partial {
            persisted_cluster_id,
        } => {
            validate_request_cluster(request.as_ref(), &persisted_cluster_id)?;
            Ok(LinuxFoundingPreflight::Resume {
                canonical_request: request,
            })
        }
        ployz_core::founding::FoundingArrival::Complete {
            persisted_cluster_id,
        } => {
            let Some(request) = request else {
                return Err(preparation(
                    "complete founding state has no canonical founding request",
                ));
            };
            validate_request_cluster(Some(&request), &persisted_cluster_id)?;
            Ok(LinuxFoundingPreflight::NoOp {
                canonical_request: request,
            })
        }
        ployz_core::founding::FoundingArrival::Joined {
            persisted_cluster_id,
        } => {
            let refusal = classify_founding_arrival(
                &persisted_cluster_id,
                ployz_core::founding::FoundingArrival::Joined {
                    persisted_cluster_id: persisted_cluster_id.clone(),
                },
            )
            .expect_err("joined arrival always refuses founding");
            Ok(LinuxFoundingPreflight::Refused {
                refusal,
                cluster_id: Some(persisted_cluster_id),
            })
        }
    }
}

fn validate_request_cluster(
    request: Option<&ValidatedFoundingRequest>,
    cluster_id: &ClusterId,
) -> Result<(), FoundingPreparationError> {
    if request.is_some_and(|request| request.request().cluster_id != *cluster_id) {
        return Err(preparation(
            "persisted founding request disagrees with cluster-id anchor",
        ));
    }
    Ok(())
}

fn require_linux_root(
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), FoundingPreparationError> {
    if !runner.is_linux() {
        return Err(preparation("ployz init requires Linux"));
    }
    if runner.current_uid().map_err(preparation)? != 0 {
        return Err(preparation("ployz init must run as root"));
    }
    Ok(())
}

/// Builds a request from machine-local facts without persisting founding state.
/// The first durable founding write remains the orchestrator's cluster-id anchor.
pub fn prepare_linux_founding<R: HostRunnerCommandRunner>(
    state: &FoundingStateDirectory,
    input: LinuxFoundingInput,
    artifacts: ReleaseArtifacts,
    corrosion_embedded_version: String,
    mut runner: R,
) -> Result<PreparedLinuxFounding<R>, FoundingPreparationError> {
    require_linux_root(&mut runner)?;
    let arrival = state.observe_arrival().map_err(preparation)?;
    let cluster_id = match &arrival {
        ployz_core::founding::FoundingArrival::Clean => ClusterId::generate(),
        ployz_core::founding::FoundingArrival::Partial {
            persisted_cluster_id,
        }
        | ployz_core::founding::FoundingArrival::Complete {
            persisted_cluster_id,
        }
        | ployz_core::founding::FoundingArrival::Joined {
            persisted_cluster_id,
        } => persisted_cluster_id.clone(),
    };
    if matches!(
        arrival,
        ployz_core::founding::FoundingArrival::Joined { .. }
    ) {
        let refusal = classify_founding_arrival(&cluster_id, arrival)
            .expect_err("joined arrival always refuses founding");
        return Err(FoundingPreparationError::Refused(refusal));
    }
    let persisted_request = read_persisted_request(state.path())?;
    if let Some(request) = persisted_request {
        if request.request().cluster_id != cluster_id {
            return Err(preparation(
                "persisted founding request disagrees with cluster-id anchor",
            ));
        }
        return prepared_from_request(
            state,
            request,
            artifacts,
            corrosion_embedded_version,
            runner,
        );
    }
    let seed = read_or_generate_machine_seed(state.path())?;
    let key = Key::try_from(seed.private_key.as_str()).map_err(preparation)?;
    let public_key =
        WireGuardPublicKey::try_new(key.public_key().to_string()).map_err(preparation)?;
    let inventory = match input.storage {
        InitStorageChoice::Flag {
            mode: StorageMode::Plain,
        } => StorageInventory {
            facts: InitStorageFacts {
                imported_zfs_pool: false,
                total_memory_bytes: 0,
            },
            pools: Vec::new(),
        },
        InitStorageChoice::Automatic
        | InitStorageChoice::Flag {
            mode: StorageMode::Zfs,
        } => storage_inventory(&mut runner)?,
    };
    let storage = select_init_storage(input.storage, inventory.facts)
        .map_err(FoundingPreparationError::Storage)?;
    let zfs_pool = select_zfs_pool(state.path(), storage.mode, inventory.pools)?;
    let subnet = input.prefix.allocate_next([]).map_err(preparation)?;
    let provenance = OperatorWriteProvenance {
        written_by: OperationInitiator::Machine {
            machine_id: seed.machine_id.clone(),
        },
        written_at: input.written_at,
    };
    let driver = build_driver(&cluster_id, &provenance, input.driver);
    let request = FoundingRequest {
        cluster_id: cluster_id.clone(),
        cluster: ClusterDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id.clone(),
            provenance: provenance.clone(),
            name: input.cluster_name,
            storage_default: storage.mode,
            hostname_mode: input.hostname_mode,
            prefix: input.prefix,
            provider: MeshProvider::BuiltinWireguard,
            acme_directory_url: input.acme_directory_url,
            acme_contact: input.acme_contact,
        },
        machine_id: seed.machine_id.clone(),
        machine: MachineDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id.clone(),
            provenance,
            name: input.machine_name,
            lifecycle: MachineLifecycle::Active,
            transport: MachineTransport::Wireguard {
                addr_v6: derive_builtin_wireguard_member(&cluster_id, &public_key)
                    .bind_address()
                    .get(),
                pubkey: public_key,
                endpoint: input.endpoint,
                subnet_v4: subnet,
            },
            storage,
        },
        driver,
    }
    .try_validate()
    .map_err(preparation)?;
    let bootstrap_credential = FoundingBootstrapCredential(
        shared_read_or_generate_secret(
            &state.path().join(BOOTSTRAP_CREDENTIAL_FILE),
            GeneratedSecretPersistence::InMemory,
        )
        .map_err(preparation)?,
    );
    let _door_material = read_door_material_state(state)?;
    let effects = LinuxFoundingHostEffects {
        state: state.clone(),
        request: request.clone(),
        artifacts,
        corrosion_embedded_version,
        machine_seed: seed,
        bootstrap_credential: bootstrap_credential.clone(),
        corrosion_token: shared_read_or_generate_secret(
            &state.path().join(CORROSION_TOKEN_FILE),
            GeneratedSecretPersistence::InMemory,
        )
        .map_err(preparation)?,
        zfs_pool,
        runner,
        profile: None,
        supervisor_directories: SupervisorDirectories::host_defaults(),
    };
    Ok(PreparedLinuxFounding {
        request,
        effects,
        bootstrap_credential,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum FoundingPreparationError {
    #[error("founding refused: {0:?}")]
    Refused(FoundingRefusal),
    #[error("founding storage selection refused: {0}")]
    Storage(InitStorageSelectionError),
    #[error(
        "founding ZFS selection found multiple imported pools ({pools:?}); export all but the intended pool or choose --storage plain"
    )]
    MultipleImportedZfsPools { pools: Vec<ZfsPoolName> },
    #[error(
        "founding ZFS selection retained pool {pool:?}, but it is no longer imported; import that pool or choose --storage plain after resetting founding state"
    )]
    RetainedZfsPoolNotImported { pool: ZfsPoolName },
    #[error("failed to prepare machine-one founding: {message}")]
    Failed { message: String },
}

fn preparation(error: impl fmt::Display) -> FoundingPreparationError {
    FoundingPreparationError::Failed {
        message: error.to_string(),
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct MachineSeed {
    machine_id: MachineRowId,
    private_key: String,
}

impl fmt::Debug for MachineSeed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MachineSeed")
            .field("machine_id", &self.machine_id)
            .field("private_key", &"[REDACTED]")
            .finish()
    }
}

struct DoorMaterial {
    certificate_pem: String,
    private_key_pem: String,
    fingerprint: String,
}

enum DoorMaterialState {
    Complete(DoorMaterial),
    Incomplete,
}

fn read_persisted_request(
    state: &Path,
) -> Result<Option<ValidatedFoundingRequest>, FoundingPreparationError> {
    match fs::read(state.join(FOUNDING_REQUEST_FILE)) {
        Ok(bytes) => serde_json::from_slice::<FoundingRequest>(&bytes)
            .map_err(preparation)?
            .try_validate()
            .map(Some)
            .map_err(preparation),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(preparation(error)),
    }
}

fn prepared_from_request<R: HostRunnerCommandRunner>(
    state: &FoundingStateDirectory,
    request: ValidatedFoundingRequest,
    artifacts: ReleaseArtifacts,
    corrosion_embedded_version: String,
    runner: R,
) -> Result<PreparedLinuxFounding<R>, FoundingPreparationError> {
    let seed = read_machine_seed(state.path())?;
    validate_seed_matches_request(&seed, &request)?;
    let bootstrap_credential = FoundingBootstrapCredential(
        shared_read_or_generate_secret(
            &state.path().join(BOOTSTRAP_CREDENTIAL_FILE),
            GeneratedSecretPersistence::InMemory,
        )
        .map_err(preparation)?,
    );
    let _door_material = read_door_material_state(state)?;
    let effects = LinuxFoundingHostEffects {
        state: state.clone(),
        request: request.clone(),
        artifacts,
        corrosion_embedded_version,
        machine_seed: seed,
        bootstrap_credential: bootstrap_credential.clone(),
        corrosion_token: shared_read_or_generate_secret(
            &state.path().join(CORROSION_TOKEN_FILE),
            GeneratedSecretPersistence::InMemory,
        )
        .map_err(preparation)?,
        zfs_pool: read_required_zfs_pool(state.path(), request.request().machine.storage.mode)?,
        runner,
        profile: None,
        supervisor_directories: SupervisorDirectories::host_defaults(),
    };
    Ok(PreparedLinuxFounding {
        request,
        effects,
        bootstrap_credential,
    })
}

fn read_machine_seed(state: &Path) -> Result<MachineSeed, FoundingPreparationError> {
    let path = state.join(MACHINE_SEED_FILE);
    let bytes = fs::read(&path).map_err(|error| {
        preparation(format!(
            "persisted founding request requires machine seed {}: {error}",
            path.display()
        ))
    })?;
    serde_json::from_slice(&bytes).map_err(preparation)
}

fn validate_seed_matches_request(
    seed: &MachineSeed,
    request: &ValidatedFoundingRequest,
) -> Result<(), FoundingPreparationError> {
    if seed.machine_id != request.request().machine_id {
        return Err(preparation(
            "machine seed id disagrees with persisted founding request",
        ));
    }
    let private = Key::try_from(seed.private_key.as_str()).map_err(preparation)?;
    let public = private.public_key().to_string();
    let MachineTransport::Wireguard { pubkey, .. } = &request.request().machine.transport else {
        return Err(preparation(
            "persisted founding request machine is not WireGuard",
        ));
    };
    if public != pubkey.as_str() {
        return Err(preparation(
            "machine seed key disagrees with persisted founding request",
        ));
    }
    Ok(())
}

fn read_door_material_state(
    state: &FoundingStateDirectory,
) -> Result<DoorMaterialState, FoundingPreparationError> {
    let certificate = state.path().join(DOOR_CERTIFICATE_FILE);
    let key = state.path().join(DOOR_KEY_FILE);
    let fingerprint = state.path().join(DOOR_FINGERPRINT_FILE);
    let certificate_result = fs::read_to_string(&certificate);
    let key_result = fs::read_to_string(&key);
    let fingerprint_result = fs::read_to_string(&fingerprint);
    if let (Ok(certificate_pem), Ok(private_key_pem), Ok(fingerprint)) =
        (&certificate_result, &key_result, &fingerprint_result)
    {
        return Ok(DoorMaterialState::Complete(DoorMaterial {
            certificate_pem: certificate_pem.clone(),
            private_key_pem: private_key_pem.clone(),
            fingerprint: fingerprint.trim().to_owned(),
        }));
    }
    for result in [&certificate_result, &key_result, &fingerprint_result] {
        if let Err(error) = result
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(preparation(error));
        }
    }
    if state
        .milestone_complete(FoundingMilestone::DoorMaterial)
        .map_err(preparation)?
    {
        return Err(FoundingPreparationError::Refused(
            FoundingRefusal::IncompleteDoorMaterial {
                repair_command: FoundingRepairCommand::ResetMachine,
            },
        ));
    }
    Ok(DoorMaterialState::Incomplete)
}

fn generate_door_material() -> Result<DoorMaterial, FailureMessage> {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(["door.ployz.internal".to_owned()]).map_err(failure)?;
    Ok(DoorMaterial {
        fingerprint: format!("{:x}", Sha256::digest(cert.der())),
        certificate_pem: cert.pem(),
        private_key_pem: signing_key.serialize_pem(),
    })
}

fn read_or_generate_machine_seed(state: &Path) -> Result<MachineSeed, FoundingPreparationError> {
    let path = state.join(MACHINE_SEED_FILE);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(preparation),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(MachineSeed {
            machine_id: MachineRowId::generate(),
            private_key: Key::generate().to_string(),
        }),
        Err(error) => Err(preparation(error)),
    }
}

struct StorageInventory {
    facts: InitStorageFacts,
    pools: Vec<ZfsPoolName>,
}

fn storage_inventory(
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<StorageInventory, FoundingPreparationError> {
    let memory = runner
        .command("cat", &["/proc/meminfo"])
        .map_err(preparation)?;
    if !memory.success {
        return Err(preparation(memory.failure));
    }
    let total_memory_bytes = memory
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|kib| kib.checked_mul(1_024))
        .ok_or_else(|| preparation("/proc/meminfo has no valid MemTotal"))?;
    let mut pools = match runner.command("zpool", &["list", "-H", "-o", "name"]) {
        Ok(output) if output.success => {
            if output.stdout_truncated {
                return Err(preparation("imported ZFS pool inventory was truncated"));
            }
            output
                .stdout
                .lines()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(|name| ZfsPoolName::try_new(name.to_owned()).map_err(preparation))
                .collect::<Result<Vec<_>, _>>()?
        }
        Ok(_) | Err(_) => Vec::new(),
    };
    pools.sort();
    pools.dedup();
    Ok(StorageInventory {
        facts: InitStorageFacts {
            imported_zfs_pool: !pools.is_empty(),
            total_memory_bytes,
        },
        pools,
    })
}

fn select_zfs_pool(
    state: &Path,
    mode: StorageMode,
    pools: Vec<ZfsPoolName>,
) -> Result<Option<ZfsPoolName>, FoundingPreparationError> {
    if mode == StorageMode::Plain {
        return Ok(None);
    }
    if let Some(retained) = read_optional_zfs_pool(state)? {
        if pools.contains(&retained) {
            return Ok(Some(retained));
        }
        return Err(FoundingPreparationError::RetainedZfsPoolNotImported { pool: retained });
    }
    match pools.as_slice() {
        [pool] => Ok(Some(pool.clone())),
        [] => Err(FoundingPreparationError::Storage(
            InitStorageSelectionError::ZfsPoolNotImported,
        )),
        _ => Err(FoundingPreparationError::MultipleImportedZfsPools { pools }),
    }
}

fn read_optional_zfs_pool(state: &Path) -> Result<Option<ZfsPoolName>, FoundingPreparationError> {
    match fs::read_to_string(state.join(ZFS_POOL_FILE)) {
        Ok(value) => ZfsPoolName::try_new(value.trim().to_owned())
            .map(Some)
            .map_err(preparation),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(preparation(error)),
    }
}

fn read_required_zfs_pool(
    state: &Path,
    mode: StorageMode,
) -> Result<Option<ZfsPoolName>, FoundingPreparationError> {
    match (mode, read_optional_zfs_pool(state)?) {
        (StorageMode::Plain, _) => Ok(None),
        (StorageMode::Zfs, Some(pool)) => Ok(Some(pool)),
        (StorageMode::Zfs, None) => Err(preparation(
            "persisted ZFS founding request has no retained pool selection",
        )),
    }
}

fn build_driver(
    cluster_id: &ClusterId,
    provenance: &OperatorWriteProvenance,
    input: FoundingDriverInput,
) -> FoundingDriverEnrollment {
    let row = |name, public_key: WireGuardPublicKey| PeerDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: cluster_id.clone(),
        provenance: provenance.clone(),
        name,
        transport: PeerTransport::Wireguard {
            addr_v6: derive_builtin_wireguard_member(cluster_id, &public_key)
                .bind_address()
                .get(),
            pubkey: public_key,
            endpoint: None,
        },
    };
    match input {
        FoundingDriverInput::OnHost => FoundingDriverEnrollment::OnHost,
        FoundingDriverInput::Ssh {
            peer_id,
            name,
            public_key,
        } => FoundingDriverEnrollment::Ssh {
            peer_id,
            document: row(name, public_key),
        },
        FoundingDriverInput::Cloud {
            peer_id,
            name,
            public_key,
        } => FoundingDriverEnrollment::Cloud {
            peer_id,
            document: row(name, public_key),
        },
    }
}

/// Production Linux executor. Secrets are redacted from Debug and never enter argv.
pub struct LinuxFoundingHostEffects<R = SystemHostRunnerCommandRunner> {
    state: FoundingStateDirectory,
    request: ValidatedFoundingRequest,
    artifacts: ReleaseArtifacts,
    corrosion_embedded_version: String,
    machine_seed: MachineSeed,
    bootstrap_credential: FoundingBootstrapCredential,
    corrosion_token: String,
    zfs_pool: Option<ZfsPoolName>,
    runner: R,
    profile: Option<HostPlatformProfile>,
    supervisor_directories: SupervisorDirectories,
}

impl<R> fmt::Debug for LinuxFoundingHostEffects<R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxFoundingHostEffects")
            .field("state", &self.state)
            .field("request", &self.request)
            .field("secrets", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

mod effects;

fn merge_docker_daemon_config(prefix: &MachineEndpointSupernet) -> Result<(), FailureMessage> {
    let path = Path::new("/etc/docker/daemon.json");
    fs::create_dir_all(path.parent().expect("Docker config has parent")).map_err(failure)?;
    let mut value = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes).map_err(failure)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(error) => return Err(failure(error)),
    };
    let Some(object) = value.as_object_mut() else {
        return Err(failure("/etc/docker/daemon.json is not a JSON object"));
    };
    let registry = prefix.as_string();
    let registries = object
        .entry("insecure-registries")
        .or_insert_with(|| serde_json::json!([]));
    let Some(registries) = registries.as_array_mut() else {
        return Err(failure("Docker insecure-registries is not an array"));
    };
    if !registries
        .iter()
        .any(|item| item.as_str() == Some(&registry))
    {
        registries.push(serde_json::Value::String(registry));
    }
    let features = object
        .entry("features")
        .or_insert_with(|| serde_json::json!({}));
    let Some(features) = features.as_object_mut() else {
        return Err(failure("Docker features is not an object"));
    };
    features.insert(
        "containerd-snapshotter".to_owned(),
        serde_json::Value::Bool(true),
    );
    let builder = object
        .entry("builder")
        .or_insert_with(|| serde_json::json!({}));
    let Some(builder) = builder.as_object_mut() else {
        return Err(failure("Docker builder is not an object"));
    };
    let gc = builder.entry("gc").or_insert_with(|| serde_json::json!({}));
    let Some(gc) = gc.as_object_mut() else {
        return Err(failure("Docker builder.gc is not an object"));
    };
    gc.insert("enabled".to_owned(), serde_json::Value::Bool(true));
    gc.insert(
        "defaultKeepStorage".to_owned(),
        serde_json::Value::String("20GB".to_owned()),
    );
    let bytes = serde_json::to_vec_pretty(&value).map_err(failure)?;
    write_durable_file(
        path.parent().expect("Docker config has parent"),
        "daemon.json",
        FileMode::Plain,
        &bytes,
    )
}

fn failure(error: impl fmt::Display) -> FailureMessage {
    FailureMessage::try_new(error.to_string()).unwrap_or_else(|_| {
        FailureMessage::try_new("founding host effect failed").expect("constant is non-empty")
    })
}

#[cfg(test)]
mod tests;
