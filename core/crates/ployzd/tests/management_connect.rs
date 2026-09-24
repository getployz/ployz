//! Connection contract of the management transport: the public client connector against
//! a real Machine API served over an in-process relay, asserting only what a client sees.

use std::{
    collections::HashSet,
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use iroh::{
    Endpoint, EndpointAddr, RelayMode, SecretKey,
    endpoint::{WeakConnectionHandle, presets},
    test_utils::run_relay_server,
    tls::CaTlsConfig,
};
use ployz::{
    connect::{ConnectError, Connector, ManagementRelay, SystemConnector},
    context::Connection,
};
use ployz_core::{
    AdvertisedEndpoint, DescribeContractRequest, InitializeRequest, MANAGEMENT_ALPN, MachineId,
    MachineName, MachineRpcClient, ManagementCapability, ManagementClientLabel, RpcErrorCode,
    SetManagementClientRequest, op,
};
use ployzd::{
    machine::{LocalMachine, LocalMachineStore, RecordOwner},
    machine_api::MachineApi,
    management::{self, ManagementConfig},
};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use tonic::{
    body::Body,
    codegen::{Service, http},
    transport::Channel,
};

#[tokio::test(flavor = "multi_thread")]
async fn client_connector_honours_the_management_transport_contract() {
    tokio::time::timeout(Duration::from_secs(120), contract())
        .await
        .expect("contract test timed out");
}

#[tokio::test(flavor = "multi_thread")]
async fn verification_racing_removal_does_not_revoke_the_saved_candidate() {
    let (_map, relay_url, _relay) = run_relay_server().await.unwrap();
    let (_dir, owner, local) = participating().await;
    let old = local
        .set_management_client(SetManagementClientRequest::Set { label: cloud() })
        .await
        .unwrap()
        .capability
        .unwrap();
    let old_key = *SecretKey::from_bytes(old.client_secret())
        .public()
        .as_bytes();
    local.activate_management_client(old_key).await.unwrap();
    let endpoint = management::bind(
        local.record().management_secret(),
        &ManagementConfig {
            relay_url: relay_url.clone(),
            port: 0,
            relay_tls: CaTlsConfig::insecure_skip_verify(),
        },
    )
    .await
    .unwrap();
    let shutdown = CancellationToken::new();
    let server = tokio::spawn(management::serve(
        endpoint,
        local.clone(),
        MachineApi::builder(owner).build(),
        shutdown.clone(),
    ));
    let connector = Arc::new(SystemConnector::default().with_management_relay(
        ManagementRelay::custom(relay_url, CaTlsConfig::insecure_skip_verify()),
    ));
    let previous = connector.connect(&connection(&old)).await.unwrap();
    let replacement = rotate(previous.clone()).await;
    // Cloud has not committed replacement publication: negotiation may only verify identity.
    let verified =
        ployz::sdk::connect_connections(vec![connection(&replacement)], connector.clone())
            .await
            .unwrap();
    assert_eq!(local.record().accepted_client(&cloud()), Some(old_key));
    // Removal wins publication and retains only the previous saved capability.
    let _ = MachineRpcClient::new(previous)
        .set_management_client(
            op::SetManagementClient::into_request(SetManagementClientRequest::Clear {
                label: cloud(),
            })
            .encode()
            .unwrap(),
        )
        .await;
    // Clear may revoke its own response; the authenticated cleared response is confirmation.
    wait_until(|| !local.record().has_management_clients()).await;
    assert!(matches!(
        connector.connect(&connection(&replacement)).await,
        Err(ConnectError::PairingCleared)
    ));
    drop(verified);
    shutdown.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn rotating_or_clearing_one_slot_revokes_only_its_holder() {
    let (_map, relay_url, _relay) = run_relay_server().await.unwrap();
    let (_dir, owner, local) = participating().await;
    let cli = ManagementClientLabel::parse("cli").unwrap();
    let set = |label: ManagementClientLabel| {
        let local = local.clone();
        async move {
            let capability = local
                .set_management_client(SetManagementClientRequest::Set { label })
                .await
                .unwrap()
                .capability
                .unwrap();
            let key = *SecretKey::from_bytes(capability.client_secret())
                .public()
                .as_bytes();
            local.activate_management_client(key).await.unwrap();
            capability
        }
    };
    let cloud_old = set(cloud()).await;
    let cli_capability = set(cli.clone()).await;
    let endpoint = management::bind(
        local.record().management_secret(),
        &ManagementConfig {
            relay_url: relay_url.clone(),
            port: 0,
            relay_tls: CaTlsConfig::insecure_skip_verify(),
        },
    )
    .await
    .unwrap();
    let shutdown = CancellationToken::new();
    let server = tokio::spawn(management::serve(
        endpoint,
        local.clone(),
        MachineApi::builder(owner).build(),
        shutdown.clone(),
    ));
    let connector = SystemConnector::default().with_management_relay(ManagementRelay::custom(
        relay_url,
        CaTlsConfig::insecure_skip_verify(),
    ));
    let cloud_channel = connector.connect(&connection(&cloud_old)).await.unwrap();
    let cli_channel = connector
        .connect(&connection(&cli_capability))
        .await
        .unwrap();
    let machine_id = describe(cli_channel.clone()).await.unwrap();

    // Rotating `cloud` revokes the old cloud key only.
    let cloud_new = set(cloud()).await;
    revoked(cloud_channel).await;
    assert_eq!(describe(cli_channel.clone()).await.unwrap(), machine_id);
    let cloud_channel = connector.connect(&connection(&cloud_new)).await.unwrap();

    // Clearing `cli` revokes the cli key only.
    local
        .set_management_client(SetManagementClientRequest::Clear { label: cli })
        .await
        .unwrap();
    revoked(cli_channel).await;
    assert_eq!(describe(cloud_channel).await.unwrap(), machine_id);
    assert_eq!(
        local.record().management_clients().collect::<Vec<_>>(),
        [&cloud()]
    );

    shutdown.cancel();
    server.await.unwrap().unwrap();
}

async fn contract() {
    let (_relay_map, relay_url, _relay) = run_relay_server().await.unwrap();
    let (_dir, owner, local) = participating().await;
    let machine_id = local.record().id();
    let capability = local
        .set_management_client(SetManagementClientRequest::Set { label: cloud() })
        .await
        .unwrap()
        .capability
        .unwrap();

    let config = ManagementConfig {
        relay_url: relay_url.clone(),
        port: 0,
        relay_tls: CaTlsConfig::insecure_skip_verify(),
    };
    let endpoint = management::bind(local.record().management_secret(), &config)
        .await
        .unwrap();
    let shutdown = CancellationToken::new();
    let (connections, mut observed) = mpsc::unbounded_channel();
    let gate = Arc::new(RequestGate::default());
    let api = ObservedApi {
        api: MachineApi::builder(owner.clone()).build(),
        connections,
        seen: Arc::default(),
        gate: Arc::clone(&gate),
    };
    let server = tokio::spawn(management::serve(
        endpoint.clone(),
        local.clone(),
        api,
        shutdown.clone(),
    ));
    let connector = Arc::new(
        SystemConnector::new("/ployz-missing-ssh-client").with_management_relay(
            ManagementRelay::custom(relay_url.clone(), CaTlsConfig::insecure_skip_verify()),
        ),
    );

    // A key the Machine never accepted is refused by identity, not by reachability.
    let stranger =
        ManagementCapability::new(*capability.machine(), SecretKey::generate().to_bytes());
    assert_refused(connector.connect(&connection(&stranger)).await);

    // Fifty concurrent RPC streams over one accepted connection all complete.
    let channel = connector.connect(&connection(&capability)).await.unwrap();
    let surviving_connection = observed.recv().await.unwrap();
    activate(channel.clone()).await;
    gate.hold.store(true, Ordering::SeqCst);
    let calls: Vec<_> = (0..50)
        .map(|_| tokio::spawn(describe(channel.clone())))
        .collect();
    wait_until(|| gate.active.load(Ordering::SeqCst) == 50).await;
    gate.hold.store(false, Ordering::SeqCst);
    gate.release.add_permits(50);
    for call in calls {
        assert_eq!(call.await.unwrap().unwrap(), machine_id);
    }

    // Cancelling in-flight streams and dropping their connection leaves the surviving
    // connection and fresh dials unaffected.
    let cancelled = connector.connect(&connection(&capability)).await.unwrap();
    let cancelled_connection = observed.recv().await.unwrap();
    gate.hold.store(true, Ordering::SeqCst);
    let in_flight: Vec<_> = (0..50)
        .map(|_| tokio::spawn(describe(cancelled.clone())))
        .collect();
    wait_until(|| gate.active.load(Ordering::SeqCst) == 50).await;
    for call in &in_flight {
        call.abort();
    }
    for call in in_flight {
        assert!(call.await.unwrap_err().is_cancelled());
    }
    drop(cancelled);
    tokio::time::timeout(Duration::from_secs(10), cancelled_connection.closed())
        .await
        .unwrap();
    wait_until(|| gate.active.load(Ordering::SeqCst) == 0).await;
    gate.hold.store(false, Ordering::SeqCst);
    assert_eq!(describe(channel.clone()).await.unwrap(), machine_id);
    let session = ployz::sdk::connect_connections(vec![connection(&capability)], connector.clone())
        .await
        .unwrap();
    let removal_connection = observed.recv().await.unwrap();

    // Revocation must also reach a peer that completed TLS but delayed its first stream.
    // Use a distinct identity to avoid replacing the connector's relay registration.
    // Lose the Set acknowledgement on the very connection being rotated. The
    // original capability still works, including after reconnecting, until a
    // replacement proves possession. A later Set retires the abandoned candidate.
    let abandoned = rotate(channel.clone()).await;
    assert_eq!(describe(channel.clone()).await.unwrap(), machine_id);
    let retry = connector.connect(&connection(&capability)).await.unwrap();
    let retry_connection = observed.recv().await.unwrap();
    let delayed_capability = rotate(retry.clone()).await;
    assert_eq!(describe(retry.clone()).await.unwrap(), machine_id);
    assert!(matches!(
        connector.connect(&connection(&abandoned)).await,
        Err(ConnectError::RefusedByIdentity)
    ));
    drop(retry);
    tokio::time::timeout(Duration::from_secs(10), retry_connection.closed())
        .await
        .unwrap();
    let delayed_secret = SecretKey::from_bytes(delayed_capability.client_secret());
    let delayed = Endpoint::builder(presets::Minimal)
        .secret_key(delayed_secret)
        .relay_mode(RelayMode::custom([relay_url.clone()]))
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .bind()
        .await
        .unwrap();
    local
        .activate_management_client(*delayed.id().as_bytes())
        .await
        .unwrap();
    let waiting = delayed
        .connect(
            EndpointAddr::new(endpoint.id()).with_relay_url(relay_url),
            MANAGEMENT_ALPN,
        )
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), waiting.closed())
            .await
            .is_err(),
        "accepted peer must remain connected before rotation"
    );
    let capability = local
        .set_management_client(SetManagementClientRequest::Set { label: cloud() })
        .await
        .unwrap()
        .capability
        .unwrap();
    // Persist before Cloud activates the replacement; losing its HTTP reply must
    // leave a freshly loaded CLI context able to reconnect.
    let cli_dir = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(cli_dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = ployz::context::Config::new(
        cli_dir.path().join("config.yaml"),
        Some("cloud".into()),
        std::collections::BTreeMap::from([(
            "cloud".into(),
            ployz::context::Context {
                connections: vec![connection(&delayed_capability)],
            },
        )]),
    );
    config.save().unwrap();
    config.save_management_capability(&capability).unwrap();
    let activated = connector.connect(&connection(&capability)).await.unwrap();
    let activated_connection = observed.recv().await.unwrap();
    activate(activated.clone()).await;
    assert_eq!(
        local.record().accepted_client(&cloud()),
        Some(
            *SecretKey::from_bytes(capability.client_secret())
                .public()
                .as_bytes()
        )
    );
    drop(activated);
    tokio::time::timeout(Duration::from_secs(10), activated_connection.closed())
        .await
        .unwrap();
    let saved = ployz::context::Config::load(config.path()).unwrap();
    let resumed = connector
        .connect(
            saved
                .contexts
                .get("cloud")
                .unwrap()
                .connections
                .first()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(describe(resumed.clone()).await.unwrap(), machine_id);
    let resumed_connection = observed.recv().await.unwrap();
    drop(resumed);
    tokio::time::timeout(Duration::from_secs(10), resumed_connection.closed())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), waiting.closed())
        .await
        .expect("rotation must revoke a peer waiting to open its first stream");
    delayed.close().await;
    drop(session);
    tokio::time::timeout(Duration::from_secs(10), removal_connection.closed())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), surviving_connection.closed())
        .await
        .unwrap();
    drop(channel);

    // Exercise Cloud's actual SDK path. A lost Clear response is not confirmation;
    // the subsequent SDK connect must preserve the daemon's identity refusal.
    let session = ployz::sdk::connect_connections(vec![connection(&capability)], connector.clone())
        .await
        .unwrap();
    let removal_connection = observed.recv().await.unwrap();
    let replaced = match ployz::sdk::connect_connections(
        vec![connection(&abandoned)],
        connector.clone(),
    )
    .await
    {
        Ok(_) => panic!("abandoned capability must be refused"),
        Err(error) => error,
    };
    assert_eq!(replaced.code, RpcErrorCode::Unauthenticated);
    assert_eq!(replaced.details, serde_json::Value::Null);
    let _ = session.clear_management_client(cloud()).await;
    tokio::time::timeout(Duration::from_secs(10), removal_connection.closed())
        .await
        .unwrap();
    drop(session);
    let error =
        match ployz::sdk::connect_connections(vec![connection(&capability)], connector.clone())
            .await
        {
            Ok(_) => panic!("removed capability must not reconnect through the SDK"),
            Err(error) => error,
        };
    assert_eq!(error.code, RpcErrorCode::Unauthenticated);
    assert_eq!(
        error.details,
        serde_json::json!({ "management_pairing": "cleared" })
    );
    assert!(matches!(
        connector.connect(&connection(&capability)).await,
        Err(ConnectError::PairingCleared)
    ));

    // Shutdown drains every remaining connection and closes the endpoint.
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("serve must finish once every client is gone")
        .unwrap()
        .unwrap();
    assert!(endpoint.is_closed());
}

