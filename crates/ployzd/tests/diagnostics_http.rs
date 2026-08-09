use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr, TcpListener as StdTcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use ployz_core::corrosion::{
    AutomaticHostnameMode, ClusterDocument, CorrosionDocumentVersion, CorrosionTable,
    CorrosionTimestamp, MachineDocument, MachineStorageSelection, MachineStorageSelectionReason,
    MachineTransport, MeshProvider, OperationInitiator, OperatorWriteProvenance, PeerDocument,
    PeerTransport, Statement, StorageMode, StoredRow,
};
use ployz_core::ids::{ClusterName, MachineName, PeerName};
use ployz_core::machine::MachineLifecycle;
use ployz_core::network::{MachineEndpointSubnet, MachineEndpointSupernet};
use ployz_core::{
    ApiRefusal, DoctorDocument, StatusBarrier, StatusDegradationReason, StatusDocument, StatusSync,
};
use ployzd::corrosion::{BearerToken, CorrosionClientBounds, CorrosionClientConfig};
use ployzd::roles::api::http::{ApiRoleConfig, ApiServer, BootstrapSecret};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

const CLUSTER: &str = "main";
const MACHINE: &str = "machine-one";
const PEER: &str = "operator";

#[tokio::test]
async fn initial_empty_database_serves_only_get_status_as_no_roster() {
    let corrosion = FakeCorrosion::start(FakeMode::Empty).await;
    let api = RunningApi::start(&corrosion).await;

    let response = api.request(Method::GET, "/status").await;

    assert_eq!(response.status(), StatusCode::OK);
    let status = response
        .json::<StatusDocument>()
        .await
        .expect("status JSON");
    assert_eq!(status.cluster, None);
    assert_eq!(status.barrier, StatusBarrier::NoRoster);
    assert!(status.machines.is_empty());
    assert_eq!(corrosion.health_requests.load(Ordering::SeqCst), 1);
    assert_eq!(
        corrosion.queries().len(),
        6,
        "three auth and three status reads"
    );

    for (method, path) in [(Method::POST, "/status"), (Method::GET, "/doctor")] {
        let response = api.request(method, path).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.json::<ApiRefusal>().await.expect("refusal JSON"),
            ApiRefusal::MissingCluster
        );
    }

    api.stop().await;
}

#[tokio::test]
async fn accepted_cluster_with_empty_roster_serves_only_get_status_as_catching_up() {
    let corrosion = FakeCorrosion::start(FakeMode::AcceptedClusterEmptyRoster).await;
    let api = RunningApi::start(&corrosion).await;

    let response = api.request(Method::GET, "/status").await;

    assert_eq!(response.status(), StatusCode::OK);
    let status = response
        .json::<StatusDocument>()
        .await
        .expect("status JSON");
    assert_eq!(status.cluster.expect("accepted cluster").id, cluster_id());
    assert_eq!(status.barrier, StatusBarrier::CatchingUp);
    assert!(status.machines.is_empty());

    for (method, path) in [(Method::POST, "/status"), (Method::GET, "/doctor")] {
        let response = api.request(method, path).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(matches!(
            response.json::<ApiRefusal>().await.expect("refusal JSON"),
            ApiRefusal::UnknownSource { .. }
        ));
    }

    api.stop().await;
}

#[tokio::test]
async fn nonmatching_source_in_nonempty_roster_remains_forbidden_from_status() {
    let corrosion = FakeCorrosion::start(FakeMode::NonMatchingRoster).await;
    let api = RunningApi::start(&corrosion).await;

    let response = api.request(Method::GET, "/status").await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(matches!(
        response.json::<ApiRefusal>().await.expect("refusal JSON"),
        ApiRefusal::UnknownSource { .. }
    ));
    assert_eq!(corrosion.health_requests.load(Ordering::SeqCst), 0);
    api.stop().await;
}

