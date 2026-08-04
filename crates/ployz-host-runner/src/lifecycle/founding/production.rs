//! Linux implementation of the founding host-effect boundary.

use std::fmt;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use defguard_wireguard_rs::key::Key;
use ployz_core::corrosion::{
    AutomaticHostnameMode, ClusterDocument, CorrosionDocumentVersion, CorrosionTimestamp,
    MachineDocument, MachineTransport, MeshProvider, OperationInitiator, OperatorWriteProvenance,
    PeerDocument, PeerTransport, StorageMode, derive_builtin_wireguard_member,
};
use ployz_core::founding::{
    FoundingDriverEnrollment, FoundingRefusal, FoundingRequest, InitStorageChoice,
    InitStorageFacts, InitStorageSelectionError, ValidatedFoundingRequest,
    classify_founding_arrival, select_init_storage,
};
use ployz_core::ids::{ClusterId, MachineRowId, PeerId};
use ployz_core::machine::{MachineLifecycle, MachineName};
use ployz_core::network::{MachineEndpointSupernet, WireGuardPublicKey};
use ployz_core::operation::FailureMessage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ArtifactKind, ArtifactSourceView, FileMode, HostPlatformProfile, HostRunnerCommandRunner,
    PloyzdRole, PloyzdRoleEnvironmentFile, PoolSelection, ReleaseArtifacts, SupervisorBackend,
    SupervisorChange, SupervisorDirectories, SupervisorUnitSpec, SystemHostRunnerCommandRunner,
    artifact_target, detect_host_platform, install_verified_artifact, prepare_storage,
    verify_artifact_file, write_durable_file,
};

use super::{FoundingHostEffects, FoundingStateDirectory};

const MACHINE_SEED_FILE: &str = "machine-seed.json";
const FOUNDING_REQUEST_FILE: &str = "founding-request.json";
const WIREGUARD_KEY_FILE: &str = "wireguard.key";
const DOOR_KEY_FILE: &str = "door.key";
const DOOR_CERTIFICATE_FILE: &str = "door.crt";
const DOOR_FINGERPRINT_FILE: &str = "door.fingerprint";
const BOOTSTRAP_CREDENTIAL_FILE: &str = "bootstrap-credential";
const ENV_FILE: &str = "ployzd.env";
const CORROSION_CONFIG_FILE: &str = "corrosion.toml";
const CORROSION_TOKEN_FILE: &str = "corrosion-token";
const API_PORT: u16 = 2_020;
const CORROSION_API_PORT: u16 = 8_080;
const CORROSION_GOSSIP_PORT: u16 = 8_787;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FoundingRoleDisposition {
    Enabled,
    DisabledAndInactive,
}

const fn founding_role_disposition(role: PloyzdRole) -> FoundingRoleDisposition {
    match role {
        PloyzdRole::Keeper | PloyzdRole::Api => FoundingRoleDisposition::Enabled,
        PloyzdRole::Gateway | PloyzdRole::Dns => FoundingRoleDisposition::DisabledAndInactive,
    }
}

/// Public driver material carried into the initial peer row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoundingDriverInput {
    OnHost,
    Ssh {
        peer_id: PeerId,
        name: String,
        public_key: WireGuardPublicKey,
        endpoint: Option<SocketAddr>,
    },
    Cloud {
        peer_id: PeerId,
        name: String,
        public_key: WireGuardPublicKey,
        endpoint: Option<SocketAddr>,
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
    Refused(FoundingRefusal),
}