fn assert_refused(result: Result<Channel, ConnectError>) {
    match result {
        Err(ConnectError::RefusedByIdentity) => {}
        Err(error) => panic!("expected refusal by identity, got: {error} ({error:?})"),
        Ok(_) => panic!("expected refusal by identity, got a channel"),
    }
}

async fn participating() -> (tempfile::TempDir, RecordOwner, LocalMachine) {
    let dir = tempfile::tempdir().unwrap();
    let owner = RecordOwner::spawn(LocalMachineStore::open(dir.path()).unwrap()).unwrap();
    let local = LocalMachine::new(owner.clone());
    local
        .initialize(InitializeRequest {
            initial_policy: Default::default(),
            name: MachineName::parse("first").unwrap(),
            cluster_network: "10.210.0.0/16".parse().unwrap(),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            wireguard_mtu: None,
        })
        .await
        .unwrap();
    (dir, owner, local)
}

fn cloud() -> ManagementClientLabel {
    ManagementClientLabel::parse("cloud").unwrap()
}

async fn revoked(channel: Channel) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while describe(channel.clone()).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a revoked key's connection must stop serving");
}

fn connection(capability: &ManagementCapability) -> Connection {
    Connection::management(capability.to_secret_string()).unwrap()
}

async fn rotate(channel: Channel) -> ManagementCapability {
    MachineRpcClient::new(channel)
        .set_management_client(
            op::SetManagementClient::into_request(SetManagementClientRequest::Set {
                label: cloud(),
            })
            .encode()
            .unwrap(),
        )
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap()
        .decode::<op::SetManagementClient>()
        .unwrap()
        .capability
        .unwrap()
}

