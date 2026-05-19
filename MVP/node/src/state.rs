use std::fs;
use std::io::{ErrorKind, Write};
use std::path::Path;

use mvp_bus::{IslandId, PrincipalId};
use mvp_identity::NodeId;
use mvp_mesh::{WireGuardPublicKey, derive_overlay_ip};
use mvp_p2panda_facts::PandaFactAuthor;
use mvp_p2panda_transport::{PandaNetNetworkId, PandaNetNodeSeed, PandaNetTopic};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::config::{InitOptions, NodePaths};
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
    pub p2panda_topic_hex: String,
    pub wg_public_key: String,
    pub wg_overlay_ip: String,
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

    pub fn p2panda_topic(&self) -> NodeResult<PandaNetTopic> {
        PandaNetTopic::parse_hex(&self.persisted.p2panda_topic_hex)
            .map_err(|source| NodeError::InvalidTopic { source })
    }
}

pub fn init_node(options: InitOptions) -> NodeResult<LoadedNodeState> {
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
        p2panda_network_id_hex: stable_hex("network-id", &network_seed),
        p2panda_node_seed_hex: stable_hex("node-seed", &node_seed),
        p2panda_topic_hex: stable_hex("topic", &topic_seed),
        wg_public_key: WireGuardPublicKey::new(wg_public_key).to_string(),
        wg_overlay_ip: overlay_ip.to_string(),
    };
    write_state(&paths, &state)?;
    let loaded = LoadedNodeState {
        persisted: state,
        paths,
    };
    loaded.author()?;
    loaded.p2panda_network_id()?;
    loaded.p2panda_node_seed()?;
    loaded.p2panda_topic()?;
    Ok(loaded)
}

pub fn load_node(state_dir: impl AsRef<Path>) -> NodeResult<LoadedNodeState> {
    let paths = NodePaths::for_state_dir(state_dir.as_ref());
    if !paths.state_file.exists() {
        return Err(NodeError::NotInitialized {
            path: paths.state_dir,
        });
    }
    let contents =
        fs::read_to_string(&paths.state_file).map_err(|source| NodeError::ReadState {
            path: paths.state_file.clone(),
            source,
        })?;
    let persisted = serde_json::from_str::<PersistedNodeState>(&contents).map_err(|source| {
        NodeError::DecodeState {
            path: paths.state_file.clone(),
            source,
        }
    })?;
    validate_schema_version(persisted.schema_version)?;
    let loaded = LoadedNodeState { persisted, paths };
    loaded.author()?;
    loaded.p2panda_network_id()?;
    loaded.p2panda_node_seed()?;
    loaded.p2panda_topic()?;
    Ok(loaded)
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

fn random_seed_hex(label: &'static str) -> String {
    let generated = PandaFactAuthor::new(PrincipalId::new(format!("seed:{label}")));
    generated.private_key_hex()
}

fn stable_hex(label: &'static str, seed: &str) -> String {
    let hash = blake3::hash(format!("{label}:{seed}").as_bytes());
    hash.to_hex().to_string()
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
