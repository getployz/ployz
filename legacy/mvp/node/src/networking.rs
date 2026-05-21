use mvp_mesh::{HostNetworkApplyReport, HostNetworkBackend, HostNetworkSnapshot};
use mvp_projection::GatewayProjection;

use crate::{LoadedNodeState, NodeError, NodeResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostNetworkingReport {
    pub applied: HostNetworkApplyReport,
    pub snapshot: HostNetworkSnapshot,
}

pub fn apply_host_networking_snapshot(
    state: &LoadedNodeState,
    revision: u64,
    gateway: &GatewayProjection,
) -> NodeResult<HostNetworkingReport> {
    let snapshot = HostNetworkSnapshot::from_gateway(&state.island(), revision, gateway)
        .map_err(|source| NodeError::Mesh { source })?;
    let backend = HostNetworkBackend::new(state.paths().host_network_snapshot.clone());
    let applied = backend
        .apply(&snapshot)
        .map_err(|source| NodeError::Mesh { source })?;

    Ok(HostNetworkingReport { applied, snapshot })
}

pub fn load_host_networking_snapshot(
    state: &LoadedNodeState,
) -> NodeResult<Option<HostNetworkSnapshot>> {
    HostNetworkBackend::new(state.paths().host_network_snapshot.clone())
        .last_applied(&state.island())
        .map_err(|source| NodeError::Mesh { source })
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use mvp_identity::NodeId;
    use mvp_projection::{BackendEndpoint, GatewayProjection, GatewayRouteProjection, RouteId};
    use tempfile::tempdir;

    use crate::{
        InitOptions, apply_host_networking_snapshot, init_node, load_host_networking_snapshot,
    };

    #[test]
    fn host_networking_snapshot_is_applied_under_node_state_dir() {
        let temp = tempdir().expect("tempdir");
        let state = init_node(
            InitOptions::new(temp.path())
                .with_island("prod")
                .with_node_id("node-1"),
        )
        .expect("init node");
        let server = TcpListener::bind("127.0.0.1:0").expect("bind service");
        let address = server.local_addr().expect("service address");
        thread::spawn(move || {
            let _ = server.accept();
        });
        let gateway = GatewayProjection {
            gateway_commit_id: "gateway-1".to_string(),
            route_commit_id: "route-1".to_string(),
            routes: vec![GatewayRouteProjection {
                route_id: RouteId::new("web"),
                hostnames: vec!["web.example.test".to_string()],
                backends: vec![BackendEndpoint {
                    node_id: NodeId::new("node-2"),
                    address: address.to_string(),
                }],
                old_backends_to_drain: Vec::new(),
            }],
        };

        let report = apply_host_networking_snapshot(&state, 4, &gateway).expect("apply networking");

        assert_eq!(report.applied.backend_count, 1);
        assert_eq!(
            load_host_networking_snapshot(&state).expect("load networking"),
            Some(report.snapshot)
        );
    }
}
