//! Durable operator-peer identity and ready context for reaching a cluster.

use std::fmt;
use std::fs;
use std::io::Write as _;
use std::net::{Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};

use defguard_wireguard_rs::key::Key;
use ployz_core::corrosion::{
    BuiltinWireguardMemberSubnet, MachineTransport, MeshProvider, derive_builtin_wireguard_member,
};
use ployz_core::ids::{ClusterName, PeerName};
use ployz_core::network::WireGuardPublicKey;
use serde::{Deserialize, Serialize};

use crate::commands::SshTarget;

pub const SSH_CONTEXT_HANDOFF_PREFIX: &str = "PLOYZ_SSH_CONTEXT_V1 ";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshPeerKey {
    pub peer_id: PeerName,
    private_key: String,
    pub public_key: WireGuardPublicKey,
}

impl fmt::Debug for SshPeerKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SshPeerKey")
            .field("peer_id", &self.peer_id)
            .field("private_key", &"[REDACTED]")
            .field("public_key", &self.public_key)
            .finish()
    }
}

impl SshPeerKey {
    pub fn generate(peer_name: String) -> Result<Self, OperatorContextError> {
        let private_key = Key::generate();
        let public_key = WireGuardPublicKey::try_new(private_key.public_key().to_string())
            .map_err(|error| OperatorContextError::InvalidKey(error.to_string()))?;
        let peer_id = PeerName::try_new(peer_name)
            .map_err(|error| OperatorContextError::InvalidDocument(error.to_string()))?;
        Ok(Self {
            peer_id,
            private_key: private_key.to_string(),
            public_key,
        })
    }

    pub fn load(config_home: &Path, target: &SshTarget) -> Result<Self, OperatorContextError> {
        OperatorContextStore::new(config_home).load_peer(target)
    }

    pub fn persist_new(
        &self,
        config_home: &Path,
        target: &SshTarget,
    ) -> Result<(), OperatorContextError> {
        OperatorContextStore::new(config_home).persist_peer_new(target, self)
    }

    pub fn load_or_create(
        config_home: &Path,
        target: &SshTarget,
        peer_name: String,
    ) -> Result<Self, OperatorContextError> {
        OperatorContextStore::new(config_home).load_or_create_peer(target, peer_name)
    }

    pub fn private_key_bytes(&self) -> Result<[u8; 32], OperatorContextError> {
        self.private_key().map(OperatorWireguardPrivateKey::bytes)
    }

