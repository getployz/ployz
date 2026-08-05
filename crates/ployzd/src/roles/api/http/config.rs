//! Environment-backed configuration for the API role.

use std::env;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hmac::{Hmac, Mac};
use ployz_core::MachineUpgradeSupervisor;
use ployz_core::ids::{ClusterId, MachineRowId};
use ployz_core::join::JOIN_DOOR_PORT;
use ployz_host_runner::{
    API_EVIDENCE_DIRECTORY, API_UPGRADE_STAGING_DIRECTORY, ArtifactStoreError, CONTROL_SOCKET_PATH,
    PloyzdArtifactStore,
};
use sha2::Sha256;

use crate::corrosion::{BearerToken, CorrosionClientBounds, CorrosionClientConfig};

const CORROSION_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CORROSION_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CORROSION_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
const CORROSION_MAX_NDJSON_FRAME_BYTES: usize = 1_048_576;
const CORROSION_MAX_ERROR_BODY_BYTES: usize = 65_536;
const MAX_BUILD_DIAGNOSTIC_BYTES: usize = 512;

const CORROSION_API_ADDR_ENV: &str = "PLOYZ_CORROSION_API_ADDR";
const CORROSION_BEARER_TOKEN_ENV: &str = "PLOYZ_CORROSION_BEARER_TOKEN";
const CLUSTER_ID_ENV: &str = "PLOYZ_CLUSTER_ID";
const MACHINE_ID_ENV: &str = "PLOYZ_MACHINE_ID";
const API_LISTEN_ADDR_ENV: &str = "PLOYZ_API_LISTEN_ADDR";
const API_DOOR_LISTEN_ADDR_ENV: &str = "PLOYZ_API_DOOR_LISTEN_ADDR";
const API_DOOR_PRIVATE_KEY_PATH_ENV: &str = "PLOYZ_API_DOOR_PRIVATE_KEY_PATH";
const API_DOOR_CERTIFICATE_PATH_ENV: &str = "PLOYZ_API_DOOR_CERTIFICATE_PATH";
const API_DOOR_FINGERPRINT_PATH_ENV: &str = "PLOYZ_API_DOOR_FINGERPRINT_PATH";
const API_JOIN_SUBSTRATE_PATH_ENV: &str = "PLOYZ_API_JOIN_SUBSTRATE_PATH";
const CORROSION_GOSSIP_PORT_ENV: &str = "PLOYZ_CORROSION_GOSSIP_PORT";
const BUILD_ENV: &str = "PLOYZ_BUILD";
const BOOTSTRAP_SECRET_ENV: &str = "PLOYZ_API_BOOTSTRAP_SECRET";
const UPGRADE_STATE_DIR_ENV: &str = "PLOYZ_UPGRADE_STATE_DIR";
const UPGRADE_SOCKET_PATH_ENV: &str = "PLOYZ_UPGRADE_SOCKET_PATH";
const CONTROL_SOCKET_PATH_ENV: &str = "PLOYZ_CONTROL_SOCKET_PATH";
const SUPERVISOR_BACKEND_ENV: &str = "PLOYZ_SUPERVISOR_BACKEND";
const MAX_BOOTSTRAP_SECRET_BYTES: usize = 4_096;
const BOOTSTRAP_SECRET_DOMAIN: &[u8] = b"ployz-api-founding-v1";

const DEFAULT_DOOR_PRIVATE_KEY_PATH: &str = "/var/lib/ployz/door.key";
const DEFAULT_DOOR_CERTIFICATE_PATH: &str = "/var/lib/ployz/door.crt";
const DEFAULT_DOOR_FINGERPRINT_PATH: &str = "/var/lib/ployz/door.fingerprint";
const DEFAULT_JOIN_SUBSTRATE_PATH: &str = "/var/lib/ployz/join-substrate.json";
const DEFAULT_CORROSION_GOSSIP_PORT: u16 = 8_787;
const DEFAULT_UPGRADE_STATE_DIR: &str = "/var/lib/ployz";
const DEFAULT_UPGRADE_SOCKET_PATH: &str = "/run/ployz/keeper-upgrade.sock";
const DEFAULT_SUPERVISOR_BACKEND: &str = "systemd";