/// Classifies local state without storage probes, downloads, or host mutation.
pub fn inspect_linux_founding(
    state: &FoundingStateDirectory,
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<LinuxFoundingPreflight, FoundingPreparationError> {
    require_linux_root(runner)?;
    let arrival = state.observe_arrival().map_err(preparation)?;
    let request = read_persisted_request(state.path())?;
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
            Ok(LinuxFoundingPreflight::Refused(refusal))
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
    let facts = match input.storage {
        InitStorageChoice::Flag {
            mode: StorageMode::Plain,
        } => InitStorageFacts {
            imported_zfs_pool: false,
            total_memory_bytes: 0,
        },
        InitStorageChoice::Automatic
        | InitStorageChoice::Flag {
            mode: StorageMode::Zfs,
        } => storage_facts(&mut runner)?,
    };
    let storage =
        select_init_storage(input.storage, facts).map_err(FoundingPreparationError::Storage)?;
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
    let bootstrap_credential = FoundingBootstrapCredential(read_or_generate_secret(
        &state.path().join(BOOTSTRAP_CREDENTIAL_FILE),
    )?);
    let effects = LinuxFoundingHostEffects {
        state: state.clone(),
        request: request.clone(),
        artifacts,
        corrosion_embedded_version,
        machine_seed: seed,
        door_material: read_or_generate_door_material(state.path())?,
        bootstrap_credential: bootstrap_credential.clone(),
        corrosion_token: read_or_generate_secret(&state.path().join(CORROSION_TOKEN_FILE))?,
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
    let bootstrap_credential = FoundingBootstrapCredential(read_or_generate_secret(
        &state.path().join(BOOTSTRAP_CREDENTIAL_FILE),
    )?);
    let effects = LinuxFoundingHostEffects {
        state: state.clone(),
        request: request.clone(),
        artifacts,
        corrosion_embedded_version,
        machine_seed: seed,
        door_material: read_or_generate_door_material(state.path())?,
        bootstrap_credential: bootstrap_credential.clone(),
        corrosion_token: read_or_generate_secret(&state.path().join(CORROSION_TOKEN_FILE))?,
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

fn read_or_generate_secret(path: &Path) -> Result<String, FoundingPreparationError> {
    match fs::read_to_string(path) {
        Ok(secret) if !secret.trim().is_empty() => Ok(secret.trim().to_owned()),
        Ok(_) => Err(preparation(format!(
            "secret file {} is empty",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(Key::generate().to_string())
        }
        Err(error) => Err(preparation(error)),
    }
}

fn read_or_generate_door_material(state: &Path) -> Result<DoorMaterial, FoundingPreparationError> {
    let certificate = state.join(DOOR_CERTIFICATE_FILE);
    let key = state.join(DOOR_KEY_FILE);
    let fingerprint = state.join(DOOR_FINGERPRINT_FILE);
    match (
        fs::read_to_string(&certificate),
        fs::read_to_string(&key),
        fs::read_to_string(&fingerprint),
    ) {
        (Ok(certificate_pem), Ok(private_key_pem), Ok(fingerprint)) => Ok(DoorMaterial {
            certificate_pem,
            private_key_pem,
            fingerprint: fingerprint.trim().to_owned(),
        }),
        (Err(cert), Err(key), Err(fingerprint))
            if cert.kind() == std::io::ErrorKind::NotFound
                && key.kind() == std::io::ErrorKind::NotFound
                && fingerprint.kind() == std::io::ErrorKind::NotFound =>
        {
            let rcgen::CertifiedKey { cert, signing_key } =
                rcgen::generate_simple_self_signed(["door.ployz.internal".to_owned()])
                    .map_err(preparation)?;
            Ok(DoorMaterial {
                fingerprint: format!("{:x}", Sha256::digest(cert.der())),
                certificate_pem: cert.pem(),
                private_key_pem: signing_key.serialize_pem(),
            })
        }
        _ => Err(preparation("cluster door TLS material is incomplete")),
    }
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

fn storage_facts(
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<InitStorageFacts, FoundingPreparationError> {
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
    let imported_zfs_pool = runner
        .command("zpool", &["list", "-H", "-o", "name"])
        .is_ok_and(|pools| pools.success && !pools.stdout.trim().is_empty());
    Ok(InitStorageFacts {
        imported_zfs_pool,
        total_memory_bytes,
    })
}

fn build_driver(
    cluster_id: &ClusterId,
    provenance: &OperatorWriteProvenance,
    input: FoundingDriverInput,
) -> FoundingDriverEnrollment {
    let row = |name, public_key: WireGuardPublicKey, endpoint| PeerDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: cluster_id.clone(),
        provenance: provenance.clone(),
        name,
        transport: PeerTransport::Wireguard {
            addr_v6: derive_builtin_wireguard_member(cluster_id, &public_key)
                .bind_address()
                .get(),
            pubkey: public_key,
            endpoint,
        },
    };
    match input {
        FoundingDriverInput::OnHost => FoundingDriverEnrollment::OnHost,
        FoundingDriverInput::Ssh {
            peer_id,
            name,
            public_key,
            endpoint,
        } => FoundingDriverEnrollment::Ssh {
            peer_id,
            document: row(name, public_key, endpoint),
        },
        FoundingDriverInput::Cloud {
            peer_id,
            name,
            public_key,
            endpoint,
        } => FoundingDriverEnrollment::Cloud {
            peer_id,
            document: row(name, public_key, endpoint),
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
    door_material: DoorMaterial,
    bootstrap_credential: FoundingBootstrapCredential,
    corrosion_token: String,
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

impl<R: HostRunnerCommandRunner> LinuxFoundingHostEffects<R> {
    fn profile(&mut self) -> Result<&HostPlatformProfile, FailureMessage> {
        if self.profile.is_none() {
            let release = self.runner.read_os_release()?;
            self.profile = Some(detect_host_platform(&release).map_err(failure)?);
        }
        Ok(self.profile.as_ref().expect("profile was populated"))
    }

    fn require(&mut self, program: &str, args: &[&str]) -> Result<(), FailureMessage> {
        let output = self.runner.command(program, args)?;
        if output.success {
            Ok(())
        } else {
            Err(failure(output.failure))
        }
    }

    fn install_artifact(
        &mut self,
        kind: ArtifactKind,
        spec: &ployz_core::install::InstallArtifactSpec,
    ) -> Result<(), FailureMessage> {
        let target = artifact_target(kind, spec).map_err(failure)?;
        let source = match target.source_view() {
            ArtifactSourceView::LocalPath(path) => path.to_path_buf(),
            ArtifactSourceView::RemoteUrl(url) => {
                let downloads = self.state.path().join("downloads");
                fs::create_dir_all(&downloads).map_err(failure)?;
                let download = downloads.join(spec.sha256.as_str());
                if !download.exists() {
                    self.runner.download(url, &download)?;
                }
                download
            }
        };
        let verified = verify_artifact_file(&source, &target.digest).map_err(failure)?;
        install_verified_artifact(&verified, &target).map_err(failure)?;
        Ok(())
    }

    fn supervisor_backend(&mut self) -> Result<SupervisorBackend, FailureMessage> {
        Ok(self.profile()?.supervisor().into())
    }

    fn env_contents(&self, include_bootstrap: bool) -> Result<Vec<u8>, FailureMessage> {
        let request = self.request.request();
        let MachineTransport::Wireguard { addr_v6, .. } = &request.machine.transport else {
            return Err(failure("founding machine transport is not WireGuard"));
        };
        let mut env = format!(
            "PLOYZ_CORROSION_API_ADDR=127.0.0.1:{CORROSION_API_PORT}\nPLOYZ_CORROSION_BEARER_TOKEN={}\nPLOYZ_CLUSTER_ID={}\nPLOYZ_MACHINE_ID={}\nPLOYZ_API_LISTEN_ADDR=[{addr_v6}]:{API_PORT}\nPLOYZ_BUILD={}\nPLOYZ_WIREGUARD_PRIVATE_KEY_PATH={}/{}\nPLOYZ_CORROSION_VERSION={}\n",
            self.corrosion_token,
            request.cluster_id,
            request.machine_id,
            self.artifacts.ployzd.version.as_str(),
            self.state.path().display(),
            WIREGUARD_KEY_FILE,
            self.corrosion_embedded_version,
        );
        if include_bootstrap {
            env.push_str("PLOYZ_API_BOOTSTRAP_SECRET=");
            env.push_str(self.bootstrap_credential.as_str());
            env.push('\n');
        }
        Ok(env.into_bytes())
    }
}

impl<R: HostRunnerCommandRunner> FoundingHostEffects for LinuxFoundingHostEffects<R> {
    fn stage_exact_ployz_and_corrosion(&mut self) -> Result<(), FailureMessage> {
        for (kind, spec) in [
            (ArtifactKind::Ployzd, self.artifacts.ployzd.clone()),
            (ArtifactKind::Corrosion, self.artifacts.corrosion.clone()),
            (
                ArtifactKind::CorrosionSchema,
                self.artifacts.corrosion_schema.clone(),
            ),
            (
                ArtifactKind::EbpfBytecode,
                self.artifacts.ebpf_bytecode.clone(),
            ),
            (ArtifactKind::EbpfCtl, self.artifacts.ebpf_ctl.clone()),
        ] {
            self.install_artifact(kind, &spec)?;
        }
        let version = self
            .runner
            .command("/usr/local/bin/corrosion", &["--version"])?;
        if version.success && version.stdout.trim() == self.corrosion_embedded_version {
            Ok(())
        } else {
            Err(failure(format!(
                "installed Corrosion version mismatch: expected {:?}, got {:?}",
                self.corrosion_embedded_version,
                version.stdout.trim()
            )))
        }
    }

    fn ensure_docker(&mut self) -> Result<(), FailureMessage> {
        if !self.runner.docker_is_installed() {
            let install = self.profile()?.docker_install();
            match install {
                crate::DockerInstall::GetDocker => {
                    let script = self.state.path().join("get-docker.sh");
                    self.runner.download("https://get.docker.com", &script)?;
                    self.require("sh", &[script.to_string_lossy().as_ref()])?;
                }
                crate::DockerInstall::AlpinePackages => {
                    self.require("apk", &["add", "docker"])?;
                }
                crate::DockerInstall::ArchPackages => {
                    self.require("pacman", &["--noconfirm", "-S", "docker"])?;
                }
                crate::DockerInstall::SusePackages => {
                    self.require("zypper", &["--non-interactive", "install", "docker"])?;
                }
                crate::DockerInstall::AmazonPackages => {
                    self.require("dnf", &["install", "-y", "docker"])?;
                }
                crate::DockerInstall::RhelRepositoryFile
                | crate::DockerInstall::CentosRepositoryFile => {
                    self.require("dnf", &["install", "-y", "docker-ce"])?;
                }
            }
        }
        let backend = self.supervisor_backend()?;
        for (program, args) in backend.docker_commands(SupervisorChange::InstallAndStart) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        self.runner.docker_info()
    }

    fn ensure_machine_identity_and_wireguard(&mut self) -> Result<(), FailureMessage> {
        let bytes = serde_json::to_vec_pretty(&self.machine_seed).map_err(failure)?;
        write_durable_file(
            self.state.path(),
            MACHINE_SEED_FILE,
            FileMode::Secret0600,
            &bytes,
        )?;
        write_durable_file(
            self.state.path(),
            WIREGUARD_KEY_FILE,
            FileMode::Secret0600,
            format!("{}\n", self.machine_seed.private_key).as_bytes(),
        )
    }

    fn ensure_cluster_door_material(&mut self) -> Result<(), FailureMessage> {
        write_durable_file(
            self.state.path(),
            DOOR_KEY_FILE,
            FileMode::Secret0600,
            self.door_material.private_key_pem.as_bytes(),
        )?;
        write_durable_file(
            self.state.path(),
            DOOR_CERTIFICATE_FILE,
            FileMode::Plain,
            self.door_material.certificate_pem.as_bytes(),
        )?;
        write_durable_file(
            self.state.path(),
            DOOR_FINGERPRINT_FILE,
            FileMode::Plain,
            format!("{}\n", self.door_material.fingerprint).as_bytes(),
        )
    }

    fn ensure_machine_endpoint_subnet(&mut self) -> Result<(), FailureMessage> {
        let request = self.request.request();
        let MachineTransport::Wireguard { subnet_v4, .. } = &request.machine.transport else {
            return Err(failure("founding machine transport is not WireGuard"));
        };
        if request.cluster.prefix.contains_subnet(subnet_v4) {
            Ok(())
        } else {
            Err(failure(
                "machine-one endpoint subnet is outside cluster prefix",
            ))
        }
    }

    fn prepare_selected_storage(&mut self) -> Result<(), FailureMessage> {
        match self.request.request().machine.storage.mode {
            StorageMode::Plain => {
                fs::create_dir_all(self.state.path().join("volumes")).map_err(failure)
            }
            StorageMode::Zfs => {
                let profile = self.profile()?.clone();
                prepare_storage(
                    &mut self.runner,
                    &profile,
                    &PoolSelection::Automatic,
                    self.state.path(),
                    Path::new("/etc/systemd/system/docker.service.d"),
                )
                .map(|_| ())
                .map_err(failure)
            }
        }
    }

    fn write_configuration_with_bootstrap(&mut self) -> Result<(), FailureMessage> {
        let request = self.request.request();
        let MachineTransport::Wireguard { addr_v6, .. } = &request.machine.transport else {
            return Err(failure("founding machine transport is not WireGuard"));
        };
        write_durable_file(
            self.state.path(),
            CORROSION_TOKEN_FILE,
            FileMode::Secret0600,
            format!("{}\n", self.corrosion_token).as_bytes(),
        )?;
        write_durable_file(
            self.state.path(),
            BOOTSTRAP_CREDENTIAL_FILE,
            FileMode::Secret0600,
            format!("{}\n", self.bootstrap_credential.as_str()).as_bytes(),
        )?;
        let subscriptions = self.state.path().join("subscriptions");
        fs::create_dir_all(&subscriptions).map_err(failure)?;
        let corrosion = format!(
            "[db]\npath = {db:?}\nschema_paths = [{schema:?}]\nsubscriptions_path = {subscriptions:?}\n\n[gossip]\naddr = {gossip:?}\nbootstrap = []\nplaintext = true\nmax_mtu = 1232\n\n[api]\naddr = {api:?}\nauthz.bearer-token = {token:?}\n\n[admin]\npath = {admin:?}\n",
            db = self.state.path().join("corrosion.db").display().to_string(),
            schema = self.artifacts.corrosion_schema.install_path.as_str(),
            subscriptions = subscriptions.display().to_string(),
            gossip = format!("[{addr_v6}]:{CORROSION_GOSSIP_PORT}"),
            api = format!("127.0.0.1:{CORROSION_API_PORT}"),
            token = self.corrosion_token,
            admin = self
                .state
                .path()
                .join("corrosion-admin.sock")
                .display()
                .to_string(),
        );
        write_durable_file(
            self.state.path(),
            CORROSION_CONFIG_FILE,
            FileMode::Secret0600,
            corrosion.as_bytes(),
        )?;
        write_durable_file(
            self.state.path(),
            ENV_FILE,
            FileMode::Secret0600,
            &self.env_contents(true)?,
        )?;
        merge_docker_daemon_config(&request.cluster.prefix)
    }

    fn persist_validated_founding_request(&mut self) -> Result<(), FailureMessage> {
        let bytes = serde_json::to_vec_pretty(self.request.request()).map_err(failure)?;
        write_durable_file(
            self.state.path(),
            FOUNDING_REQUEST_FILE,
            FileMode::Secret0600,
            &bytes,
        )
    }

    fn restart_and_verify_docker_configuration(&mut self) -> Result<(), FailureMessage> {
        let backend = self.supervisor_backend()?;
        for (program, args) in backend.docker_commands(SupervisorChange::Restart) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        self.runner.docker_info()
    }

    fn install_units_and_enable_ready_roles(&mut self) -> Result<(), FailureMessage> {
        let backend = self.supervisor_backend()?;
        let ployzd =
            artifact_target(ArtifactKind::Ployzd, &self.artifacts.ployzd).map_err(failure)?;
        let environment =
            PloyzdRoleEnvironmentFile::new(self.state.path().join(ENV_FILE)).map_err(failure)?;
        for role in [
            PloyzdRole::Keeper,
            PloyzdRole::Api,
            PloyzdRole::Gateway,
            PloyzdRole::Dns,
        ] {
            let spec = SupervisorUnitSpec::PloyzdRole {
                role,
                artifact: ployzd.clone(),
                environment_file: environment.clone(),
            };
            let rendered = backend.render(&spec).map_err(failure)?;
            write_durable_file(
                self.supervisor_directories.directory(backend),
                rendered.file_name(),
                FileMode::Executable0755,
                rendered.contents().as_bytes(),
            )?;
            let target = spec.target();
            let changes: &[SupervisorChange] = match founding_role_disposition(role) {
                FoundingRoleDisposition::Enabled => &[SupervisorChange::Enable],
                FoundingRoleDisposition::DisabledAndInactive => {
                    &[SupervisorChange::Disable, SupervisorChange::Stop]
                }
            };
            for change in changes {
                for (program, args) in backend.commands(*change, &target) {
                    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
                    self.require(program, &refs)?;
                }
            }
        }
        install_corrosion_unit(
            backend,
            &self.supervisor_directories,
            self.state.path().join(CORROSION_CONFIG_FILE),
        )?;
        for (program, args) in corrosion_commands(backend, SupervisorChange::Enable) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        Ok(())
    }

    fn start_keeper(&mut self) -> Result<(), FailureMessage> {
        self.restart_role(PloyzdRole::Keeper)
    }

    fn start_corrosion(&mut self) -> Result<(), FailureMessage> {
        let backend = self.supervisor_backend()?;
        for (program, args) in corrosion_commands(backend, SupervisorChange::Restart) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        Ok(())
    }

    fn start_api_with_bootstrap(&mut self) -> Result<(), FailureMessage> {
        self.restart_role(PloyzdRole::Api)
    }

    fn await_driver_peer_convergence(
        &mut self,
        driver: &FoundingDriverEnrollment,
    ) -> Result<(), FailureMessage> {
        let Some((_peer_id, document)) = driver.enrolled_peer() else {
            return Ok(());
        };
        let PeerTransport::Wireguard { pubkey, .. } = &document.transport else {
            return Err(failure("founding driver transport is not WireGuard"));
        };
        for _ in 0..30 {
            let output = self.runner.command("wg", &["show", "ployz0", "peers"])?;
            if output.success
                && output
                    .stdout
                    .lines()
                    .any(|line| line.trim() == pubkey.as_str())
            {
                return Ok(());
            }
            thread::sleep(Duration::from_secs(1));
        }
        Err(failure(
            "Keeper did not converge the founding driver peer within 30 seconds",
        ))
    }

    fn remove_bootstrap_credential(&mut self) -> Result<(), FailureMessage> {
        write_durable_file(
            self.state.path(),
            ENV_FILE,
            FileMode::Secret0600,
            &self.env_contents(false)?,
        )?;
        match fs::remove_file(self.state.path().join(BOOTSTRAP_CREDENTIAL_FILE)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(failure(error)),
        }
    }

    fn restart_api_without_bootstrap(&mut self) -> Result<(), FailureMessage> {
        self.restart_role(PloyzdRole::Api)
    }
}

impl<R: HostRunnerCommandRunner> LinuxFoundingHostEffects<R> {
    fn restart_role(&mut self, role: PloyzdRole) -> Result<(), FailureMessage> {
        let backend = self.supervisor_backend()?;
        let target = crate::SupervisorUnitTarget::PloyzdRole(role);
        for (program, args) in backend.commands(SupervisorChange::Restart, &target) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        Ok(())
    }
}

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

fn install_corrosion_unit(
    backend: SupervisorBackend,
    directories: &SupervisorDirectories,
    config: PathBuf,
) -> Result<(), FailureMessage> {
    let (name, contents) = match backend {
        SupervisorBackend::Systemd => (
            "ployz-corrosion.service",
            format!(
                "[Unit]\nDescription=Ployz Corrosion\nAfter=network-online.target ployzd-keeper.service\nWants=network-online.target\n\n[Service]\nType=exec\nExecStart=/usr/local/bin/corrosion --config {} agent\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n",
                config.display()
            ),
        ),
        SupervisorBackend::OpenRc => (
            "ployz-corrosion",
            format!(
                "#!/sbin/openrc-run\nname=ployz-corrosion\nsupervisor=supervise-daemon\ncommand=/usr/local/bin/corrosion\ncommand_args=\"--config {} agent\"\nrespawn_delay=5\n\ndepend() {{ need net; after ployzd-keeper; }}\n",
                config.display()
            ),
        ),
    };
    write_durable_file(
        directories.directory(backend),
        name,
        FileMode::Executable0755,
        contents.as_bytes(),
    )
}

fn corrosion_commands(
    backend: SupervisorBackend,
    change: SupervisorChange,
) -> Vec<(&'static str, Vec<String>)> {
    match (backend, change) {
        (SupervisorBackend::Systemd, SupervisorChange::Enable) => vec![
            ("systemctl", vec!["daemon-reload".to_owned()]),
            (
                "systemctl",
                vec!["enable".to_owned(), "ployz-corrosion.service".to_owned()],
            ),
        ],
        (SupervisorBackend::Systemd, SupervisorChange::Restart) => vec![(
            "systemctl",
            vec!["restart".to_owned(), "ployz-corrosion.service".to_owned()],
        )],
        (SupervisorBackend::OpenRc, SupervisorChange::Enable) => vec![(
            "rc-update",
            vec![
                "add".to_owned(),
                "ployz-corrosion".to_owned(),
                "default".to_owned(),
            ],
        )],
        (SupervisorBackend::OpenRc, SupervisorChange::Restart) => vec![(
            "rc-service",
            vec!["ployz-corrosion".to_owned(), "restart".to_owned()],
        )],
        _ => Vec::new(),
    }
}

fn failure(error: impl fmt::Display) -> FailureMessage {
    FailureMessage::try_new(error.to_string()).unwrap_or_else(|_| {
        FailureMessage::try_new("founding host effect failed").expect("constant is non-empty")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HostRunnerCommandOutput;

    #[test]
    fn founding_enables_only_implemented_roles() {
        assert_eq!(
            founding_role_disposition(PloyzdRole::Keeper),
            FoundingRoleDisposition::Enabled
        );
        assert_eq!(
            founding_role_disposition(PloyzdRole::Api),
            FoundingRoleDisposition::Enabled
        );
        assert_eq!(
            founding_role_disposition(PloyzdRole::Gateway),
            FoundingRoleDisposition::DisabledAndInactive
        );
        assert_eq!(
            founding_role_disposition(PloyzdRole::Dns),
            FoundingRoleDisposition::DisabledAndInactive
        );
    }

    #[derive(Debug)]
    struct FactsRunner;

    impl HostRunnerCommandRunner for FactsRunner {
        fn command(
            &mut self,
            program: &str,
            _args: &[&str],
        ) -> Result<HostRunnerCommandOutput, FailureMessage> {
            match program {
                "cat" => Ok(output(true, "MemTotal:       4194304 kB\n")),
                "zpool" => Ok(output(false, "")),
                _ => Err(failure("unexpected command")),
            }
        }

        fn is_linux(&mut self) -> bool {
            true
        }

        fn current_uid(&mut self) -> Result<u32, FailureMessage> {
            Ok(0)
        }

        fn download(&mut self, _url: &str, _destination: &Path) -> Result<(), FailureMessage> {
            Err(failure("download is not used during preparation"))
        }

        fn docker_info(&mut self) -> Result<(), FailureMessage> {
            Ok(())
        }

        fn docker_is_installed(&mut self) -> bool {
            true
        }

        fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
            Ok(true)
        }

        fn docker_has_insecure_registry(&mut self, _cidr: &str) -> Result<bool, FailureMessage> {
            Ok(true)
        }
    }

    #[test]
    fn preparation_builds_matching_request_without_persisting_generated_material() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = FoundingStateDirectory::initialize(directory.path().join("state"))
            .expect("state initializes");
        let prepared = prepare_linux_founding(
            &state,
            LinuxFoundingInput {
                cluster_name: "ares".to_owned(),
                machine_name: MachineName::try_new("ares").expect("machine name"),
                endpoint: Some("203.0.113.7:51820".parse().expect("endpoint")),
                prefix: MachineEndpointSupernet::default_v1(),
                hostname_mode: AutomaticHostnameMode::Disabled,
                storage: InitStorageChoice::Automatic,
                driver: FoundingDriverInput::OnHost,
                written_at: CorrosionTimestamp::try_new("2026-08-04T12:00:00Z").expect("timestamp"),
                acme_directory_url: "https://acme.example/directory".to_owned(),
                acme_contact: None,
            },
            fixture_artifacts(),
            "corrosion 0.2.0-beta.0".to_owned(),
            FactsRunner,
        )
        .expect("preparation succeeds");

        assert_eq!(
            prepared.request.request().machine.storage.mode,
            StorageMode::Plain
        );
        assert!(!state.path().join("cluster-id").exists());
        assert!(!state.path().join(MACHINE_SEED_FILE).exists());
        let debug = format!("{:?}", prepared.effects);
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(prepared.bootstrap_credential.as_str()));
    }

    #[derive(Debug)]
    struct RecordingRunner {
        calls: Vec<String>,
        allow_facts: bool,
    }

    impl HostRunnerCommandRunner for RecordingRunner {
        fn command(
            &mut self,
            program: &str,
            args: &[&str],
        ) -> Result<HostRunnerCommandOutput, FailureMessage> {
            self.calls.push(format!("{program} {}", args.join(" ")));
            match program {
                "cat" if self.allow_facts => Ok(output(true, "MemTotal: 4194304 kB\n")),
                "zpool" if self.allow_facts => Ok(output(false, "")),
                _ => Err(failure(format!("unexpected command {program}"))),
            }
        }

        fn is_linux(&mut self) -> bool {
            true
        }

        fn current_uid(&mut self) -> Result<u32, FailureMessage> {
            Ok(0)
        }

        fn download(&mut self, _url: &str, _destination: &Path) -> Result<(), FailureMessage> {
            Err(failure("unexpected download"))
        }

        fn docker_info(&mut self) -> Result<(), FailureMessage> {
            Ok(())
        }

        fn docker_is_installed(&mut self) -> bool {
            true
        }

        fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
            Ok(true)
        }

        fn docker_has_insecure_registry(&mut self, _cidr: &str) -> Result<bool, FailureMessage> {
            Ok(true)
        }
    }

    #[test]
    fn machine_material_persists_keys_without_mutating_keeper_owned_wireguard() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = FoundingStateDirectory::initialize(directory.path().join("state"))
            .expect("state initializes");
        let mut prepared = prepare_linux_founding(
            &state,
            input("2026-08-04T12:00:00Z"),
            fixture_artifacts(),
            "corrosion 0.2.0-beta.0".to_owned(),
            RecordingRunner {
                calls: Vec::new(),
                allow_facts: true,
            },
        )
        .expect("preparation succeeds");
        let commands_before_material = prepared.effects.runner.calls.len();

        prepared
            .effects
            .ensure_machine_identity_and_wireguard()
            .expect("first persistence succeeds");
        prepared
            .effects
            .ensure_machine_identity_and_wireguard()
            .expect("second persistence succeeds");

        let calls = &prepared.effects.runner.calls;
        assert_eq!(calls.len(), commands_before_material);
        assert!(state.path().join(MACHINE_SEED_FILE).is_file());
        assert!(state.path().join(WIREGUARD_KEY_FILE).is_file());
    }

    #[test]
    fn persisted_request_and_secrets_are_reloaded_without_host_fact_probes() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = FoundingStateDirectory::initialize(directory.path().join("state"))
            .expect("state initializes");
        let mut first = prepare_linux_founding(
            &state,
            input("2026-08-04T12:00:00Z"),
            fixture_artifacts(),
            "corrosion 0.2.0-beta.0".to_owned(),
            RecordingRunner {
                calls: Vec::new(),
                allow_facts: true,
            },
        )
        .expect("first preparation succeeds");
        first
            .effects
            .ensure_machine_identity_and_wireguard()
            .expect("machine material persists");
        first
            .effects
            .persist_validated_founding_request()
            .expect("request persists");
        first
            .effects
            .ensure_cluster_door_material()
            .expect("door persists");
        write_durable_file(
            state.path(),
            BOOTSTRAP_CREDENTIAL_FILE,
            FileMode::Secret0600,
            first.bootstrap_credential.as_str().as_bytes(),
        )
        .expect("bootstrap credential persists");
        write_durable_file(
            state.path(),
            CORROSION_TOKEN_FILE,
            FileMode::Secret0600,
            first.effects.corrosion_token.as_bytes(),
        )
        .expect("Corrosion token persists");
        state
            .persist_cluster_id_exclusive(&first.request.request().cluster_id)
            .expect("cluster anchor persists");
        let expected_request = serde_json::to_value(first.request.request()).expect("request wire");
        let expected_bootstrap = first.bootstrap_credential.clone();
        let expected_corrosion = first.effects.corrosion_token.clone();
        let expected_door = first.effects.door_material.fingerprint.clone();

        let second = prepare_linux_founding(
            &state,
            input("2026-08-05T12:00:00Z"),
            fixture_artifacts(),
            "corrosion 0.2.0-beta.0".to_owned(),
            RecordingRunner {
                calls: Vec::new(),
                allow_facts: false,
            },
        )
        .expect("resume preparation succeeds");

        assert_eq!(
            serde_json::to_value(second.request.request()).expect("request wire"),
            expected_request
        );
        assert_eq!(second.bootstrap_credential, expected_bootstrap);
        assert_eq!(second.effects.corrosion_token, expected_corrosion);
        assert_eq!(second.effects.door_material.fingerprint, expected_door);
        assert!(second.effects.runner.calls.is_empty());
    }

    fn input(timestamp: &str) -> LinuxFoundingInput {
        LinuxFoundingInput {
            cluster_name: "ares".to_owned(),
            machine_name: MachineName::try_new("ares").expect("machine name"),
            endpoint: Some("203.0.113.7:51820".parse().expect("endpoint")),
            prefix: MachineEndpointSupernet::default_v1(),
            hostname_mode: AutomaticHostnameMode::Disabled,
            storage: InitStorageChoice::Automatic,
            driver: FoundingDriverInput::OnHost,
            written_at: CorrosionTimestamp::try_new(timestamp).expect("timestamp"),
            acme_directory_url: "https://acme.example/directory".to_owned(),
            acme_contact: None,
        }
    }

    fn fixture_artifacts() -> ReleaseArtifacts {
        let spec = |name: &str, path: &str| ployz_core::install::InstallArtifactSpec {
            version: ployz_core::install::InstallArtifactVersion::try_new("v1").expect("version"),
            source: ployz_core::install::InstallArtifactSource::try_new(format!("/tmp/{name}"))
                .expect("source"),
            sha256: ployz_core::install::InstallSha256Digest::try_new(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .expect("digest"),
            install_path: ployz_core::install::AbsoluteInstallPath::try_new(path)
                .expect("install path"),
        };
        ReleaseArtifacts {
            ployzd: spec("ployzd", "/usr/local/bin/ployzd"),
            ebpf_bytecode: spec("ebpf", "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"),
            ebpf_ctl: spec("ebpf-ctl", "/usr/local/bin/ployz-ebpf-ctl"),
            corrosion: spec("corrosion", "/usr/local/bin/corrosion"),
            corrosion_schema: spec("schema", "/usr/local/lib/ployz/corrosion-schema-v1.sql"),
            railpack: spec("railpack", "/usr/local/bin/railpack"),
        }
    }

    fn output(success: bool, stdout: &str) -> HostRunnerCommandOutput {
        HostRunnerCommandOutput {
            success,
            exit_code: success.then_some(0),
            stdout: stdout.to_owned(),
            stdout_truncated: false,
            failure: if success {
                String::new()
            } else {
                "not installed".to_owned()
            },
        }
    }
}