async fn activate(channel: Channel) {
    MachineRpcClient::new(channel)
        .machine_token(
            op::MachineToken::into_request(ployz_core::MachineTokenRequest::default())
                .encode()
                .unwrap(),
        )
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap()
        .decode::<op::MachineToken>()
        .unwrap();
}

async fn describe(channel: Channel) -> Result<MachineId, tonic::Status> {
    let response = MachineRpcClient::new(channel)
        .describe_contract(
            op::DescribeContract::into_request(DescribeContractRequest {})
                .encode()
                .unwrap(),
        )
        .await?;
    Ok(response
        .into_inner()
        .decode_response()
        .unwrap()
        .decode::<op::DescribeContract>()
        .unwrap()
        .machine_id)
}

async fn wait_until(predicate: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("observable connection state did not settle");
}

struct RequestGate {
    hold: AtomicBool,
    active: Arc<AtomicUsize>,
    release: Semaphore,
}

impl Default for RequestGate {
    fn default() -> Self {
        Self {
            hold: AtomicBool::new(false),
            active: Arc::default(),
            release: Semaphore::new(0),
        }
    }
}

struct ActiveRequest(Arc<AtomicUsize>);
impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

// Observe the connection metadata supplied to the real Machine API and hold requests
// at the service boundary so concurrency and cancellation cannot pass by scheduling luck.
#[derive(Clone)]
struct ObservedApi {
    api: MachineApi,
    connections: mpsc::UnboundedSender<WeakConnectionHandle>,
    seen: Arc<Mutex<HashSet<usize>>>,
    gate: Arc<RequestGate>,
}

impl Service<http::Request<Body>> for ObservedApi {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.api.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let weak = request
            .extensions()
            .get::<WeakConnectionHandle>()
            .unwrap()
            .clone();
        let connection = weak.upgrade().unwrap();
        if self.seen.lock().unwrap().insert(connection.stable_id()) {
            self.connections.send(weak).unwrap();
        }
        let future = self.api.call(request);
        let gate = Arc::clone(&self.gate);
        Box::pin(async move {
            if gate.hold.load(Ordering::SeqCst) {
                gate.active.fetch_add(1, Ordering::SeqCst);
                let _active = ActiveRequest(Arc::clone(&gate.active));
                gate.release.acquire().await.unwrap().forget();
            }
            future.await
        })
    }
}