#[tokio::test]
async fn invalid_cluster_never_reaches_status_diagnostics() {
    let corrosion = FakeCorrosion::start(FakeMode::InvalidCluster).await;
    let api = RunningApi::start(&corrosion).await;

    let response = api.request(Method::GET, "/status").await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.json::<ApiRefusal>().await.expect("refusal JSON"),
        ApiRefusal::InvalidCluster
    );
    assert_eq!(corrosion.health_requests.load(Ordering::SeqCst), 0);
    api.stop().await;
}

#[tokio::test]
async fn status_normalizes_invalid_corrosion_health_without_exposing_its_body() {
    let corrosion = FakeCorrosion::start(FakeMode::InvalidHealth).await;
    let api = RunningApi::start(&corrosion).await;

    let response = api.request(Method::GET, "/status").await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.expect("status body");
    assert!(!body.contains("test-secret"));
    let status = serde_json::from_str::<StatusDocument>(&body).expect("status JSON");
    assert_eq!(
        status.sync,
        StatusSync::Degraded {
            reason: StatusDegradationReason::InvalidCorrosionHealthResponse,
        }
    );
    api.stop().await;
}

#[tokio::test]
async fn authenticated_doctor_performs_one_read_only_sweep_of_every_table() {
    let corrosion = FakeCorrosion::start(FakeMode::StableRoster).await;
    let api = RunningApi::start(&corrosion).await;

    let response = api.request(Method::GET, "/doctor").await;

    assert_eq!(response.status(), StatusCode::OK);
    let doctor = response
        .json::<DoctorDocument>()
        .await
        .expect("doctor JSON");
    assert!(doctor.skipped_newer_versions.is_empty());
    assert!(doctor.foreign_clusters.is_empty());
    let queries = corrosion.queries();
    assert_eq!(
        queries.len(),
        17,
        "three auth reads plus fourteen doctor reads"
    );
    assert!(queries.iter().all(|query| query.starts_with("SELECT ")));
    let [_, _, _, doctor_queries @ ..] = queries.as_slice() else {
        panic!("doctor request did not perform its complete query sweep");
    };
    for table in CorrosionTable::ALL {
        let expected = match table {
            CorrosionTable::MachineStatus | CorrosionTable::GatewayObservations => {
                format!("SELECT machine_id AS id, document FROM {}", table.as_str())
            }
            CorrosionTable::Cluster
            | CorrosionTable::Machines
            | CorrosionTable::Peers
            | CorrosionTable::Tokens
            | CorrosionTable::Namespaces
            | CorrosionTable::Services
            | CorrosionTable::RouteBindings
            | CorrosionTable::Containers
            | CorrosionTable::Operations
            | CorrosionTable::CertHoldings
            | CorrosionTable::AcmeHttp01
            | CorrosionTable::Controller => {
                format!("SELECT id, document FROM {}", table.as_str())
            }
        };
        assert_eq!(
            doctor_queries
                .iter()
                .filter(|query| **query == expected)
                .count(),
            1,
            "table {} is swept exactly once",
            table.as_str()
        );
    }
    api.stop().await;
}

struct RunningApi {
    addr: SocketAddr,
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl RunningApi {
    async fn start(corrosion: &FakeCorrosion) -> Self {
        let addr = unused_loopback_addr();
        let config = ApiRoleConfig::new_founding(
            corrosion.config(),
            cluster_id(),
            machine_id(),
            addr,
            "diagnostics-test".to_owned(),
            BootstrapSecret::new("unused-test-secret").expect("bootstrap secret"),
        )
        .expect("API config");
        let server = ApiServer::bind_without_join_door_for_integration_test(config)
            .await
            .expect("API server");
        let (shutdown, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            server
                .serve(async move {
                    let _ = stopped.await;
                })
                .await
                .expect("API server stops cleanly");
        });
        Self {
            addr,
            shutdown,
            task,
        }
    }

    async fn request(&self, method: Method, path: &str) -> reqwest::Response {
        reqwest::Client::new()
            .request(method, format!("http://{}{path}", self.addr))
            .send()
            .await
            .expect("diagnostics request")
    }

