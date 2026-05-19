use std::fs;
use std::io::{ErrorKind, Write};
use std::path::Path;

use mvp_bus::{IslandId, PrincipalId};
use mvp_identity::NodeId;
use mvp_mesh::{WireGuardPublicKey, derive_overlay_ip};
use mvp_p2panda_facts::{PandaFactAuthor, PandaFactAuthorKey};
use mvp_p2panda_transport::{PandaNetNetworkId, PandaNetNodeSeed, PandaNetTopic};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::config::{BootstrapPeerConfig, InitOptions, JoinedInitOptions, NodePaths};
use crate::error::{NodeError, NodeResult};

const STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PersistedNodeState {
    pub schema_version: u32,
    pub island_id: String,
    pub node_id: String,
    pub principal_id: String,
    pub p2panda_author_private_key_hex: String,
    pub p2panda_network_id_hex: String,
    pub p2panda_node_seed_hex: String,
    #[serde(default)]
    pub p2panda_port: Option<u16>,
    pub p2panda_topic_hex: String,
    pub wg_public_key: String,
    pub wg_overlay_ip: String,
    #[serde(default)]
    pub bootstrap_peers: Vec<BootstrapPeerConfig>,
    #[serde(default)]
    pub join_invite: Option<StoredJoinInvite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredJoinInvite {
    pub invite_id: String,
    pub invite_secret: String,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IssuedInviteRecord {
    pub invite_id: String,
    pub invite_secret: String,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct LoadedNodeState {
    persisted: PersistedNodeState,
    paths: NodePaths,
}

impl LoadedNodeState {
    #[must_use]
    pub fn island_id(&self) -> &str {
        &self.persisted.island_id
    }

    #[must_use]
    pub fn node_id_str(&self) -> &str {
        &self.persisted.node_id
    }

    #[must_use]
    pub fn principal_id(&self) -> &str {
        &self.persisted.principal_id
    }

    #[must_use]
    pub fn wireguard_overlay_ip(&self) -> &str {
        &self.persisted.wg_overlay_ip
    }

    #[must_use]
    pub fn wireguard_public_key(&self) -> &str {
        &self.persisted.wg_public_key
    }

    #[must_use]
    pub fn paths(&self) -> &NodePaths {
        &self.paths
    }

    #[must_use]
    pub fn island(&self) -> IslandId {
        IslandId::new(self.persisted.island_id.clone())
    }

    #[must_use]
    pub fn node_id(&self) -> NodeId {
        NodeId::new(self.persisted.node_id.clone())
    }

    #[must_use]
    pub fn principal(&self) -> PrincipalId {
        PrincipalId::new(self.persisted.principal_id.clone())
    }

    pub fn author(&self) -> NodeResult<PandaFactAuthor> {
        PandaFactAuthor::from_private_key_hex(
            self.principal(),
            &self.persisted.p2panda_author_private_key_hex,
        )
        .map_err(|source| NodeError::InvalidAuthorKey { source })
    }

    pub fn p2panda_network_id(&self) -> NodeResult<PandaNetNetworkId> {
        PandaNetNetworkId::parse_hex(&self.persisted.p2panda_network_id_hex)
            .map_err(|source| NodeError::InvalidNetworkId { source })
    }

    pub fn p2panda_node_seed(&self) -> NodeResult<PandaNetNodeSeed> {
        PandaNetNodeSeed::parse_hex(&self.persisted.p2panda_node_seed_hex)
            .map_err(|source| NodeError::InvalidNodeSeed { source })
    }

    #[must_use]
    pub fn p2panda_port(&self) -> u16 {
        self.persisted
            .p2panda_port
            .unwrap_or_else(|| stable_port(self.node_id_str()))
    }

    pub fn p2panda_topic(&self) -> NodeResult<PandaNetTopic> {
        PandaNetTopic::parse_hex(&self.persisted.p2panda_topic_hex)
            .map_err(|source| NodeError::InvalidTopic { source })
    }

    #[must_use]
    pub fn p2panda_network_id_hex(&self) -> &str {
        &self.persisted.p2panda_network_id_hex
    }

    #[must_use]
    pub fn p2panda_topic_hex(&self) -> &str {
        &self.persisted.p2panda_topic_hex
    }

    pub fn bootstrap_tickets(&self) -> NodeResult<Vec<mvp_p2panda_transport::PandaNetNodeTicket>> {
        self.persisted
            .bootstrap_peers
            .iter()
            .map(|peer| {
                mvp_p2panda_transport::PandaNetNodeTicket::parse(&peer.p2panda_ticket)
                    .map_err(|source| NodeError::InvalidBootstrapTicket { source })
            })
            .collect()
    }

    pub fn trusted_fact_authors(&self) -> NodeResult<Vec<TrustedFactAuthor>> {
        self.persisted
            .bootstrap_peers
            .iter()
            .map(|peer| {
                Ok(TrustedFactAuthor {
                    principal: PrincipalId::new(peer.principal_id.clone()),
                    author_key: PandaFactAuthorKey::parse_hex(&peer.author_key_hex)
                        .map_err(|source| NodeError::InvalidAuthorKey { source })?,
                })
            })
            .collect()
    }

    #[must_use]
    pub fn admitted_peers(&self) -> Vec<BootstrapPeerConfig> {
        self.persisted
            .bootstrap_peers
            .iter()
            .filter(|peer| peer.node_id.is_some())
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn join_invite(&self) -> Option<&StoredJoinInvite> {
        self.persisted.join_invite.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedFactAuthor {
    principal: PrincipalId,
    author_key: PandaFactAuthorKey,
}

impl TrustedFactAuthor {
    #[must_use]
    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    #[must_use]
    pub fn author_key(&self) -> PandaFactAuthorKey {
        self.author_key
    }
}

pub fn init_node(options: InitOptions) -> NodeResult<LoadedNodeState> {
    let state_options = NewNodeStateOptions {
        state_dir: options.state_dir,
        island: options.island,
        node_id: options.node_id,
        p2panda_network_id_hex: None,
        p2panda_topic_hex: None,
        bootstrap_peers: Vec::new(),
        join_invite: None,
    };
    create_node_state(state_options)
}

pub fn init_joined_node(options: JoinedInitOptions) -> NodeResult<LoadedNodeState> {
    let state_options = NewNodeStateOptions {
        state_dir: options.state_dir,
        island: options.island,
        node_id: Some(options.node_id),
        p2panda_network_id_hex: Some(options.p2panda_network_id_hex),
        p2panda_topic_hex: Some(options.p2panda_topic_hex),
        bootstrap_peers: vec![options.bootstrap_peer],
        join_invite: Some(StoredJoinInvite {
            invite_id: options.invite_id,
            invite_secret: options.invite_secret,
            expires_at_ms: options.invite_expires_at_ms,
        }),
    };
    create_node_state(state_options)
}

struct NewNodeStateOptions {
    state_dir: std::path::PathBuf,
    island: String,
    node_id: Option<String>,
    p2panda_network_id_hex: Option<String>,
    p2panda_topic_hex: Option<String>,
    bootstrap_peers: Vec<BootstrapPeerConfig>,
    join_invite: Option<StoredJoinInvite>,
}

fn create_node_state(options: NewNodeStateOptions) -> NodeResult<LoadedNodeState> {
    let paths = NodePaths::for_state_dir(options.state_dir);
    if paths.state_file.exists() {
        return Err(NodeError::AlreadyInitialized {
            path: paths.state_dir.clone(),
        });
    }
    fs::create_dir_all(&paths.state_dir).map_err(|source| NodeError::CreateStateDir {
        path: paths.state_dir.clone(),
        source,
    })?;

    let island = IslandId::new(options.island);
    let author_seed = random_seed_hex("author");
    let network_seed = random_seed_hex("network");
    let node_seed = random_seed_hex("node");
    let topic_seed = random_seed_hex("topic");
    let node_id = options
        .node_id
        .unwrap_or_else(|| format!("node-{}", &node_seed[..12]));
    let principal_id = format!("node:{node_id}");
    let node_id_typed = NodeId::new(node_id.clone());
    let overlay_ip = derive_overlay_ip(&island, &node_id_typed);
    let wg_public_key = format!("mvp-wg-{}", &stable_hex("wg", &node_seed)[..32]);

    let state = PersistedNodeState {
        schema_version: STATE_SCHEMA_VERSION,
        island_id: island.to_string(),
        node_id,
        principal_id: principal_id.clone(),
        p2panda_author_private_key_hex: author_seed,
        p2panda_network_id_hex: options
            .p2panda_network_id_hex
            .unwrap_or_else(|| stable_hex("network-id", &network_seed)),
        p2panda_node_seed_hex: stable_hex("node-seed", &node_seed),
        p2panda_port: Some(stable_port(&node_seed)),
        p2panda_topic_hex: options
            .p2panda_topic_hex
            .unwrap_or_else(|| stable_hex("topic", &topic_seed)),
        wg_public_key: WireGuardPublicKey::new(wg_public_key).to_string(),
        wg_overlay_ip: overlay_ip.to_string(),
        bootstrap_peers: options.bootstrap_peers,
        join_invite: options.join_invite,
    };
    let loaded = LoadedNodeState {
        persisted: state,
        paths,
    };
    validate_loaded_state(&loaded)?;
    write_state(&loaded.paths, &loaded.persisted)?;
    Ok(loaded)
}

pub fn load_node(state_dir: impl AsRef<Path>) -> NodeResult<LoadedNodeState> {
    let paths = NodePaths::for_state_dir(state_dir.as_ref());
    let persisted = read_persisted_state(&paths)?;
    validate_schema_version(persisted.schema_version)?;
    let loaded = LoadedNodeState { persisted, paths };
    validate_loaded_state(&loaded)?;
    Ok(loaded)
}

pub fn node_ticket_path(state: &LoadedNodeState) -> std::path::PathBuf {
    state.paths().state_dir.join("node.ticket")
}

pub fn load_node_ticket(state: &LoadedNodeState) -> NodeResult<String> {
    let path = node_ticket_path(state);
    let ticket = fs::read_to_string(&path).map_err(|source| NodeError::ReadState {
        path: path.clone(),
        source,
    })?;
    Ok(ticket.trim().to_string())
}

pub fn write_node_ticket(state: &LoadedNodeState, ticket: &str) -> NodeResult<()> {
    let path = node_ticket_path(state);
    fs::write(&path, ticket).map_err(|source| NodeError::WriteState { path, source })
}

pub(crate) fn record_bootstrap_peer(
    state_dir: impl AsRef<Path>,
    peer: BootstrapPeerConfig,
) -> NodeResult<LoadedNodeState> {
    let paths = NodePaths::for_state_dir(state_dir.as_ref());
    let mut persisted = read_persisted_state(&paths)?;
    validate_schema_version(persisted.schema_version)?;

    mvp_p2panda_transport::PandaNetNodeTicket::parse(&peer.p2panda_ticket)
        .map_err(|source| NodeError::InvalidBootstrapTicket { source })?;
    PandaFactAuthorKey::parse_hex(&peer.author_key_hex)
        .map_err(|source| NodeError::InvalidAuthorKey { source })?;

    if let Some(index) = persisted.bootstrap_peers.iter().position(|stored| {
        stored.principal_id == peer.principal_id || stored.node_id == peer.node_id
    }) {
        if persisted.bootstrap_peers[index] != peer {
            return Err(NodeError::AdmissionPeerConflict {
                node_id: peer.node_id.unwrap_or_else(|| "<unknown>".to_string()),
                principal_id: peer.principal_id,
            });
        }
        persisted.bootstrap_peers[index] = peer;
    } else {
        persisted.bootstrap_peers.push(peer);
    }

    replace_state(&paths, &persisted)?;
    let loaded = LoadedNodeState { persisted, paths };
    validate_loaded_state(&loaded)?;
    Ok(loaded)
}

pub(crate) fn record_issued_invite(
    state: &LoadedNodeState,
    issued: &IssuedInviteRecord,
) -> NodeResult<()> {
    let path = issued_invite_path(state, &issued.invite_id);
    let parent = path.parent().expect("issued invite path has parent");
    fs::create_dir_all(parent).map_err(|source| NodeError::CreateStateDir {
        path: parent.to_path_buf(),
        source,
    })?;
    let bytes =
        serde_json::to_vec_pretty(issued).map_err(|source| NodeError::EncodeState { source })?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|source| NodeError::WriteState {
        path: parent.to_path_buf(),
        source,
    })?;
    temporary
        .write_all(&bytes)
        .map_err(|source| NodeError::WriteState {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| NodeError::WriteState {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    fs::rename(temporary.path(), &path).map_err(|source| NodeError::WriteState {
        path: path.clone(),
        source,
    })?;
    sync_state_dir(parent)
}

pub(crate) fn load_issued_invite(
    state: &LoadedNodeState,
    invite_id: &str,
) -> NodeResult<IssuedInviteRecord> {
    let path = issued_invite_path(state, invite_id);
    let contents = fs::read_to_string(&path).map_err(|source| {
        if source.kind() == ErrorKind::NotFound {
            return NodeError::InviteNotFound {
                invite_id: invite_id.to_string(),
            };
        }
        NodeError::ReadState {
            path: path.clone(),
            source,
        }
    })?;
    serde_json::from_str(&contents).map_err(|source| NodeError::DecodeState { path, source })
}

fn issued_invite_path(state: &LoadedNodeState, invite_id: &str) -> std::path::PathBuf {
    let digest = blake3::hash(invite_id.as_bytes()).to_hex().to_string();
    state
        .paths()
        .state_dir
        .join("invites")
        .join(format!("{digest}.json"))
}

fn read_persisted_state(paths: &NodePaths) -> NodeResult<PersistedNodeState> {
    if !paths.state_file.exists() {
        return Err(NodeError::NotInitialized {
            path: paths.state_dir.clone(),
        });
    }
    let contents =
        fs::read_to_string(&paths.state_file).map_err(|source| NodeError::ReadState {
            path: paths.state_file.clone(),
            source,
        })?;
    serde_json::from_str::<PersistedNodeState>(&contents).map_err(|source| NodeError::DecodeState {
        path: paths.state_file.clone(),
        source,
    })
}

fn write_state(paths: &NodePaths, state: &PersistedNodeState) -> NodeResult<()> {
    let bytes =
        serde_json::to_vec_pretty(state).map_err(|source| NodeError::EncodeState { source })?;
    let mut temporary =
        NamedTempFile::new_in(&paths.state_dir).map_err(|source| NodeError::WriteState {
            path: paths.state_dir.clone(),
            source,
        })?;
    temporary
        .write_all(&bytes)
        .map_err(|source| NodeError::WriteState {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| NodeError::WriteState {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    temporary
        .persist_noclobber(&paths.state_file)
        .map_err(|error| persist_error(paths, error.error))?;
    sync_state_dir(&paths.state_dir)
}

fn replace_state(paths: &NodePaths, state: &PersistedNodeState) -> NodeResult<()> {
    let bytes =
        serde_json::to_vec_pretty(state).map_err(|source| NodeError::EncodeState { source })?;
    let mut temporary =
        NamedTempFile::new_in(&paths.state_dir).map_err(|source| NodeError::WriteState {
            path: paths.state_dir.clone(),
            source,
        })?;
    temporary
        .write_all(&bytes)
        .map_err(|source| NodeError::WriteState {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| NodeError::WriteState {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    fs::rename(temporary.path(), &paths.state_file).map_err(|source| NodeError::WriteState {
        path: paths.state_file.clone(),
        source,
    })?;
    sync_state_dir(&paths.state_dir)
}

fn persist_error(paths: &NodePaths, source: std::io::Error) -> NodeError {
    if source.kind() == ErrorKind::AlreadyExists {
        return NodeError::AlreadyInitialized {
            path: paths.state_dir.clone(),
        };
    }
    NodeError::PersistState {
        path: paths.state_file.clone(),
        source,
    }
}

fn sync_state_dir(path: &Path) -> NodeResult<()> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| NodeError::SyncStateDir {
            path: path.to_path_buf(),
            source,
        })
}

fn validate_schema_version(found: u32) -> NodeResult<()> {
    if found == STATE_SCHEMA_VERSION {
        return Ok(());
    }
    Err(NodeError::UnsupportedSchemaVersion {
        found,
        expected: STATE_SCHEMA_VERSION,
    })
}

fn validate_loaded_state(state: &LoadedNodeState) -> NodeResult<()> {
    state.author()?;
    state.p2panda_network_id()?;
    state.p2panda_node_seed()?;
    state.p2panda_topic()?;
    state.bootstrap_tickets()?;
    state.trusted_fact_authors()?;
    Ok(())
}

fn random_seed_hex(label: &'static str) -> String {
    let generated = PandaFactAuthor::new(PrincipalId::new(format!("seed:{label}")));
    generated.private_key_hex()
}

fn stable_hex(label: &'static str, seed: &str) -> String {
    let hash = blake3::hash(format!("{label}:{seed}").as_bytes());
    hash.to_hex().to_string()
}

fn stable_port(value: &str) -> u16 {
    let hash = blake3::hash(format!("p2panda-port:{value}").as_bytes());
    let bytes = hash.as_bytes();
    let offset = u16::from_be_bytes([bytes[0], bytes[1]]) % 20_000;
    30_000 + offset
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::{InitOptions, NodeError, init_node, load_node};

    #[test]
    fn init_persists_reopenable_node_identity_and_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join("node-a");

        let initialized = init_node(
            InitOptions::new(&state_dir)
                .with_island("prod")
                .with_node_id("node-a"),
        )
        .expect("init node");
        let reopened = load_node(&state_dir).expect("reopen node");

        assert_eq!(initialized.island_id(), reopened.island_id());
        assert_eq!(initialized.node_id_str(), reopened.node_id_str());
        assert_eq!(initialized.principal_id(), reopened.principal_id());
        assert_eq!(reopened.island_id(), "prod");
        assert_eq!(reopened.node_id_str(), "node-a");
        assert_eq!(reopened.principal_id(), "node:node-a");
        assert!(reopened.wireguard_overlay_ip().starts_with("fd"));
        assert_eq!(reopened.paths().state_dir, state_dir);
        assert!(reopened.author().is_ok());
        assert!(reopened.p2panda_network_id().is_ok());
        assert!(reopened.p2panda_node_seed().is_ok());
        assert!(reopened.p2panda_topic().is_ok());
    }

    #[test]
    fn init_refuses_to_overwrite_existing_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join("node-a");
        init_node(InitOptions::new(&state_dir)).expect("first init");

        let error = init_node(InitOptions::new(&state_dir)).expect_err("second init fails");

        assert!(matches!(error, NodeError::AlreadyInitialized { path } if path == state_dir));
    }

    #[test]
    fn load_missing_state_reports_not_initialized() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join("missing");

        let error = load_node(&state_dir).expect_err("missing state fails");

        assert!(matches!(error, NodeError::NotInitialized { path } if path == state_dir));
    }

    #[test]
    fn corrupt_state_reports_decode_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join("node-a");
        fs::create_dir_all(&state_dir).expect("create state dir");
        fs::write(state_dir.join("node-state.json"), b"{not-json").expect("write corrupt state");

        let error = load_node(&state_dir).expect_err("corrupt state fails");

        assert!(matches!(error, NodeError::DecodeState { .. }));
    }

    #[test]
    fn unsupported_schema_version_fails_at_load_boundary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join("node-a");
        init_node(InitOptions::new(&state_dir)).expect("init node");
        let state_file = state_dir.join("node-state.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&state_file).expect("read state"))
                .expect("state json");
        value["schema_version"] = serde_json::json!(999);
        fs::write(
            &state_file,
            serde_json::to_vec_pretty(&value).expect("encode state"),
        )
        .expect("write unsupported schema");

        let error = load_node(&state_dir).expect_err("unsupported schema fails");

        assert!(matches!(
            error,
            NodeError::UnsupportedSchemaVersion {
                found: 999,
                expected: 1
            }
        ));
    }

    #[test]
    fn copied_state_rehydrates_paths_from_requested_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("original");
        let copied = temp.path().join("copied");
        init_node(InitOptions::new(&original)).expect("init node");
        fs::create_dir_all(&copied).expect("create copied dir");
        fs::copy(
            original.join("node-state.json"),
            copied.join("node-state.json"),
        )
        .expect("copy state");

        let loaded = load_node(&copied).expect("load copied state");

        assert_eq!(loaded.paths().state_dir, copied);
        assert_eq!(
            loaded.paths().fact_store,
            loaded.paths().state_dir.join("facts.sqlite")
        );
    }

    #[test]
    fn concurrent_init_does_not_clobber_existing_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join("node-a");
        let first = state_dir.clone();
        let second = state_dir.clone();
        let first = std::thread::spawn(move || init_node(InitOptions::new(first)));
        let second = std::thread::spawn(move || init_node(InitOptions::new(second)));

        let results = [
            first.join().expect("first thread"),
            second.join().expect("second thread"),
        ];
        let successes = results.iter().filter(|result| result.is_ok()).count();
        let already_initialized = results
            .iter()
            .filter(|result| matches!(result, Err(NodeError::AlreadyInitialized { .. })))
            .count();

        assert_eq!(successes, 1);
        assert_eq!(already_initialized, 1);
        assert!(load_node(&state_dir).is_ok());
    }
}