type BootstrapMac = Hmac<Sha256>;

#[derive(Debug, Clone)]
struct JoinDoorConfig {
    listen_addr: DoorListenAddress,
    material: DoorMaterialPaths,
    substrate_path: PathBuf,
    corrosion_gossip_port: u16,
}

#[derive(Debug, Clone)]
struct ApiUpgradeConfig {
    store: PloyzdArtifactStore,
    keeper_socket_path: PathBuf,
    supervisor: MachineUpgradeSupervisor,
}

#[derive(Debug, Clone)]
struct ApiRoleDetails {
    door: JoinDoorConfig,
    build: String,
    mode: ApiRoleMode,
    upgrade: ApiUpgradeConfig,
    evidence_directory: PathBuf,
    keeper_control_socket_path: PathBuf,
}

impl JoinDoorConfig {
    fn defaults() -> Self {
        Self {
            listen_addr: DoorListenAddress::default(),
            material: DoorMaterialPaths::defaults(),
            substrate_path: PathBuf::from(DEFAULT_JOIN_SUBSTRATE_PATH),
            corrosion_gossip_port: DEFAULT_CORROSION_GOSSIP_PORT,
        }
    }
}

/// The public join door listener. Unlike the mesh API listener, the door is
/// intentionally allowed to bind a wildcard address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoorListenAddress(SocketAddr);

impl DoorListenAddress {
    pub fn try_new(address: SocketAddr) -> Result<Self, ApiRoleConfigError> {
        if address.port() == 0 {
            return Err(ApiRoleConfigError::ZeroDoorListenPort);
        }
        if address.port() != JOIN_DOOR_PORT {
            return Err(ApiRoleConfigError::UnexpectedDoorListenPort {
                expected: JOIN_DOOR_PORT,
                actual: address.port(),
            });
        }
        Ok(Self(address))
    }

    #[must_use]
    pub const fn get(self) -> SocketAddr {
        self.0
    }
}

impl Default for DoorListenAddress {
    fn default() -> Self {
        Self(SocketAddr::new(
            std::net::Ipv6Addr::UNSPECIFIED.into(),
            JOIN_DOOR_PORT,
        ))
    }
}

/// The API startup authority mode.
#[derive(Clone)]
pub enum ApiRoleMode {
    /// The listener and every caller are authenticated from converged roster rows.
    Ordinary,
    /// The one local founding endpoint is available before roster rows exist.
    Founding(BootstrapSecret),
}

impl fmt::Debug for ApiRoleMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ordinary => formatter.write_str("Ordinary"),
            Self::Founding(_) => formatter.write_str("Founding([REDACTED])"),
        }
    }
}

/// A bootstrap credential retained only as a one-way authentication tag.
#[derive(Clone)]
pub struct BootstrapSecret([u8; 32]);

impl BootstrapSecret {
    pub fn new(value: impl AsRef<[u8]>) -> Result<Self, ApiRoleConfigError> {
        let value = value.as_ref();
        if value.is_empty() {
            return Err(ApiRoleConfigError::EmptyBootstrapSecret);
        }
        if value.len() > MAX_BOOTSTRAP_SECRET_BYTES {
            return Err(ApiRoleConfigError::BootstrapSecretTooLong {
                limit: MAX_BOOTSTRAP_SECRET_BYTES,
            });
        }
        Ok(Self(bootstrap_tag(value)))
    }

    #[must_use]
    pub fn verifies(&self, candidate: &[u8]) -> bool {
        let Ok(mut verifier) = BootstrapMac::new_from_slice(candidate) else {
            return false;
        };
        verifier.update(BOOTSTRAP_SECRET_DOMAIN);
        verifier.verify_slice(&self.0).is_ok()
    }
}

impl fmt::Debug for BootstrapSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BootstrapSecret([REDACTED])")
    }
}