    async fn stop(self) {
        let _ = self.shutdown.send(());
        self.task.await.expect("API task");
    }
}

#[derive(Clone, Copy)]
enum FakeMode {
    Empty,
    AcceptedClusterEmptyRoster,
    InvalidCluster,
    InvalidHealth,
    NonMatchingRoster,
    StableRoster,
}

struct FakeCorrosion {
    addr: SocketAddr,
    queries: Arc<Mutex<Vec<String>>>,
    health_requests: Arc<AtomicUsize>,
    _task: tokio::task::JoinHandle<()>,
}

impl FakeCorrosion {
    async fn start(mode: FakeMode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Corrosion listener");
        let addr = listener.local_addr().expect("Corrosion address");
        let queries = Arc::new(Mutex::new(Vec::new()));
        let health_requests = Arc::new(AtomicUsize::new(0));
        let task_queries = Arc::clone(&queries);
        let task_health_requests = Arc::clone(&health_requests);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let queries = Arc::clone(&task_queries);
                let health_requests = Arc::clone(&task_health_requests);
                tokio::spawn(async move {
                    let result = http1::Builder::new()
                        .serve_connection(
                            TokioIo::new(stream),
                            service_fn(move |request| {
                                fake_corrosion_response(
                                    request,
                                    mode,
                                    Arc::clone(&queries),
                                    Arc::clone(&health_requests),
                                )
                            }),
                        )
                        .await;
                    assert!(result.is_ok(), "fake Corrosion connection: {result:?}");
                });
            }
        });
        Self {
            addr,
            queries,
            health_requests,
            _task: task,
        }
    }

    fn config(&self) -> CorrosionClientConfig {
        CorrosionClientConfig::new(
            self.addr,
            BearerToken::new("diagnostics-test-token").expect("token"),
            CorrosionClientBounds {
                connect_timeout: Duration::from_secs(1),
                request_timeout: Duration::from_secs(2),
                stream_idle_timeout: Duration::from_secs(2),
                max_ndjson_frame_bytes: 64 * 1024,
                max_error_body_bytes: 4096,
            },
        )
        .expect("Corrosion config")
    }

    fn queries(&self) -> Vec<String> {
        self.queries.lock().expect("query lock").clone()
    }
}

async fn fake_corrosion_response(
    request: Request<hyper::body::Incoming>,
    mode: FakeMode,
    queries: Arc<Mutex<Vec<String>>>,
    health_requests: Arc<AtomicUsize>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    if request.uri().path() == "/v1/health" {
        health_requests.fetch_add(1, Ordering::SeqCst);
        if matches!(mode, FakeMode::InvalidHealth) {
            return Ok(json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                br#"{"error":"database unavailable: test-secret"}"#.to_vec(),
            ));
        }
        return Ok(json_response(
            StatusCode::OK,
            br#"{"response":{"gaps":0,"members":1,"p99_lag":0.0,"queue_size":0}}"#.to_vec(),
        ));
    }
    assert_eq!(request.uri().path(), "/v1/queries");
    let body = request
        .into_body()
        .collect()
        .await
        .expect("query request body")
        .to_bytes();
    let statement: Statement = serde_json::from_slice(&body).expect("query statement");
    let sql = match statement {
        Statement::Simple(sql) | Statement::WithParams(sql, _) => sql,
    };
    queries.lock().expect("query lock").push(sql.clone());
    let rows = match mode {
        FakeMode::Empty => Vec::new(),
        FakeMode::InvalidCluster if sql.contains("FROM cluster") => {
            vec![StoredRow::new(CLUSTER, "{}")]
        }
        FakeMode::InvalidCluster => Vec::new(),
        FakeMode::AcceptedClusterEmptyRoster
        | FakeMode::InvalidHealth
        | FakeMode::NonMatchingRoster
        | FakeMode::StableRoster
            if sql.contains("FROM cluster") =>
        {
            vec![StoredRow::new(
                CLUSTER,
                serde_json::to_string(&cluster_document()).expect("cluster document"),
            )]
        }
        FakeMode::InvalidHealth | FakeMode::NonMatchingRoster | FakeMode::StableRoster
            if sql.contains("FROM machines") =>
        {
            vec![StoredRow::new(
                MACHINE,
                serde_json::to_string(&machine_document()).expect("machine document"),
            )]
        }
        FakeMode::InvalidHealth | FakeMode::StableRoster if sql.contains("FROM peers") => {
            vec![StoredRow::new(
                PEER,
                serde_json::to_string(&peer_document()).expect("peer document"),
            )]
        }
        FakeMode::NonMatchingRoster if sql.contains("FROM peers") => vec![StoredRow::new(
            PEER,
            serde_json::to_string(&nonmatching_peer_document()).expect("peer document"),
        )],
        FakeMode::AcceptedClusterEmptyRoster
        | FakeMode::InvalidHealth
        | FakeMode::NonMatchingRoster
        | FakeMode::StableRoster => Vec::new(),
    };
    Ok(json_response(StatusCode::OK, query_frames(&rows)))
}