    fn private_key(&self) -> Result<OperatorWireguardPrivateKey, OperatorContextError> {
        Key::try_from(self.private_key.as_str())
            .map(|key| OperatorWireguardPrivateKey(key.as_array()))
            .map_err(|error| OperatorContextError::InvalidKey(error.to_string()))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OperatorWireguardPrivateKey([u8; 32]);

impl OperatorWireguardPrivateKey {
    #[must_use]
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Debug for OperatorWireguardPrivateKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OperatorWireguardPrivateKey([REDACTED])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshContextHandoff {
    pub cluster_id: ClusterName,
    pub provider: MeshProvider,
    pub machine_transport: MachineTransport,
}

impl SshContextHandoff {
    pub fn decode_handoff(line: &str) -> Result<Self, OperatorContextError> {
        let Some(payload) = line.strip_prefix(SSH_CONTEXT_HANDOFF_PREFIX) else {
            return Err(OperatorContextError::MissingHandoffPrefix);
        };
        let context: Self = serde_json::from_str(payload)
            .map_err(|error| OperatorContextError::InvalidDocument(error.to_string()))?;
        Ok(context)
    }

    pub fn encode_handoff(&self) -> Result<String, OperatorContextError> {
        let payload = serde_json::to_string(self)
            .map_err(|error| OperatorContextError::InvalidDocument(error.to_string()))?;
        Ok(format!("{SSH_CONTEXT_HANDOFF_PREFIX}{payload}\n"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorContext {
    pub v: OperatorContextVersion,
    pub target: SshTarget,
    pub peer_id: PeerName,
    pub peer_public_key: WireGuardPublicKey,
    pub cluster_id: ClusterName,
    pub provider: MeshProvider,
    pub machine_transport: MachineTransport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorContextVersion {
    V1,
}

impl OperatorContext {
    #[must_use]
    pub fn from_handoff(
        target: &SshTarget,
        peer_key: &SshPeerKey,
        handoff: SshContextHandoff,
    ) -> Self {
        Self {
            v: OperatorContextVersion::V1,
            target: target.clone(),
            peer_id: peer_key.peer_id.clone(),
            peer_public_key: peer_key.public_key.clone(),
            cluster_id: handoff.cluster_id,
            provider: handoff.provider,
            machine_transport: handoff.machine_transport,
        }
    }

    fn validate_identity(
        &self,
        target: &SshTarget,
        peer_key: &SshPeerKey,
    ) -> Result<(), OperatorContextError> {
        if self.target != *target {
            return Err(OperatorContextError::TargetMismatch {
                expected: target.clone(),
                found: self.target.clone(),
            });
        }
        if self.peer_id != peer_key.peer_id || self.peer_public_key != peer_key.public_key {
            return Err(OperatorContextError::PeerIdentityMismatch);
        }
        Ok(())
    }
}

pub struct OperatorContextStore {
    config_home: PathBuf,
}

impl OperatorContextStore {
    #[must_use]
    pub fn new(config_home: impl Into<PathBuf>) -> Self {
        Self {
            config_home: config_home.into(),
        }
    }

    pub fn persist(
        &self,
        target: &SshTarget,
        handoff: SshContextHandoff,
        peer_key: &SshPeerKey,
    ) -> Result<(), OperatorContextError> {
        let stored_peer_key = self.load_peer(target)?;
        if stored_peer_key != *peer_key {
            return Err(OperatorContextError::PeerIdentityMismatch);
        }
        let context = OperatorContext::from_handoff(target, peer_key, handoff);
        context.clone().normalize(target, peer_key)?;
        let path = context_path(&self.config_home, target);
        secure_publish_json(&path, &context, Publication::Replace)
    }

    pub fn load_or_create_peer(
        &self,
        target: &SshTarget,
        peer_name: String,
    ) -> Result<SshPeerKey, OperatorContextError> {
        match self.load_peer(target) {
            Ok(key) => return Ok(key),
            Err(OperatorContextError::Filesystem(error))
                if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let key = SshPeerKey::generate(peer_name)?;
        match self.persist_peer_new(target, &key) {
            Ok(()) => Ok(key),
            Err(OperatorContextError::Filesystem(error))
                if error.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                self.load_peer(target)
            }
            Err(error) => Err(error),
        }
    }

    pub fn persist_peer_new(
        &self,
        target: &SshTarget,
        key: &SshPeerKey,
    ) -> Result<(), OperatorContextError> {
        validate_key(key)?;
        secure_publish_json(
            &key_path(&self.config_home, target),
            key,
            Publication::CreateNew,
        )
    }

    pub fn load_peer(&self, target: &SshTarget) -> Result<SshPeerKey, OperatorContextError> {
        let path = key_path(&self.config_home, target);
        read_regular_private_file(&path).and_then(|bytes| decode_key(&bytes))
    }

    pub fn load_target(
        &self,
        target: &SshTarget,
    ) -> Result<LoadedOperatorContext, OperatorContextError> {
        let path = context_path(&self.config_home, target);
        let bytes = match read_regular_private_file(&path) {
            Ok(bytes) => bytes,
            Err(OperatorContextError::Filesystem(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                return Err(OperatorContextError::MissingContext {
                    target: target.clone(),
                });
            }
            Err(error) => return Err(error),
        };
        let context: OperatorContext = serde_json::from_slice(&bytes)
            .map_err(|error| OperatorContextError::InvalidDocument(error.to_string()))?;
        let peer_key = self.load_peer(target)?;
        context.normalize(target, &peer_key)
    }

    pub fn load_all(&self) -> Result<Vec<LoadedOperatorContext>, OperatorContextError> {
        let directory = context_directory(&self.config_home);
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(OperatorContextError::Filesystem(error)),
        };
        let mut contexts = Vec::new();
        let mut removed_temporary = false;
        for entry in entries {
            let entry = entry.map_err(OperatorContextError::Filesystem)?;
            let file_type = entry
                .file_type()
                .map_err(OperatorContextError::Filesystem)?;
            if is_context_temporary(&entry.file_name()) && file_type.is_file() {
                fs::remove_file(entry.path()).map_err(OperatorContextError::Filesystem)?;
                removed_temporary = true;
                continue;
            }
            if !file_type.is_file() {
                return Err(OperatorContextError::UnsafeContextFile);
            }
            let bytes = read_regular_private_file(&entry.path())?;
            let context: OperatorContext = serde_json::from_slice(&bytes)
                .map_err(|error| OperatorContextError::InvalidDocument(error.to_string()))?;
            let target = context.target.clone();
            if entry.path() != context_path(&self.config_home, &target) {
                return Err(OperatorContextError::TargetFilenameMismatch {
                    target: context.target,
                });
            }
            let peer_key = self.load_peer(&target)?;
            contexts.push(context.normalize(&target, &peer_key)?);
        }
        if removed_temporary {
            sync_directory(&context_directory(&self.config_home))?;
        }
        contexts.sort_by(|left, right| left.target().as_str().cmp(right.target().as_str()));
        Ok(contexts)
    }
}

#[derive(Debug)]
pub enum LoadedOperatorContext {
    BuiltinWireguard(BuiltinWireguardOperatorContext),
    UnsupportedProvider(UnsupportedOperatorContext),
}

impl LoadedOperatorContext {
    #[must_use]
    pub const fn target(&self) -> &SshTarget {
        match self {
            Self::BuiltinWireguard(context) => &context.target,
            Self::UnsupportedProvider(context) => &context.target,
        }
    }

    #[must_use]
    pub const fn cluster_id(&self) -> &ClusterName {
        match self {
            Self::BuiltinWireguard(context) => &context.cluster_id,
            Self::UnsupportedProvider(context) => &context.cluster_id,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct BuiltinWireguardOperatorContext {
    pub target: SshTarget,
    pub cluster_id: ClusterName,
    pub private_key: OperatorWireguardPrivateKey,
    pub source_address: Ipv6Addr,
    pub machine_public_key: [u8; 32],
    pub machine_address: Ipv6Addr,
    pub machine_endpoint: SocketAddr,
    pub machine_allowed_subnet: BuiltinWireguardMemberSubnet,
}

impl fmt::Debug for BuiltinWireguardOperatorContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BuiltinWireguardOperatorContext")
            .field("target", &self.target)
            .field("cluster_id", &self.cluster_id)
            .field("private_key", &self.private_key)
            .field("source_address", &self.source_address)
            .field("machine_public_key", &self.machine_public_key)
            .field("machine_address", &self.machine_address)
            .field("machine_endpoint", &self.machine_endpoint)
            .field("machine_allowed_subnet", &self.machine_allowed_subnet)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedOperatorContext {
    pub target: SshTarget,
    pub cluster_id: ClusterName,
    pub provider: UnsupportedMeshProvider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedMeshProvider {
    Tailscale,
}

impl OperatorContext {
    fn normalize(
        self,
        target: &SshTarget,
        peer_key: &SshPeerKey,
    ) -> Result<LoadedOperatorContext, OperatorContextError> {
        self.validate_identity(target, peer_key)?;
        match (self.provider, self.machine_transport) {
            (
                MeshProvider::BuiltinWireguard,
                MachineTransport::Wireguard {
                    pubkey,
                    addr_v6,
                    endpoint,
                    subnet_v4: _,
                },
            ) => {
                let machine_identity = derive_builtin_wireguard_member(&self.cluster_id, &pubkey);
                let expected_machine = machine_identity.bind_address().get();
                if addr_v6 != expected_machine {
                    return Err(OperatorContextError::InvalidMachineAddress {
                        expected: expected_machine,
                        found: addr_v6,
                    });
                }
                if pubkey == self.peer_public_key {
                    return Err(OperatorContextError::MachineUsesPeerKey);
                }
                let source_address =
                    derive_builtin_wireguard_member(&self.cluster_id, &self.peer_public_key)
                        .bind_address()
                        .get();
                if source_address == addr_v6 {
                    return Err(OperatorContextError::MachineUsesPeerAddress);
                }
                let Some(machine_endpoint) = endpoint else {
                    return Err(OperatorContextError::MissingWireguardEndpoint {
                        target: self.target,
                    });
                };
                Ok(LoadedOperatorContext::BuiltinWireguard(
                    BuiltinWireguardOperatorContext {
                        target: self.target,
                        cluster_id: self.cluster_id,
                        private_key: peer_key.private_key()?,
                        source_address,
                        machine_public_key: pubkey.decoded_bytes(),
                        machine_address: addr_v6,
                        machine_endpoint,
                        machine_allowed_subnet: machine_identity.subnet(),
                    },
                ))
            }
            (MeshProvider::Tailscale, MachineTransport::Tailscale { .. }) => Ok(
                LoadedOperatorContext::UnsupportedProvider(UnsupportedOperatorContext {
                    target: self.target,
                    cluster_id: self.cluster_id,
                    provider: UnsupportedMeshProvider::Tailscale,
                }),
            ),
            (MeshProvider::BuiltinWireguard, MachineTransport::Tailscale { .. })
            | (MeshProvider::Tailscale, MachineTransport::Wireguard { .. }) => {
                Err(OperatorContextError::ProviderTransportMismatch)
            }
        }
    }
}

#[must_use]
pub fn context_path(config_home: &Path, target: &SshTarget) -> PathBuf {
    context_directory(config_home).join(format!("{}.json", hex(target.as_str().as_bytes())))
}

#[must_use]
pub fn default_config_home(home: &Path) -> PathBuf {
    home.join(".ployz")
}

fn context_directory(config_home: &Path) -> PathBuf {
    config_home.join("mesh/contexts")
}

fn key_directory(config_home: &Path) -> PathBuf {
    config_home.join("mesh/init-peers")
}

pub(crate) fn key_path(config_home: &Path, target: &SshTarget) -> PathBuf {
    key_directory(config_home).join(format!("{}.json", hex(target.as_str().as_bytes())))
}

fn is_context_temporary(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name.starts_with(".ployz-") && name.ends_with(".tmp")
}

fn read_regular_private_file(path: &Path) -> Result<Vec<u8>, OperatorContextError> {
    let metadata = fs::symlink_metadata(path).map_err(OperatorContextError::Filesystem)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(OperatorContextError::UnsafeContextFile);
    }
    protect_private_file(path)?;
    fs::read(path).map_err(OperatorContextError::Filesystem)
}

fn set_private_directory(path: &Path) -> Result<(), OperatorContextError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(OperatorContextError::Filesystem)?;
    }
    Ok(())
}

fn protect_private_file(path: &Path) -> Result<(), OperatorContextError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(OperatorContextError::Filesystem)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Publication {
    CreateNew,
    Replace,
}

fn secure_publish_json(
    path: &Path,
    value: &impl Serialize,
    publication: Publication,
) -> Result<(), OperatorContextError> {
    let Some(directory) = path.parent() else {
        return Err(OperatorContextError::InvalidStorePath(path.to_path_buf()));
    };
    fs::create_dir_all(directory).map_err(OperatorContextError::Filesystem)?;
    set_private_directory(directory)?;
    if publication == Publication::Replace {
        match fs::symlink_metadata(path) {
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                return Err(OperatorContextError::UnsafeContextFile);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(OperatorContextError::Filesystem(error)),
        }
    }
    let encoded = serde_json::to_vec_pretty(value)
        .map_err(|error| OperatorContextError::InvalidDocument(error.to_string()))?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".ployz-")
        .suffix(".tmp")
        .tempfile_in(directory)
        .map_err(OperatorContextError::Filesystem)?;
    protect_private_file(temporary.path())?;
    let file = temporary.as_file_mut();
    file.write_all(&encoded)
        .and_then(|()| file.sync_all())
        .map_err(OperatorContextError::Filesystem)?;
    let published = match publication {
        Publication::CreateNew => fs::hard_link(temporary.path(), path),
        Publication::Replace => fs::rename(temporary.path(), path),
    };
    if let Err(error) = published {
        return Err(OperatorContextError::Filesystem(error));
    }
    sync_directory(directory)
}

fn decode_key(bytes: &[u8]) -> Result<SshPeerKey, OperatorContextError> {
    let key: SshPeerKey = serde_json::from_slice(bytes)
        .map_err(|error| OperatorContextError::InvalidKey(error.to_string()))?;
    validate_key(&key)?;
    Ok(key)
}

fn validate_key(key: &SshPeerKey) -> Result<(), OperatorContextError> {
    let private = Key::try_from(key.private_key.as_str())
        .map_err(|error| OperatorContextError::InvalidKey(error.to_string()))?;
    if private.public_key().to_string() != key.public_key.as_str() {
        return Err(OperatorContextError::InvalidKey(
            "stored public and private WireGuard keys disagree".to_owned(),
        ));
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), OperatorContextError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(OperatorContextError::Filesystem)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, thiserror::Error)]
pub enum OperatorContextError {
    #[error("operator context filesystem failed: {0}")]
    Filesystem(std::io::Error),
    #[error("operator context for {target} is not configured")]
    MissingContext { target: SshTarget },
    #[error("operator context document is invalid: {0}")]
    InvalidDocument(String),
    #[error("SSH context handoff prefix is missing")]
    MissingHandoffPrefix,
    #[error("operator context provider and machine transport disagree")]
    ProviderTransportMismatch,
    #[error("operator context machine address is {found}, expected {expected}")]
    InvalidMachineAddress { expected: Ipv6Addr, found: Ipv6Addr },
    #[error("operator context machine transport reuses the operator peer key")]
    MachineUsesPeerKey,
    #[error("operator context machine transport reuses the operator peer address")]
    MachineUsesPeerAddress,
    #[error(
        "operator context for {target} has no WireGuard endpoint; run `ployz init {target} --wireguard-endpoint <ip>:51820`"
    )]
    MissingWireguardEndpoint { target: SshTarget },
    #[error("operator context peer identity disagrees with its authoritative key")]
    PeerIdentityMismatch,
    #[error("operator context target is {found}, expected {expected}")]
    TargetMismatch {
        expected: SshTarget,
        found: SshTarget,
    },
    #[error("operator context filename does not match target {target}")]
    TargetFilenameMismatch { target: SshTarget },
    #[error("operator context path is not a regular file")]
    UnsafeContextFile,
    #[error("operator peer key is invalid: {0}")]
    InvalidKey(String),
    #[error("operator context store path has no parent: {0}")]
    InvalidStorePath(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::network::{MachineEndpointSubnet, WireGuardPublicKey};

    const MACHINE_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn handoff() -> SshContextHandoff {
        let cluster_id = ClusterName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("cluster id");
        let machine_key = WireGuardPublicKey::try_new(MACHINE_KEY).expect("machine key");
        SshContextHandoff {
            machine_transport: MachineTransport::Wireguard {
                addr_v6: derive_builtin_wireguard_member(&cluster_id, &machine_key)
                    .bind_address()
                    .get(),
                pubkey: machine_key,
                endpoint: Some("203.0.113.7:51820".parse().expect("endpoint")),
                subnet_v4: MachineEndpointSubnet::try_new("10.210.0.0/24").expect("machine subnet"),
            },
            cluster_id,
            provider: MeshProvider::BuiltinWireguard,
        }
    }

    #[test]
    fn handoff_is_one_prefixed_strict_json_line() {
        let encoded = handoff().encode_handoff().expect("handoff");
        assert!(encoded.starts_with(SSH_CONTEXT_HANDOFF_PREFIX));
        assert_eq!(encoded.lines().count(), 1);
        assert!(!encoded.contains("target"));
        assert!(!encoded.contains("peer_id"));
        assert_eq!(
            SshContextHandoff::decode_handoff(encoded.trim_end()).expect("decoded"),
            handoff()
        );
        let error = SshContextHandoff::decode_handoff(&format!(
            "{SSH_CONTEXT_HANDOFF_PREFIX}{{\"cluster_id\":\"01ARZ3NDEKTSV4RRFFQ69G5FAV\",\"provider\":\"builtin_wireguard\",\"machine_transport\":{},\"future\":true}}",
            serde_json::to_string(&handoff().machine_transport).expect("transport")
        ))
        .expect_err("unknown fields fail");
        assert!(matches!(error, OperatorContextError::InvalidDocument(_)));
    }

    #[test]
    fn persisted_context_is_private_and_retry_converges() {
        let temporary = tempfile::tempdir().expect("temp directory");
        let target: SshTarget = "root@203.0.113.7".parse().expect("target");
        let key = SshPeerKey::generate("laptop".to_owned()).expect("key");
        key.persist_new(temporary.path(), &target)
            .expect("key file");
        let store = OperatorContextStore::new(temporary.path());
        store.persist(&target, handoff(), &key).expect("persist");
        store.persist(&target, handoff(), &key).expect("retry");
        let loaded = store.load_target(&target).expect("load");
        let LoadedOperatorContext::BuiltinWireguard(loaded) = loaded else {
            panic!("builtin context")
        };
        assert_eq!(loaded.target, target);
        assert_eq!(loaded.cluster_id, handoff().cluster_id);
        assert_eq!(
            loaded.private_key.bytes(),
            key.private_key_bytes().expect("key bytes")
        );
        let key_json = serde_json::to_value(&key).expect("key JSON");
        let private_key = key_json
            .get("private_key")
            .and_then(serde_json::Value::as_str)
            .expect("private key field");
        let context_bytes =
            fs::read(context_path(temporary.path(), &target)).expect("context file");
        let context_json: serde_json::Value =
            serde_json::from_slice(&context_bytes).expect("context JSON");
        assert_eq!(
            context_json.get("target").and_then(|value| value.as_str()),
            Some(target.as_str())
        );
        assert!(
            !String::from_utf8(context_bytes)
                .expect("context UTF-8")
                .contains(private_key)
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(temporary.path().join("mesh/init-peers"))
                    .expect("key directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(key_path(temporary.path(), &target))
                    .expect("key file metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(temporary.path().join("mesh/contexts"))
                    .expect("directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(context_path(temporary.path(), &target))
                    .expect("file metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn invalid_persisted_target_is_rejected_during_decode() {
        let temporary = tempfile::tempdir().expect("temp directory");
        let target: SshTarget = "root@203.0.113.7".parse().expect("target");
        let key = SshPeerKey::generate("laptop".to_owned()).expect("key");
        key.persist_new(temporary.path(), &target)
            .expect("key file");
        let store = OperatorContextStore::new(temporary.path());
        store.persist(&target, handoff(), &key).expect("context");
        let path = context_path(temporary.path(), &target);
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("context bytes")).expect("context JSON");
        let Some(target_value) = document.get_mut("target") else {
            panic!("context target")
        };
        *target_value = serde_json::Value::String("203.0.113.7".to_owned());
        fs::write(
            &path,
            serde_json::to_vec(&document).expect("encoded context"),
        )
        .expect("invalid context");
        assert!(matches!(
            store.load_target(&target),
            Err(OperatorContextError::InvalidDocument(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn context_publication_refuses_an_existing_symlink() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temp directory");
        let target: SshTarget = "root@203.0.113.7".parse().expect("target");
        let key = SshPeerKey::generate("laptop".to_owned()).expect("key");
        key.persist_new(temporary.path(), &target)
            .expect("key file");
        let store = OperatorContextStore::new(temporary.path());
        let context_directory = temporary.path().join("mesh/contexts");
        fs::create_dir_all(&context_directory).expect("context directory");
        let outside = temporary.path().join("outside");
        fs::write(&outside, b"untouched").expect("outside file");
        symlink(&outside, context_path(temporary.path(), &target)).expect("context symlink");
        assert!(matches!(
            store.persist(&target, handoff(), &key),
            Err(OperatorContextError::UnsafeContextFile)
        ));
        assert_eq!(fs::read(outside).expect("outside file"), b"untouched");
    }

    #[test]
    fn provider_and_derived_machine_address_are_validated() {
        let target: SshTarget = "root@203.0.113.7".parse().expect("target");
        let key = SshPeerKey::generate("laptop".to_owned()).expect("key");
        let mut wrong_provider = OperatorContext::from_handoff(&target, &key, handoff());
        wrong_provider.provider = MeshProvider::Tailscale;
        assert!(matches!(
            wrong_provider.normalize(&target, &key),
            Err(OperatorContextError::ProviderTransportMismatch)
        ));

        let mut wrong_address = OperatorContext::from_handoff(&target, &key, handoff());
        let MachineTransport::Wireguard { addr_v6, .. } = &mut wrong_address.machine_transport
        else {
            panic!("WireGuard context")
        };
        *addr_v6 = "fd00::1".parse().expect("IPv6");
        assert!(matches!(
            wrong_address.normalize(&target, &key),
            Err(OperatorContextError::InvalidMachineAddress { .. })
        ));
    }

    #[test]
    fn builtin_runtime_context_requires_a_machine_endpoint() {
        let temporary = tempfile::tempdir().expect("temp directory");
        let target: SshTarget = "root@203.0.113.7".parse().expect("target");
        let key = SshPeerKey::generate("laptop".to_owned()).expect("key");
        key.persist_new(temporary.path(), &target)
            .expect("key file");
        let mut handoff = handoff();
        let MachineTransport::Wireguard { endpoint, .. } = &mut handoff.machine_transport else {
            panic!("WireGuard handoff")
        };
        *endpoint = None;
        assert!(matches!(
            OperatorContextStore::new(temporary.path()).persist(&target, handoff, &key),
            Err(OperatorContextError::MissingWireguardEndpoint { .. })
        ));
    }

    #[test]
    fn tailscale_context_normalizes_to_an_explicit_unsupported_provider() {
        let temporary = tempfile::tempdir().expect("temp directory");
        let target: SshTarget = "root@machine.example".parse().expect("target");
        let key = SshPeerKey::generate("laptop".to_owned()).expect("key");
        key.persist_new(temporary.path(), &target)
            .expect("key file");
        let cluster_id = ClusterName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("cluster id");
        let handoff = SshContextHandoff {
            cluster_id: cluster_id.clone(),
            provider: MeshProvider::Tailscale,
            machine_transport: MachineTransport::Tailscale {
                ip: "100.64.0.7".parse().expect("tailnet IP"),
                subnet_v4: MachineEndpointSubnet::try_new("10.210.0.0/24").expect("machine subnet"),
            },
        };
        let store = OperatorContextStore::new(temporary.path());
        store.persist(&target, handoff, &key).expect("context");
        let LoadedOperatorContext::UnsupportedProvider(loaded) =
            store.load_target(&target).expect("loaded context")
        else {
            panic!("unsupported context")
        };
        assert_eq!(loaded.target, target);
        assert_eq!(loaded.cluster_id, cluster_id);
        assert_eq!(loaded.provider, UnsupportedMeshProvider::Tailscale);
    }

    #[test]
    fn replacing_the_same_targets_peer_key_is_rejected() {
        let temporary = tempfile::tempdir().expect("temp directory");
        let target: SshTarget = "root@203.0.113.7".parse().expect("target");
        let original = SshPeerKey::generate("laptop".to_owned()).expect("original key");
        original
            .persist_new(temporary.path(), &target)
            .expect("key file");
        let store = OperatorContextStore::new(temporary.path());
        store
            .persist(&target, handoff(), &original)
            .expect("context");
        fs::remove_file(key_path(temporary.path(), &target)).expect("remove original key");
        let replacement = SshPeerKey::generate("other".to_owned()).expect("replacement key");
        replacement
            .persist_new(temporary.path(), &target)
            .expect("replacement file");
        assert!(matches!(
            store.load_target(&target),
            Err(OperatorContextError::PeerIdentityMismatch)
        ));
    }

    #[test]
    fn persistence_requires_the_matching_authoritative_key_file() {
        let temporary = tempfile::tempdir().expect("temp directory");
        let target: SshTarget = "root@203.0.113.7".parse().expect("target");
        let key = SshPeerKey::generate("laptop".to_owned()).expect("key");
        let store = OperatorContextStore::new(temporary.path());
        assert!(matches!(
            store.persist(&target, handoff(), &key),
            Err(OperatorContextError::Filesystem(error))
                if error.kind() == std::io::ErrorKind::NotFound
        ));
        assert!(!context_path(temporary.path(), &target).exists());
    }

    #[test]
    fn listing_loads_every_key_without_minting_and_sorts_by_target() {
        let temporary = tempfile::tempdir().expect("temp directory");
        let store = OperatorContextStore::new(temporary.path());
        for target in ["root@z.example", "root@a.example"] {
            let target: SshTarget = target.parse().expect("target");
            let key = SshPeerKey::generate("laptop".to_owned()).expect("key");
            key.persist_new(temporary.path(), &target)
                .expect("key file");
            store.persist(&target, handoff(), &key).expect("context");
        }
        let loaded = store.load_all().expect("all contexts");
        assert_eq!(
            loaded
                .iter()
                .map(|context| context.target().as_str())
                .collect::<Vec<_>>(),
            ["root@a.example", "root@z.example"]
        );
        assert_eq!(
            fs::read_dir(temporary.path().join("mesh/init-peers"))
                .expect("key directory")
                .count(),
            2
        );
    }

    #[test]
    fn listing_cleans_only_owned_temporary_residues() {
        let temporary = tempfile::tempdir().expect("temp directory");
        let directory = temporary.path().join("mesh/contexts");
        fs::create_dir_all(&directory).expect("context directory");
        let residue = directory.join(".ployz-residue.tmp");
        fs::write(&residue, b"partial").expect("temporary residue");
        assert!(
            OperatorContextStore::new(temporary.path())
                .load_all()
                .expect("owned residue is cleaned")
                .is_empty()
        );
        assert!(!residue.exists());

        fs::write(directory.join(".not.ours.tmp"), b"partial").expect("unknown file");
        assert!(matches!(
            OperatorContextStore::new(temporary.path()).load_all(),
            Err(OperatorContextError::InvalidDocument(_))
        ));
    }
}