fn bootstrap_tag(value: &[u8]) -> [u8; 32] {
    let mut mac = BootstrapMac::new_from_slice(value)
        .expect("HMAC accepts bootstrap credentials of every byte length");
    mac.update(BOOTSTRAP_SECRET_DOMAIN);
    mac.finalize().into_bytes().into()
}

/// Configuration supplied by the local `ployzd` role environment file.
///
/// The listener still undergoes roster-backed validation at startup: a
/// syntactically concrete address is not enough to prove it is this machine's
/// mesh address.
#[derive(Debug, Clone)]
pub struct ApiRoleConfig {
    corrosion: CorrosionClientConfig,
    cluster_id: ClusterId,
    local_machine_id: MachineRowId,
    listen_addr: SocketAddr,
    door: JoinDoorConfig,
    build: String,
    mode: ApiRoleMode,
    upgrade: ApiUpgradeConfig,
    evidence_directory: PathBuf,
    keeper_control_socket_path: PathBuf,
}

impl ApiRoleConfig {
    /// Parses the API role's local environment-file inputs.
    pub fn from_environment() -> Result<Self, ApiRoleConfigError> {
        let corrosion_api_addr = environment_value(CORROSION_API_ADDR_ENV)?
            .parse::<SocketAddr>()
            .map_err(|error| ApiRoleConfigError::InvalidSocketAddress {
                name: CORROSION_API_ADDR_ENV,
                detail: error.to_string(),
            })?;
        let bearer_token = BearerToken::new(environment_value(CORROSION_BEARER_TOKEN_ENV)?)
            .map_err(ApiRoleConfigError::CorrosionConfig)?;
        let corrosion = CorrosionClientConfig::new(
            corrosion_api_addr,
            bearer_token,
            CorrosionClientBounds {
                connect_timeout: CORROSION_CONNECT_TIMEOUT,
                request_timeout: CORROSION_REQUEST_TIMEOUT,
                stream_idle_timeout: CORROSION_STREAM_IDLE_TIMEOUT,
                max_ndjson_frame_bytes: CORROSION_MAX_NDJSON_FRAME_BYTES,
                max_error_body_bytes: CORROSION_MAX_ERROR_BODY_BYTES,
            },
        )
        .map_err(ApiRoleConfigError::CorrosionConfig)?;
        let cluster_id =
            ClusterId::try_new(environment_value(CLUSTER_ID_ENV)?).map_err(|error| {
                ApiRoleConfigError::InvalidClusterId {
                    detail: error.to_string(),
                }
            })?;
        let local_machine_id =
            MachineRowId::try_new(environment_value(MACHINE_ID_ENV)?).map_err(|error| {
                ApiRoleConfigError::InvalidMachineId {
                    detail: error.to_string(),
                }
            })?;
        let listen_addr = environment_value(API_LISTEN_ADDR_ENV)?
            .parse::<SocketAddr>()
            .map_err(|error| ApiRoleConfigError::InvalidSocketAddress {
                name: API_LISTEN_ADDR_ENV,
                detail: error.to_string(),
            })?;
        let door_listen_addr = match env::var(API_DOOR_LISTEN_ADDR_ENV) {
            Ok(value) => {
                DoorListenAddress::try_new(value.parse::<SocketAddr>().map_err(|error| {
                    ApiRoleConfigError::InvalidSocketAddress {
                        name: API_DOOR_LISTEN_ADDR_ENV,
                        detail: error.to_string(),
                    }
                })?)?
            }
            Err(env::VarError::NotPresent) => DoorListenAddress::default(),
            Err(env::VarError::NotUnicode(_)) => {
                return Err(ApiRoleConfigError::NonUnicodeEnvironment {
                    name: API_DOOR_LISTEN_ADDR_ENV,
                });
            }
        };
        let door_private_key_path = optional_path_environment(
            API_DOOR_PRIVATE_KEY_PATH_ENV,
            DEFAULT_DOOR_PRIVATE_KEY_PATH,
        )?;
        let door_certificate_path = optional_path_environment(
            API_DOOR_CERTIFICATE_PATH_ENV,
            DEFAULT_DOOR_CERTIFICATE_PATH,
        )?;
        let door_fingerprint_path = optional_path_environment(
            API_DOOR_FINGERPRINT_PATH_ENV,
            DEFAULT_DOOR_FINGERPRINT_PATH,
        )?;
        let join_substrate_path =
            optional_path_environment(API_JOIN_SUBSTRATE_PATH_ENV, DEFAULT_JOIN_SUBSTRATE_PATH)?;
        let corrosion_gossip_port =
            optional_port_environment(CORROSION_GOSSIP_PORT_ENV, DEFAULT_CORROSION_GOSSIP_PORT)?;
        let build = validate_build_diagnostic(environment_value(BUILD_ENV)?)?;
        let upgrade_state =
            optional_path_environment(UPGRADE_STATE_DIR_ENV, DEFAULT_UPGRADE_STATE_DIR)?;
        let upgrade_socket_path =
            optional_path_environment(UPGRADE_SOCKET_PATH_ENV, DEFAULT_UPGRADE_SOCKET_PATH)?;
        let upgrade = ApiUpgradeConfig {
            store: api_upgrade_store(upgrade_state.clone())?,
            keeper_socket_path: upgrade_socket_path,
            supervisor: upgrade_supervisor_environment()?,
        };
        let evidence_directory = upgrade_state.join(API_EVIDENCE_DIRECTORY);
        let keeper_control_socket_path =
            optional_path_environment(CONTROL_SOCKET_PATH_ENV, CONTROL_SOCKET_PATH)?;

        let mode = match env::var(BOOTSTRAP_SECRET_ENV) {
            Ok(value) => ApiRoleMode::Founding(BootstrapSecret::new(value.as_bytes())?),
            Err(env::VarError::NotPresent) => ApiRoleMode::Ordinary,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(ApiRoleConfigError::NonUnicodeEnvironment {
                    name: BOOTSTRAP_SECRET_ENV,
                });
            }
        };

