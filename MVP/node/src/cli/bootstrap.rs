use std::fs;
use std::path::{Path, PathBuf};

use mvp_bus::IslandId;
use mvp_node::{InitOptions, NodeError, NodeResult, init_node, load_node};
use mvp_projection::{
    DnsProjection, GatewayProjection, ProjectionState, write_projection_snapshots,
    write_serving_generation_manifest,
};
use serde::Serialize;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ParsedArgs {
    pub(crate) state_dir: Option<PathBuf>,
    pub(crate) island: Option<String>,
    pub(crate) node_id: Option<String>,
}

pub(crate) fn init(args: &[String]) -> NodeResult<String> {
    let parsed = ParsedArgs::parse(args)?;
    let Some(state_dir) = parsed.state_dir else {
        return Err(NodeError::MissingFlagValue { flag: "--state" });
    };
    let mut options = InitOptions::new(state_dir);
    if let Some(island) = parsed.island {
        options = options.with_island(island);
    }
    if let Some(node_id) = parsed.node_id {
        options = options.with_node_id(node_id);
    }
    let state = init_node(options)?;
    Ok(format!(
        "initialized node={} island={} state={}",
        state.node_id_str(),
        state.island_id(),
        state.paths().state_dir.display()
    ))
}

pub(crate) fn bootstrap(args: &[String]) -> NodeResult<String> {
    let parsed = ParsedArgs::parse(args)?;
    let Some(state_dir) = parsed.state_dir else {
        return Err(NodeError::MissingFlagValue { flag: "--state" });
    };
    let state = match load_node(&state_dir) {
        Ok(state) => {
            if let Some(island) = parsed.island.as_deref()
                && state.island_id() != island
            {
                return Err(NodeError::BootstrapConflict {
                    field: "island",
                    requested: island.to_string(),
                    existing: state.island_id().to_string(),
                });
            }
            if let Some(node_id) = parsed.node_id.as_deref()
                && state.node_id_str() != node_id
            {
                return Err(NodeError::BootstrapConflict {
                    field: "node_id",
                    requested: node_id.to_string(),
                    existing: state.node_id_str().to_string(),
                });
            }
            state
        }
        Err(NodeError::NotInitialized { .. }) => {
            let mut options = InitOptions::new(&state_dir);
            if let Some(island) = parsed.island {
                options = options.with_island(island);
            }
            if let Some(node_id) = parsed.node_id {
                options = options.with_node_id(node_id);
            }
            init_node(options)?
        }
        Err(error) => return Err(error),
    };
    ensure_bootstrap_dirs(state.paths())?;
    ensure_bootstrap_snapshots(&state)?;
    let response = BootstrapResponse::from_state(&state);
    serde_json::to_string(&response).map_err(|source| NodeError::EncodeNodeAgentRpc { source })
}

pub(crate) fn status(args: &[String]) -> NodeResult<String> {
    let state_dir = parse_state_dir_only(args)?;
    let state = load_node(state_dir)?;
    Ok(format!(
        "node={} island={} principal={} facts={} projection={} gateway_snapshot={} dns_snapshot={}",
        state.node_id_str(),
        state.island_id(),
        state.principal_id(),
        state.paths().fact_store.display(),
        state.paths().projection_db.display(),
        state.paths().gateway_snapshot.display(),
        state.paths().dns_snapshot.display()
    ))
}

pub(crate) fn parse_state_dir_only(args: &[String]) -> NodeResult<PathBuf> {
    let mut state_dir = None;
    let mut remaining = args.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--state" => {
                let Some(value) = remaining.next() else {
                    return Err(NodeError::MissingFlagValue { flag: "--state" });
                };
                state_dir = Some(PathBuf::from(value));
            }
            other => {
                return Err(NodeError::UnknownArgument {
                    argument: other.to_string(),
                });
            }
        }
    }
    state_dir.ok_or(NodeError::MissingFlagValue { flag: "--state" })
}

fn ensure_bootstrap_dirs(paths: &mvp_node::NodePaths) -> NodeResult<()> {
    for path in [
        paths.runtime_dir.clone(),
        paths.wireguard_dir.clone(),
        paths.state_dir.join("control"),
        paths.state_dir.join("acme"),
    ] {
        fs::create_dir_all(&path).map_err(|source| NodeError::CreateStateDir { path, source })?;
    }
    Ok(())
}