fn json_response(status: StatusCode, body: Vec<u8>) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(body)));
    *response.status_mut() = status;
    response
}

fn query_frames(rows: &[StoredRow]) -> Vec<u8> {
    let mut frames = br#"{"columns":["id","document"]}
"#
    .to_vec();
    for (index, row) in rows.iter().enumerate() {
        frames.extend_from_slice(
            serde_json::to_string(&serde_json::json!({
                "row": [index, [row.key, row.document]],
            }))
            .expect("query row")
            .as_bytes(),
        );
        frames.push(b'\n');
    }
    frames.extend_from_slice(
        br#"{"eoq":{"time":0.0}}
"#,
    );
    frames
}

fn unused_loopback_addr() -> SocketAddr {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("unused API address");
    listener.local_addr().expect("API address")
}

fn cluster_id() -> ClusterName {
    ClusterName::try_new(CLUSTER).expect("cluster id")
}

fn machine_id() -> MachineName {
    MachineName::try_new(MACHINE).expect("machine id")
}

fn provenance() -> OperatorWriteProvenance {
    OperatorWriteProvenance {
        written_by: OperationInitiator::Machine {
            machine_id: machine_id(),
        },
        written_at: CorrosionTimestamp::try_new("2026-08-05T00:00:00Z").expect("timestamp"),
    }
}

fn cluster_document() -> ClusterDocument {
    ClusterDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: cluster_id(),
        provenance: provenance(),
        name: "diagnostics".to_owned(),
        storage_default: StorageMode::Plain,
        hostname_mode: AutomaticHostnameMode::Disabled,
        prefix: MachineEndpointSupernet::default_v1(),
        provider: MeshProvider::Tailscale,
        acme_directory_url: "https://acme.invalid/directory".to_owned(),
        acme_contact: None,
    }
}

fn machine_document() -> MachineDocument {
    MachineDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: cluster_id(),
        provenance: provenance(),
        name: MachineName::try_new("machine-one").expect("machine name"),
        lifecycle: MachineLifecycle::Active,
        transport: MachineTransport::Tailscale {
            ip: Ipv4Addr::new(127, 0, 0, 2),
            subnet_v4: MachineEndpointSubnet::try_new("10.210.0.0/24").expect("machine subnet"),
        },
        storage: MachineStorageSelection {
            mode: StorageMode::Plain,
            reason: MachineStorageSelectionReason::Default,
        },
    }
}

fn peer_document() -> PeerDocument {
    PeerDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: cluster_id(),
        provenance: provenance(),
        name: PeerName::try_new("operator").expect("peer name"),
        transport: PeerTransport::Tailscale {
            ip: Ipv4Addr::LOCALHOST,
        },
    }
}

fn nonmatching_peer_document() -> PeerDocument {
    PeerDocument {
        transport: PeerTransport::Tailscale {
            ip: Ipv4Addr::new(127, 0, 0, 3),
        },
        ..peer_document()
    }
}