        Self::new_with_details(
            corrosion,
            cluster_id,
            local_machine_id,
            listen_addr,
            ApiRoleDetails {
                door: JoinDoorConfig {
                    listen_addr: door_listen_addr,
                    material: DoorMaterialPaths::new(
                        door_private_key_path,
                        door_certificate_path,
                        door_fingerprint_path,
                    )?,
                    substrate_path: join_substrate_path,
                    corrosion_gossip_port,
                },
                build,
                mode,
                upgrade,
                evidence_directory,
                keeper_control_socket_path,
            },
        )
    }

    /// Builds validated production configuration without reading the process
    /// environment. Tests and process wiring use this to keep credentials out
    /// of command-line arguments.
    pub fn new(
        corrosion: CorrosionClientConfig,
        cluster_id: ClusterId,
        local_machine_id: MachineRowId,
        listen_addr: SocketAddr,
        build: String,
    ) -> Result<Self, ApiRoleConfigError> {
        Self::new_with_mode(
            corrosion,
            cluster_id,
            local_machine_id,
            listen_addr,
            build,
            ApiRoleMode::Ordinary,
        )
    }

    /// Builds founding-mode configuration around a local bootstrap credential.
    pub fn new_founding(
        corrosion: CorrosionClientConfig,
        cluster_id: ClusterId,
        local_machine_id: MachineRowId,
        listen_addr: SocketAddr,
        build: String,
        bootstrap_secret: BootstrapSecret,
    ) -> Result<Self, ApiRoleConfigError> {
        Self::new_with_mode(
            corrosion,
            cluster_id,
            local_machine_id,
            listen_addr,
            build,
            ApiRoleMode::Founding(bootstrap_secret),
        )
    }

    fn new_with_mode(
        corrosion: CorrosionClientConfig,
        cluster_id: ClusterId,
        local_machine_id: MachineRowId,
        listen_addr: SocketAddr,
        build: String,
        mode: ApiRoleMode,
    ) -> Result<Self, ApiRoleConfigError> {
        Self::new_with_details(
            corrosion,
            cluster_id,
            local_machine_id,
            listen_addr,
            ApiRoleDetails {
                door: JoinDoorConfig::defaults(),
                build,
                mode,
                upgrade: ApiUpgradeConfig {
                    store: api_upgrade_store(PathBuf::from(DEFAULT_UPGRADE_STATE_DIR))
                        .expect("fixed upgrade state directory is absolute"),
                    keeper_socket_path: PathBuf::from(DEFAULT_UPGRADE_SOCKET_PATH),
                    supervisor: MachineUpgradeSupervisor::Systemd,
                },
                evidence_directory: PathBuf::from(DEFAULT_UPGRADE_STATE_DIR)
                    .join(API_EVIDENCE_DIRECTORY),
                keeper_control_socket_path: PathBuf::from(CONTROL_SOCKET_PATH),
            },
        )
    }

    fn new_with_details(
        corrosion: CorrosionClientConfig,
        cluster_id: ClusterId,
        local_machine_id: MachineRowId,
        listen_addr: SocketAddr,
        details: ApiRoleDetails,
    ) -> Result<Self, ApiRoleConfigError> {
        let ApiRoleDetails {
            door,
            build,
            mode,
            upgrade,
            evidence_directory,
            keeper_control_socket_path,
        } = details;
        if listen_addr.ip().is_unspecified() {
            return Err(ApiRoleConfigError::WildcardListenAddress { listen_addr });
        }
        if listen_addr.port() == 0 {
            return Err(ApiRoleConfigError::ZeroListenPort);
        }
        validate_absolute_path(API_JOIN_SUBSTRATE_PATH_ENV, &door.substrate_path)?;
        validate_absolute_path(UPGRADE_STATE_DIR_ENV, &evidence_directory)?;
        validate_absolute_path(CONTROL_SOCKET_PATH_ENV, &keeper_control_socket_path)?;
        if door.corrosion_gossip_port == 0 {
            return Err(ApiRoleConfigError::ZeroCorrosionGossipPort);
        }
        let build = validate_build_diagnostic(build)?;

        Ok(Self {
            corrosion,
            cluster_id,
            local_machine_id,
            listen_addr,
            door,
            build,
            mode,
            upgrade,
            evidence_directory,
            keeper_control_socket_path,
        })
    }

    #[must_use]
    pub fn corrosion(&self) -> &CorrosionClientConfig {
        &self.corrosion
    }

    #[must_use]
    pub fn cluster_id(&self) -> &ClusterId {
        &self.cluster_id
    }

    #[must_use]
    pub fn local_machine_id(&self) -> &MachineRowId {
        &self.local_machine_id
    }

    #[must_use]
    pub const fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    #[must_use]
    pub const fn door_listen_addr(&self) -> DoorListenAddress {
        self.door.listen_addr
    }

    #[must_use]
    pub fn door_private_key_path(&self) -> &Path {
        &self.door.material.private_key
    }

    #[must_use]
    pub fn door_certificate_path(&self) -> &Path {
        &self.door.material.certificate
    }

    #[must_use]
    pub fn door_fingerprint_path(&self) -> &Path {
        &self.door.material.fingerprint
    }

    #[must_use]
    pub fn join_substrate_path(&self) -> &Path {
        &self.door.substrate_path
    }

    #[must_use]
    pub const fn corrosion_gossip_port(&self) -> u16 {
        self.door.corrosion_gossip_port
    }

    #[must_use]
    pub fn build(&self) -> &str {
        &self.build
    }

    #[must_use]
    pub const fn mode(&self) -> &ApiRoleMode {
        &self.mode
    }

    #[must_use]
    pub(super) fn upgrade_store(&self) -> &PloyzdArtifactStore {
        &self.upgrade.store
    }

    #[must_use]
    pub(super) fn keeper_upgrade_socket_path(&self) -> &Path {
        &self.upgrade.keeper_socket_path
    }

    #[must_use]
    pub(super) const fn upgrade_supervisor(&self) -> MachineUpgradeSupervisor {
        self.upgrade.supervisor
    }

    #[must_use]
    pub(super) fn evidence_directory(&self) -> &Path {
        &self.evidence_directory
    }

    #[must_use]
    pub(super) fn keeper_control_socket_path(&self) -> &Path {
        &self.keeper_control_socket_path
    }
}