fn ensure_bootstrap_snapshots(state: &mvp_node::LoadedNodeState) -> NodeResult<()> {
    if state.paths().gateway_snapshot.exists() && state.paths().dns_snapshot.exists() {
        write_serving_generation_manifest(
            &state.paths().gateway_snapshot,
            &state.paths().dns_snapshot,
        )
        .map_err(|source| NodeError::Projection { source })?;
        return Ok(());
    }
    let mut projection = ProjectionState::for_island(IslandId::new(state.island_id().to_string()));
    projection.gateway = Some(GatewayProjection {
        gateway_commit_id: "bootstrap-gateway-empty".to_string(),
        route_commit_id: "bootstrap-route-empty".to_string(),
        routes: Vec::new(),
    });
    projection.dns = Some(DnsProjection {
        dns_commit_id: "bootstrap-dns-empty".to_string(),
        records: Vec::new(),
    });
    write_projection_snapshots(
        &projection,
        &state.paths().gateway_snapshot,
        &state.paths().dns_snapshot,
    )
    .map_err(|source| NodeError::Projection { source })?;
    Ok(())
}

#[derive(Serialize)]
struct BootstrapResponse {
    status: &'static str,
    node_id: String,
    island: String,
    principal: String,
    paths: BootstrapPaths,
    identity: BootstrapIdentity,
    role_defaults: BootstrapRoleDefaults,
}

#[derive(Serialize)]
struct BootstrapPaths {
    state_dir: PathBuf,
    fact_store: PathBuf,
    projection_db: PathBuf,
    gateway_snapshot: PathBuf,
    dns_snapshot: PathBuf,
    runtime_dir: PathBuf,
    wireguard_dir: PathBuf,
    wireguard_private_key: PathBuf,
    acme_accounts: PathBuf,
}

#[derive(Serialize)]
struct BootstrapIdentity {
    p2panda_network_id_hex: String,
    p2panda_topic_hex: String,
    wireguard_public_key: String,
    wireguard_overlay_ip: String,
    container_subnet: String,
}

#[derive(Serialize)]
struct BootstrapRoleDefaults {
    daemon_control_socket: PathBuf,
    gateway_control_socket: PathBuf,
    dns_control_socket: PathBuf,
}

impl BootstrapResponse {
    fn from_state(state: &mvp_node::LoadedNodeState) -> Self {
        let paths = state.paths();
        Self {
            status: "bootstrapped",
            node_id: state.node_id_str().to_string(),
            island: state.island_id().to_string(),
            principal: state.principal_id().to_string(),
            paths: BootstrapPaths {
                state_dir: paths.state_dir.clone(),
                fact_store: paths.fact_store.clone(),
                projection_db: paths.projection_db.clone(),
                gateway_snapshot: paths.gateway_snapshot.clone(),
                dns_snapshot: paths.dns_snapshot.clone(),
                runtime_dir: paths.runtime_dir.clone(),
                wireguard_dir: paths.wireguard_dir.clone(),
                wireguard_private_key: paths.wireguard_private_key.clone(),
                acme_accounts: paths.state_dir.join("acme-accounts.json"),
            },
            identity: BootstrapIdentity {
                p2panda_network_id_hex: state.p2panda_network_id_hex().to_string(),
                p2panda_topic_hex: state.p2panda_topic_hex().to_string(),
                wireguard_public_key: state.wireguard_public_key().to_string(),
                wireguard_overlay_ip: state.wireguard_overlay_ip().to_string(),
                container_subnet: state.container_subnet().to_string(),
            },
            role_defaults: BootstrapRoleDefaults {
                daemon_control_socket: role_socket(paths.state_dir.as_path(), "daemon"),
                gateway_control_socket: role_socket(paths.state_dir.as_path(), "gateway"),
                dns_control_socket: role_socket(paths.state_dir.as_path(), "dns"),
            },
        }
    }
}

fn role_socket(state_dir: &Path, role: &str) -> PathBuf {
    state_dir.join("control").join(format!("{role}.sock"))
}

impl ParsedArgs {
    pub(crate) fn parse(args: &[String]) -> NodeResult<Self> {
        let mut parsed = Self {
            state_dir: None,
            island: None,
            node_id: None,
        };
        let mut remaining = args.iter();
        while let Some(argument) = remaining.next() {
            match argument.as_str() {
                "--state" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--state" });
                    };
                    parsed.state_dir = Some(PathBuf::from(value));
                }
                "--island" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--island" });
                    };
                    parsed.island = Some(value.clone());
                }
                "--node-id" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--node-id" });
                    };
                    parsed.node_id = Some(value.clone());
                }
                other => {
                    return Err(NodeError::UnknownArgument {
                        argument: other.to_string(),
                    });
                }
            }
        }
        Ok(parsed)
    }
}
