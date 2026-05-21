use std::fs;
use std::io::ErrorKind;
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use mvp_bus::IslandId;
use mvp_identity::NodeId;
use mvp_projection::{BackendEndpoint, GatewayProjection, RouteId};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::{MeshError, MeshResult};

const DEFAULT_REACHABILITY_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HostServiceAddress(SocketAddr);

impl HostServiceAddress {
    #[must_use]
    pub fn new(address: SocketAddr) -> Self {
        Self(address)
    }

    #[must_use]
    pub fn socket_addr(self) -> SocketAddr {
        self.0
    }
}

impl std::fmt::Display for HostServiceAddress {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}

impl TryFrom<&BackendEndpoint> for HostServiceAddress {
    type Error = MeshError;

    fn try_from(endpoint: &BackendEndpoint) -> Result<Self, Self::Error> {
        let address = endpoint.address.parse::<SocketAddr>().map_err(|source| {
            MeshError::InvalidHostEndpoint {
                address: endpoint.address.clone(),
                message: source.to_string(),
            }
        })?;
        Ok(Self::new(address))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HostNetworkEndpoint {
    pub node_id: NodeId,
    pub address: HostServiceAddress,
}

impl TryFrom<&BackendEndpoint> for HostNetworkEndpoint {
    type Error = MeshError;

    fn try_from(endpoint: &BackendEndpoint) -> Result<Self, Self::Error> {
        Ok(Self {
            node_id: endpoint.node_id.clone(),
            address: HostServiceAddress::try_from(endpoint)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostNetworkRoute {
    pub route_id: RouteId,
    pub backends: Vec<HostNetworkEndpoint>,
    pub draining_backends: Vec<HostNetworkEndpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostNetworkSnapshot {
    pub island: String,
    pub revision: u64,
    pub routes: Vec<HostNetworkRoute>,
}

impl HostNetworkSnapshot {
    pub fn from_gateway(
        island: &IslandId,
        revision: u64,
        gateway: &GatewayProjection,
    ) -> MeshResult<Self> {
        let mut routes = Vec::new();
        for route in &gateway.routes {
            let mut backends = Vec::new();
            for endpoint in &route.backends {
                backends.push(HostNetworkEndpoint::try_from(endpoint)?);
            }
            backends.sort();

            let mut draining_backends = Vec::new();
            for endpoint in &route.old_backends_to_drain {
                draining_backends.push(HostNetworkEndpoint::try_from(endpoint)?);
            }
            draining_backends.sort();

            routes.push(HostNetworkRoute {
                route_id: route.route_id.clone(),
                backends,
                draining_backends,
            });
        }
        routes.sort_by(|left, right| left.route_id.cmp(&right.route_id));

        Ok(Self {
            island: island.as_str().to_string(),
            revision,
            routes,
        })
    }

    #[must_use]
    pub fn backend_count(&self) -> usize {
        self.routes.iter().map(|route| route.backends.len()).sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostNetworkApplyReport {
    pub revision: u64,
    pub route_count: usize,
    pub backend_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostNetworkBackend {
    applied_path: PathBuf,
    reachability_timeout: Duration,
}

impl HostNetworkBackend {
    #[must_use]
    pub fn new(applied_path: impl Into<PathBuf>) -> Self {
        Self {
            applied_path: applied_path.into(),
            reachability_timeout: DEFAULT_REACHABILITY_TIMEOUT,
        }
    }

    #[must_use]
    pub fn with_reachability_timeout(mut self, timeout: Duration) -> Self {
        self.reachability_timeout = timeout;
        self
    }

    pub fn apply(&self, snapshot: &HostNetworkSnapshot) -> MeshResult<HostNetworkApplyReport> {
        self.validate_reachable(snapshot)?;
        write_host_network_snapshot(&self.applied_path, snapshot)?;

        Ok(HostNetworkApplyReport {
            revision: snapshot.revision,
            route_count: snapshot.routes.len(),
            backend_count: snapshot.backend_count(),
        })
    }

    pub fn last_applied(
        &self,
        expected_island: &IslandId,
    ) -> MeshResult<Option<HostNetworkSnapshot>> {
        load_optional_host_network_snapshot(&self.applied_path, expected_island)
    }

    fn validate_reachable(&self, snapshot: &HostNetworkSnapshot) -> MeshResult<()> {
        for route in &snapshot.routes {
            for endpoint in &route.backends {
                let address = endpoint.address.socket_addr();
                TcpStream::connect_timeout(&address, self.reachability_timeout).map_err(
                    |source| MeshError::HostEndpointUnreachable {
                        address,
                        operation: "connect",
                        message: source.to_string(),
                    },
                )?;
            }
        }
        Ok(())
    }
}

pub fn write_host_network_snapshot(path: &Path, snapshot: &HostNetworkSnapshot) -> MeshResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| snapshot_io("create parent", parent, error))?;
    }
    reject_symlink(path)?;
    let bytes =
        serde_json::to_vec_pretty(snapshot).map_err(|error| MeshError::InvalidSnapshot {
            path: path.display().to_string(),
            message: error.to_string(),
        })?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temp =
        NamedTempFile::new_in(parent).map_err(|error| snapshot_io("create temp", parent, error))?;
    temp.write_all(&bytes)
        .map_err(|error| snapshot_io("write temp", temp.path(), error))?;
    temp.as_file_mut()
        .sync_all()
        .map_err(|error| snapshot_io("sync temp", temp.path(), error))?;
    reject_symlink(path)?;
    temp.persist(path)
        .map(|_file| ())
        .map_err(|error| snapshot_io("persist", path, error.error))
}

pub fn load_host_network_snapshot(
    path: &Path,
    expected_island: &IslandId,
) -> MeshResult<HostNetworkSnapshot> {
    let bytes = read_host_network_snapshot(path)?.ok_or_else(|| MeshError::SnapshotIo {
        operation: "read",
        path: path.display().to_string(),
        message: "snapshot does not exist".to_string(),
    })?;
    decode_host_network_snapshot(path, expected_island, &bytes)
}

fn load_optional_host_network_snapshot(
    path: &Path,
    expected_island: &IslandId,
) -> MeshResult<Option<HostNetworkSnapshot>> {
    let Some(bytes) = read_host_network_snapshot(path)? else {
        return Ok(None);
    };
    decode_host_network_snapshot(path, expected_island, &bytes).map(Some)
}

fn read_host_network_snapshot(path: &Path) -> MeshResult<Option<Vec<u8>>> {
    reject_symlink(path)?;
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(snapshot_io("read", path, error)),
    }
}

fn decode_host_network_snapshot(
    path: &Path,
    expected_island: &IslandId,
    bytes: &[u8],
) -> MeshResult<HostNetworkSnapshot> {
    let snapshot = serde_json::from_slice::<HostNetworkSnapshot>(bytes).map_err(|error| {
        MeshError::InvalidSnapshot {
            path: path.display().to_string(),
            message: error.to_string(),
        }
    })?;
    if snapshot.island != expected_island.as_str() {
        return Err(MeshError::InvalidSnapshot {
            path: path.display().to_string(),
            message: format!(
                "snapshot island {} did not match expected {}",
                snapshot.island,
                expected_island.as_str()
            ),
        });
    }
    Ok(snapshot)
}

fn reject_symlink(path: &Path) -> MeshResult<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        return Err(MeshError::InvalidSnapshot {
            path: path.display().to_string(),
            message: "symlink target rejected".to_string(),
        });
    }
    Ok(())
}

fn snapshot_io(operation: &'static str, path: &Path, error: std::io::Error) -> MeshError {
    MeshError::SnapshotIo {
        operation,
        path: path.display().to_string(),
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use mvp_bus::IslandId;
    use mvp_identity::NodeId;
    use mvp_projection::{BackendEndpoint, GatewayProjection, GatewayRouteProjection, RouteId};
    use tempfile::tempdir;

    use super::{
        HostNetworkBackend, HostNetworkSnapshot, load_host_network_snapshot,
        write_host_network_snapshot,
    };

    #[test]
    fn host_snapshot_converts_gateway_routes_to_typed_socket_endpoints() {
        let gateway = gateway_with_backend("127.0.0.1:3000");

        let snapshot = HostNetworkSnapshot::from_gateway(&IslandId::new("prod"), 42, &gateway)
            .expect("snapshot");

        assert_eq!(snapshot.revision, 42);
        assert_eq!(
            snapshot.routes[0].backends[0].address.to_string(),
            "127.0.0.1:3000"
        );
    }

    #[test]
    fn host_snapshot_rejects_non_socket_backend_address() {
        let gateway = gateway_with_backend("http://127.0.0.1:3000");

        let error = HostNetworkSnapshot::from_gateway(&IslandId::new("prod"), 1, &gateway)
            .expect_err("invalid endpoint");

        assert!(matches!(
            error,
            crate::MeshError::InvalidHostEndpoint { .. }
        ));
    }

    #[test]
    fn host_backend_persists_last_applied_snapshot_after_reachability_probe() {
        let temp = tempdir().expect("tempdir");
        let server = TestServer::start();
        let gateway = gateway_with_backend(server.address().to_string());
        let snapshot = HostNetworkSnapshot::from_gateway(&IslandId::new("prod"), 7, &gateway)
            .expect("snapshot");
        let backend = HostNetworkBackend::new(temp.path().join("host-network.snapshot"));

        let report = backend.apply(&snapshot).expect("apply host network");

        assert_eq!(report.backend_count, 1);
        assert_eq!(
            backend
                .last_applied(&IslandId::new("prod"))
                .expect("load last applied"),
            Some(snapshot)
        );
    }

    #[test]
    fn host_backend_keeps_last_applied_state_independent_of_backend_instance() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("host-network.snapshot");
        let server = TestServer::start();
        let gateway = gateway_with_backend(server.address().to_string());
        let snapshot = HostNetworkSnapshot::from_gateway(&IslandId::new("prod"), 9, &gateway)
            .expect("snapshot");

        HostNetworkBackend::new(&path)
            .apply(&snapshot)
            .expect("apply host network");

        assert_eq!(
            HostNetworkBackend::new(&path)
                .last_applied(&IslandId::new("prod"))
                .expect("load last applied"),
            Some(snapshot)
        );
    }

    #[test]
    fn host_snapshot_round_trips_and_checks_island() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("host-network.snapshot");
        let snapshot = HostNetworkSnapshot {
            island: "prod".to_string(),
            revision: 3,
            routes: Vec::new(),
        };

        write_host_network_snapshot(&path, &snapshot).expect("write snapshot");

        assert_eq!(
            load_host_network_snapshot(&path, &IslandId::new("prod")).expect("load snapshot"),
            snapshot
        );
        assert!(load_host_network_snapshot(&path, &IslandId::new("dev")).is_err());
    }

    fn gateway_with_backend(address: impl Into<String>) -> GatewayProjection {
        GatewayProjection {
            gateway_commit_id: "gateway-1".to_string(),
            route_commit_id: "route-1".to_string(),
            routes: vec![GatewayRouteProjection {
                route_id: RouteId::new("web"),
                hostnames: vec!["web.example.test".to_string()],
                backends: vec![BackendEndpoint {
                    node_id: NodeId::new("node-2"),
                    address: address.into(),
                }],
                old_backends_to_drain: Vec::new(),
            }],
        }
    }

    struct TestServer {
        address: std::net::SocketAddr,
    }

    impl TestServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
            let address = listener.local_addr().expect("test server address");
            thread::spawn(move || {
                let _ = listener.accept();
            });
            Self { address }
        }

        fn address(&self) -> std::net::SocketAddr {
            self.address
        }
    }
}