fn api_upgrade_store(state: PathBuf) -> Result<PloyzdArtifactStore, ApiRoleConfigError> {
    PloyzdArtifactStore::new(state.join(API_UPGRADE_STAGING_DIRECTORY))
        .map_err(ApiRoleConfigError::UpgradeStore)
}

/// Exact machine-local TLS material used by the public join door.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoorMaterialPaths {
    private_key: PathBuf,
    certificate: PathBuf,
    fingerprint: PathBuf,
}

impl DoorMaterialPaths {
    pub fn new(
        private_key: PathBuf,
        certificate: PathBuf,
        fingerprint: PathBuf,
    ) -> Result<Self, ApiRoleConfigError> {
        validate_absolute_path(API_DOOR_PRIVATE_KEY_PATH_ENV, &private_key)?;
        validate_absolute_path(API_DOOR_CERTIFICATE_PATH_ENV, &certificate)?;
        validate_absolute_path(API_DOOR_FINGERPRINT_PATH_ENV, &fingerprint)?;
        Ok(Self {
            private_key,
            certificate,
            fingerprint,
        })
    }

    fn defaults() -> Self {
        Self {
            private_key: PathBuf::from(DEFAULT_DOOR_PRIVATE_KEY_PATH),
            certificate: PathBuf::from(DEFAULT_DOOR_CERTIFICATE_PATH),
            fingerprint: PathBuf::from(DEFAULT_DOOR_FINGERPRINT_PATH),
        }
    }
}

fn environment_value(name: &'static str) -> Result<String, ApiRoleConfigError> {
    env::var(name).map_err(|error| match error {
        env::VarError::NotPresent => ApiRoleConfigError::MissingEnvironment { name },
        env::VarError::NotUnicode(_) => ApiRoleConfigError::NonUnicodeEnvironment { name },
    })
}

fn optional_path_environment(
    name: &'static str,
    default: &'static str,
) -> Result<PathBuf, ApiRoleConfigError> {
    let value = match env::var(name) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => default.to_owned(),
        Err(env::VarError::NotUnicode(_)) => {
            return Err(ApiRoleConfigError::NonUnicodeEnvironment { name });
        }
    };
    let path = PathBuf::from(value);
    validate_absolute_path(name, &path)?;
    Ok(path)
}

fn upgrade_supervisor_environment() -> Result<MachineUpgradeSupervisor, ApiRoleConfigError> {
    let value = match env::var(SUPERVISOR_BACKEND_ENV) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => DEFAULT_SUPERVISOR_BACKEND.to_owned(),
        Err(env::VarError::NotUnicode(_)) => {
            return Err(ApiRoleConfigError::NonUnicodeEnvironment {
                name: SUPERVISOR_BACKEND_ENV,
            });
        }
    };
    match value.as_str() {
        "systemd" => Ok(MachineUpgradeSupervisor::Systemd),
        "openrc" => Ok(MachineUpgradeSupervisor::OpenRc),
        _ => Err(ApiRoleConfigError::InvalidUpgradeSupervisor { value }),
    }
}

fn validate_absolute_path(name: &'static str, path: &Path) -> Result<(), ApiRoleConfigError> {
    if path.as_os_str().is_empty() {
        Err(ApiRoleConfigError::EmptyPath { name })
    } else if !path.is_absolute() {
        Err(ApiRoleConfigError::RelativePath {
            name,
            value: path.to_path_buf(),
        })
    } else {
        Ok(())
    }
}

fn optional_port_environment(name: &'static str, default: u16) -> Result<u16, ApiRoleConfigError> {
    let value = match env::var(name) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => return Ok(default),
        Err(env::VarError::NotUnicode(_)) => {
            return Err(ApiRoleConfigError::NonUnicodeEnvironment { name });
        }
    };
    let port = value
        .parse::<u16>()
        .map_err(|error| ApiRoleConfigError::InvalidPort {
            name,
            detail: error.to_string(),
        })?;
    if port == 0 {
        return Err(ApiRoleConfigError::ZeroCorrosionGossipPort);
    }
    Ok(port)
}

fn validate_build_diagnostic(value: String) -> Result<String, ApiRoleConfigError> {
    if value.trim().is_empty() {
        return Err(ApiRoleConfigError::EmptyBuildDiagnostic);
    }
    if value.len() > MAX_BUILD_DIAGNOSTIC_BYTES {
        return Err(ApiRoleConfigError::BuildDiagnosticTooLong {
            limit: MAX_BUILD_DIAGNOSTIC_BYTES,
        });
    }
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(ApiRoleConfigError::InvalidBuildDiagnostic);
    }
    Ok(value)
}

/// A local API-role startup configuration failure.
#[derive(Debug, thiserror::Error)]
pub enum ApiRoleConfigError {
    #[error("required API role environment variable {name} is missing")]
    MissingEnvironment { name: &'static str },
    #[error("API role environment variable {name} is not valid Unicode")]
    NonUnicodeEnvironment { name: &'static str },
    #[error("API role environment variable {name} is not a socket address: {detail}")]
    InvalidSocketAddress { name: &'static str, detail: String },
    #[error("API listener address {listen_addr} must not be wildcard")]
    WildcardListenAddress { listen_addr: SocketAddr },
    #[error("API listener port must be nonzero")]
    ZeroListenPort,
    #[error("join door listener port must be nonzero")]
    ZeroDoorListenPort,
    #[error("join door listener must use fixed port {expected}, got {actual}")]
    UnexpectedDoorListenPort { expected: u16, actual: u16 },
    #[error("API role path from {name} must not be empty")]
    EmptyPath { name: &'static str },
    #[error("API role path from {name} must be absolute: {value:?}")]
    RelativePath { name: &'static str, value: PathBuf },
    #[error("API role port from {name} is invalid: {detail}")]
    InvalidPort { name: &'static str, detail: String },
    #[error("Corrosion gossip port must not be zero")]
    ZeroCorrosionGossipPort,
    #[error("PLOYZ_CLUSTER_ID is invalid: {detail}")]
    InvalidClusterId { detail: String },
    #[error("PLOYZ_MACHINE_ID is invalid: {detail}")]
    InvalidMachineId { detail: String },
    #[error("PLOYZ_BUILD must not be empty")]
    EmptyBuildDiagnostic,
    #[error("PLOYZ_BUILD exceeds the {limit}-byte diagnostic limit")]
    BuildDiagnosticTooLong { limit: usize },
    #[error("PLOYZ_BUILD must not contain control characters")]
    InvalidBuildDiagnostic,
    #[error("PLOYZ_API_BOOTSTRAP_SECRET must not be empty")]
    EmptyBootstrapSecret,
    #[error("PLOYZ_API_BOOTSTRAP_SECRET exceeds the {limit}-byte limit")]
    BootstrapSecretTooLong { limit: usize },
    #[error("invalid local Corrosion configuration: {0}")]
    CorrosionConfig(#[source] crate::corrosion::CorrosionClientConfigError),
    #[error("invalid local upgrade artifact store: {0}")]
    UpgradeStore(#[source] ArtifactStoreError),
    #[error("PLOYZ_SUPERVISOR_BACKEND must be systemd or openrc, got {value:?}")]
    InvalidUpgradeSupervisor { value: String },
}

#[cfg(test)]
mod upgrade_staging_tests {
    use super::*;

    #[test]
    fn api_upgrade_store_is_confined_to_the_exported_staging_directory() {
        let store = api_upgrade_store(PathBuf::from("/var/lib/ployz"))
            .expect("absolute API upgrade staging store");

        assert_eq!(
            store.state(),
            Path::new("/var/lib/ployz").join(API_UPGRADE_STAGING_DIRECTORY)
        );
        assert_ne!(store.state(), Path::new(DEFAULT_UPGRADE_STATE_DIR));
    }
}
