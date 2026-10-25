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
    endpoint::{VarInt, WeakConnectionHandle, presets},
    test_utils::run_relay_server,
    tls::CaTlsConfig,
};
use ployz::{
    connect::{ConnectError, Connector, ManagementRelay, SystemConnector, open_grant_registry},
    context::Connection,
};
use ployz_core::{
    AdvertisedEndpoint, ContainerId, DescribeContractRequest, DeviceKey, EnrollAs, EnrollRequest,
    ExecConfig, ExecOptions, ExecRequestFrame, ForgottenAnswer, MANAGEMENT_ALPN, MachineId,
    MachineName, MachineRpcClient, ManagementCapability, ManagementClientLabel, ManagementIdentity,
    RpcErrorCode, SetDeviceRequest, SetManagementClientRequest, TUNNEL_ALPN, TUNNEL_FAILED,
    TUNNEL_OPEN, op,
};
use ployzd::{
    corrosion::ReplicatedStore,
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

/// Two processes holding one capability, such as two CLI commands or Cloud's web and
/// worker, each dial the relay with their own endpoint. Neither may take the other's
/// traffic.
#[tokio::test(flavor = "multi_thread")]
async fn two_holders_of_one_capability_stay_independent_through_the_relay() {
    let (_map, relay_url, _relay) = run_relay_server().await.unwrap();
    let (_dir, owner, local) = participating().await;
    let capability = local
        .set_management_client(SetManagementClientRequest::Set { label: cloud() })
        .await
        .unwrap()
        .capability
        .unwrap();
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
        Arc::default(),
        None::<ReplicatedStore>,
        shutdown.clone(),
    ));
    let process = || {
        Arc::new(
            SystemConnector::default().with_management_relay(
                ManagementRelay::custom(relay_url.clone(), CaTlsConfig::insecure_skip_verify())
                    .relay_only(),
            ),
        )
    };
    let cli = process().connect(&connection(&capability)).await.unwrap();
    activate(cli.clone()).await;
    let cloud = ployz::sdk::connect_connections(vec![connection(&capability)], process())
        .await
        .unwrap();
    for _ in 0..3 {
        tokio::time::timeout(Duration::from_secs(5), describe(cli.clone()))
            .await
            .expect("the first holder stalled")
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), cloud.about())
            .await
            .expect("the second holder stalled")
            .unwrap();
    }
    drop(cloud);
    drop(cli);
    shutdown.cancel();
    server.await.unwrap();
}

/// A session held open in one process, as Cloud holds a runtime watch, must not make that
/// process's next dial wait on the held session's relay connection. Here that connection
/// goes silent without closing, as a dropped NAT or load-balancer mapping leaves it, and
/// iroh notices only at its next relay ping, 15 s on (#3309).
#[tokio::test(flavor = "multi_thread")]
async fn a_held_session_with_a_silent_relay_does_not_stall_the_next_dial() {
    let (_map, relay_url, _relay) = run_relay_server().await.unwrap();
    let proxy = FreezableProxy::start(&relay_url).await;
    let (_dir, owner, local) = participating().await;
    let capability = local
        .set_management_client(SetManagementClientRequest::Set { label: cloud() })
        .await
        .unwrap()
        .capability
        .unwrap();
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
        Arc::default(),
        None::<ReplicatedStore>,
        shutdown.clone(),
    ));
    let process: Arc<dyn Connector> = Arc::new(
        SystemConnector::default().with_management_relay(
            ManagementRelay::custom(proxy.url.clone(), CaTlsConfig::insecure_skip_verify())
                .relay_only(),
        ),
    );
    let held = ployz::sdk::connect_connections(vec![connection(&capability)], process.clone())
        .await
        .unwrap();
    proxy.freeze();
    let next = tokio::time::timeout(
        Duration::from_secs(5),
        ployz::sdk::connect_connections(vec![connection(&capability)], process),
    )
    .await
    .expect("the next dial waited on the held session's silent relay connection")
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), next.about())
        .await
        .expect("the next session stalled")
        .unwrap();
    drop(next);
    drop(held);
    shutdown.cancel();
    server.await.unwrap();
}

/// A TCP proxy in front of a relay. [`Self::freeze`] silences every connection open so
/// far without closing it; later connections pass through.
struct FreezableProxy {
    url: iroh::RelayUrl,
    open: Arc<Mutex<Vec<CancellationToken>>>,
}

impl FreezableProxy {
    async fn start(relay: &iroh::RelayUrl) -> Self {
        let target = format!(
            "{}:{}",
            relay.host_str().unwrap(),
            relay.port_or_known_default().unwrap()
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let open = Arc::new(Mutex::new(Vec::<CancellationToken>::new()));
        let tracked = Arc::clone(&open);
        tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                let frozen = CancellationToken::new();
                tracked.lock().unwrap().push(frozen.clone());
                let target = target.clone();
                tokio::spawn(async move {
                    let mut outbound = tokio::net::TcpStream::connect(target).await.unwrap();
                    tokio::select! {
                        () = frozen.cancelled() => {}
                        _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => return,
                    }
                    // Hold both sockets open and pass nothing more.
                    std::future::pending::<()>().await;
                    drop((inbound, outbound));
                });
            }
        });
        Self { url, open }
    }

    fn freeze(&self) {
        for frozen in self.open.lock().unwrap().iter() {
            frozen.cancel();
        }
    }
}

/// A proof binds to the connection it was made on: replayed on another connection, even
/// by a peer that saw it, it admits nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_machine_on_the_previous_management_protocol_is_named_as_older() {
    let (_map, relay_url, _relay) = run_relay_server().await.unwrap();
    let machine = SecretKey::generate();
    let capability = ManagementCapability::new(
        ployz_core::ManagementIdentity::from_bytes(*machine.public().as_bytes()),
        SecretKey::generate().to_bytes(),
    );
    let older = Endpoint::builder(presets::Minimal)
        .secret_key(machine)
        .alpns(vec![b"ployz/rpc/1".to_vec()])
        .relay_mode(RelayMode::custom([relay_url.clone()]))
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .clear_ip_transports()
        .bind()
        .await
        .unwrap();
    let accepting = older.clone();
    tokio::spawn(async move {
        while let Some(incoming) = accepting.accept().await {
            drop(incoming.await);
        }
    });
    let connector = SystemConnector::default().with_management_relay(
        ManagementRelay::custom(relay_url, CaTlsConfig::insecure_skip_verify()).relay_only(),
    );
    match connector.connect(&connection(&capability)).await {
        Err(error @ ConnectError::OlderMachine) => assert!(
            error
                .to_string()
                .contains("ployz server upgrade stable SERVER --connect ssh://USER@HOST"),
            "the error names the SSH upgrade the docs give: {error}"
        ),
        Err(error) => panic!("expected OlderMachine, got: {error} ({error:?})"),
        Ok(_) => panic!("expected OlderMachine, got a channel"),
    }
    older.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_management_proof_admits_only_the_connection_it_was_made_on() {
    let (_map, relay_url, _relay) = run_relay_server().await.unwrap();
    let (_dir, owner, local) = participating().await;
    let capability = local
        .set_management_client(SetManagementClientRequest::Set { label: cloud() })
        .await
        .unwrap()
        .capability
        .unwrap();
    local
        .activate_management_client(
            *SecretKey::from_bytes(capability.client_secret())
                .public()
                .as_bytes(),
        )
        .await
        .unwrap();
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
    let machine = EndpointAddr::new(endpoint.id()).with_relay_url(relay_url.clone());
    let shutdown = CancellationToken::new();
    let server = tokio::spawn(management::serve(
        endpoint,
        local.clone(),
        MachineApi::builder(owner).build(),
        Arc::default(),
        None::<ReplicatedStore>,
        shutdown.clone(),
    ));
    let (holder, eavesdropper) = (
        raw_endpoint(&relay_url).await,
        raw_endpoint(&relay_url).await,
    );
    let original = holder
        .connect(machine.clone(), MANAGEMENT_ALPN)
        .await
        .unwrap();
    let proof = ployz::connect::management_proof(&original, &capability)
        .unwrap()
        .to_bytes();
    let replayed = eavesdropper
        .connect(machine, MANAGEMENT_ALPN)
        .await
        .unwrap();
    let (mut send, _recv) = replayed.open_bi().await.unwrap();
    send.write_all(&proof).await.unwrap();
    assert_refused_close(
        tokio::time::timeout(Duration::from_secs(10), replayed.closed())
            .await
            .expect("a replayed proof must be refused"),
    );
    let (mut send, _recv) = original.open_bi().await.unwrap();
    send.write_all(&proof).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(500), original.closed())
            .await
            .is_err(),
        "the proof admits the connection it was made on"
    );
    holder.close().await;
    eavesdropper.close().await;
    shutdown.cancel();
    server.await.unwrap();
}

/// Leave asks for the shutdown that drains its own connection, and its reply is the
/// caller's only proof of the reset. A reply finished after the drain's GOAWAY and PING
/// round trip ends the connection's last stream; it must still reach the caller.
#[tokio::test(flavor = "multi_thread")]
async fn a_reply_finished_as_shutdown_drains_reaches_the_caller() {
    let (_map, relay_url, _relay) = run_relay_server().await.unwrap();
    let (_dir, owner, local) = participating().await;
    let machine_id = local.record().id();
    let capability = local
        .set_management_client(SetManagementClientRequest::Set { label: cloud() })
        .await
        .unwrap()
        .capability
        .unwrap();
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
    let (connections, _observed) = mpsc::unbounded_channel();
    let gate = Arc::new(RequestGate::default());
    let server = tokio::spawn(management::serve(
        endpoint,
        local.clone(),
        ObservedApi {
            api: MachineApi::builder(owner).build(),
            connections,
            seen: Arc::default(),
            gate: Arc::clone(&gate),
        },
        Arc::default(),
        None::<ReplicatedStore>,
        shutdown.clone(),
    ));
    let channel = SystemConnector::new("/ployz-missing-ssh-client")
        .with_management_relay(ManagementRelay::custom(
            relay_url,
            CaTlsConfig::insecure_skip_verify(),
        ))
        .connect(&connection(&capability))
        .await
        .unwrap();
    activate(channel.clone()).await;
    gate.hold.store(true, Ordering::SeqCst);
    let call = tokio::spawn(describe(channel));
    wait_until(|| gate.active.load(Ordering::SeqCst) == 1).await;
    shutdown.cancel();
    // The in-process relay answers the drain's PING well within this; waiting longer
    // only makes the reply more surely the last thing written before the close.
    // real-wait: hyper's GOAWAY and PING round trip crosses real sockets and signals nothing.
    tokio::time::sleep(Duration::from_millis(200)).await;
    gate.release.add_permits(1);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), call)
            .await
            .expect("the reply must not wait out the connection's idle timeout")
            .unwrap()
            .expect("the drain must not drop the reply"),
        machine_id
    );
    server.await.unwrap();
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
        Arc::default(),
        None::<ReplicatedStore>,
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
    MachineRpcClient::new(previous.clone())
        .set_management_client(
            op::SetManagementClient::into_request(SetManagementClientRequest::Clear {
                label: cloud(),
            })
            .encode()
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(!local.record().has_management_clients());
    revoked(previous).await;
    // The candidate is in the tombstone too: its redial confirms the removal.
    assert!(matches!(
        connector.connect(&connection(&replacement)).await,
        Err(ConnectError::ClientCleared)
    ));
    drop(verified);
    shutdown.cancel();
    server.await.unwrap();
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
        Arc::default(),
        None::<ReplicatedStore>,
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

    let cloud_new = set(cloud()).await;
    revoked(cloud_channel).await;
    assert_eq!(describe(cli_channel.clone()).await.unwrap(), machine_id);
    let cloud_channel = connector.connect(&connection(&cloud_new)).await.unwrap();

    local
        .set_management_client(SetManagementClientRequest::Clear { label: cli })
        .await
        .unwrap();
    revoked(cli_channel).await;
    assert_eq!(describe(cloud_channel.clone()).await.unwrap(), machine_id);
    assert_eq!(
        local.record().management_clients().collect::<Vec<_>>(),
        [&cloud()]
    );
    assert!(matches!(
        connector.connect(&connection(&cli_capability)).await,
        Err(ConnectError::ClientCleared)
    ));

    // Once `cloud` is cleared too, its rotated-away key is still never confirmed as cleared.
    local
        .set_management_client(SetManagementClientRequest::Clear { label: cloud() })
        .await
        .unwrap();
    revoked(cloud_channel).await;
    assert!(matches!(
        connector.connect(&connection(&cloud_new)).await,
        Err(ConnectError::ClientCleared)
    ));
    assert!(matches!(
        connector.connect(&connection(&cloud_old)).await,
        Err(ConnectError::ClientRefused)
    ));

    let cloud_again = set(cloud()).await;
    let channel = connector.connect(&connection(&cloud_again)).await.unwrap();
    assert_eq!(describe(channel).await.unwrap(), machine_id);

    shutdown.cancel();
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_device_execs_or_tunnels() {
    let (_map, relay_url, _relay) = run_relay_server().await.unwrap();
    let (_dir, owner, local) = participating_in("127.0.0.0/16").await;
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
    let cloud_capability = set(cloud()).await;
    let cli_capability = set(ManagementClientLabel::parse("cli-device").unwrap()).await;
    let device = SecretKey::generate();
    local
        .set_device(SetDeviceRequest::Approve {
            key: DeviceKey::from_bytes(*device.public().as_bytes()),
        })
        .await
        .unwrap();
    let device_capability =
        ManagementCapability::new(*cloud_capability.machine(), device.to_bytes());
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
    let machine = EndpointAddr::new(endpoint.id()).with_relay_url(relay_url.clone());
    let shutdown = CancellationToken::new();
    let server = tokio::spawn(management::serve(
        endpoint,
        local.clone(),
        MachineApi::builder(owner).build(),
        Arc::default(),
        None::<ReplicatedStore>,
        shutdown.clone(),
    ));
    let connector = SystemConnector::default().with_management_relay(ManagementRelay::custom(
        relay_url.clone(),
        CaTlsConfig::insecure_skip_verify(),
    ));
    let cloud_channel = connector
        .connect(&connection(&cloud_capability))
        .await
        .unwrap();
    let cli_channel = connector
        .connect(&connection(&cli_capability))
        .await
        .unwrap();
    let device_channel = connector
        .connect(&connection(&device_capability))
        .await
        .unwrap();

    activate(cloud_channel.clone()).await;
    for channel in [cloud_channel, cli_channel] {
        let refused = exec(channel).await.unwrap_err();
        assert_eq!(refused.code(), tonic::Code::PermissionDenied, "{refused:?}");
        // A v0.3.4 CLI drops the details and prints only the message.
        assert_eq!(
            refused.message(),
            "ployz.container.exec.v1 needs Full access: run `ployz device request` \
             (update ployz first if it has no `device` command)",
        );
        assert_eq!(
            ployz_core::rpc::status_details(refused.details()),
            Some(serde_json::json!({
                "capability": "ployz.container.exec.v1",
                "next": "ployz device request",
            })),
        );
    }
    let status = exec(device_channel).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unavailable, "{status:?}");

    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = echo.local_addr().unwrap().to_string();
    for capability in [&cloud_capability, &cli_capability] {
        match connector
            .dial_proxy(&connection(capability), "tcp", &target)
            .await
        {
            Err(ConnectError::TunnelNeedsFull) => {}
            other => panic!("an Operator tunnel must be refused, got {:?}", other.err()),
        }
        let close = v0_3_4_tunnel(&relay_url, &machine, capability, &target).await;
        assert_eq!(
            close.error_code,
            VarInt::from_u32(TUNNEL_FAILED),
            "{close:?}"
        );
        assert_eq!(
            String::from_utf8_lossy(&close.reason),
            "tunnels need Full access: run `ployz device request` \
             (update ployz first if it has no `device` command)",
        );
    }
    connector
        .dial_proxy(&connection(&device_capability), "tcp", &target)
        .await
        .unwrap();

    shutdown.cancel();
    server.await.unwrap();
}

/// Open a tunnel the way a v0.3.4 CLI does. It prints a close reason only under
/// [`TUNNEL_FAILED`], and waits two seconds for that close.
async fn v0_3_4_tunnel(
    relay_url: &iroh::RelayUrl,
    machine: &EndpointAddr,
    capability: &ManagementCapability,
    target: &str,
) -> iroh::endpoint::ApplicationClose {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let client = raw_endpoint(relay_url).await;
    let connection = client.connect(machine.clone(), TUNNEL_ALPN).await.unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    let proof = ployz::connect::management_proof(&connection, capability).unwrap();
    send.write_all(&proof.to_bytes()).await.unwrap();
    send.write_u8(u8::try_from(target.len()).unwrap())
        .await
        .unwrap();
    send.write_all(target.as_bytes()).await.unwrap();
    assert_ne!(recv.read_u8().await.ok(), Some(TUNNEL_OPEN));
    let closed = tokio::time::timeout(Duration::from_secs(2), connection.closed())
        .await
        .expect("the refusal must close within a v0.3.4 CLI's grace");
    client.close().await;
    let iroh::endpoint::ConnectionError::ApplicationClosed(close) = closed else {
        panic!("expected an application close, got {closed:?}");
    };
    close
}

async fn exec(channel: Channel) -> Result<(), tonic::Status> {
    let config = ExecRequestFrame::Config(ExecConfig {
        container_id: ContainerId::parse("a".repeat(64)).unwrap(),
        options: ExecOptions {
            command: vec!["sh".into()],
            attach_stdin: false,
            attach_stdout: true,
            attach_stderr: true,
            tty: false,
            detach: false,
        },
    })
    .encode()
    .unwrap();
    let request = tonic::Request::new(tokio_stream::iter([config]));
    tokio::time::timeout(
        Duration::from_secs(10),
        MachineRpcClient::new(channel).exec(request),
    )
    .await
    .expect("exec must answer")
    .map(drop)
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
        Arc::default(),
        None::<ReplicatedStore>,
        shutdown.clone(),
    ));
    let connector = Arc::new(
        SystemConnector::new("/ployz-missing-ssh-client").with_management_relay(
            ManagementRelay::custom(relay_url.clone(), CaTlsConfig::insecure_skip_verify()),
        ),
    );

    // A key the Machine never accepted gets CLIENT_REFUSED, not an unreachable error.
    let stranger =
        ManagementCapability::new(*capability.machine(), SecretKey::generate().to_bytes());
    assert_refused(connector.connect(&connection(&stranger)).await);

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
        Err(ConnectError::ClientRefused)
    ));
    drop(retry);
    tokio::time::timeout(Duration::from_secs(10), retry_connection.closed())
        .await
        .unwrap();
    local
        .activate_management_client(
            *SecretKey::from_bytes(delayed_capability.client_secret())
                .public()
                .as_bytes(),
        )
        .await
        .unwrap();
    let delayed = raw_endpoint(&relay_url).await;
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
        "a peer may take its time to prove its key"
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
    // A proof that arrives after rotation is judged by the record it arrives under.
    let (mut send, _recv) = waiting.open_bi().await.unwrap();
    send.write_all(
        &ployz::connect::management_proof(&waiting, &delayed_capability)
            .unwrap()
            .to_bytes(),
    )
    .await
    .unwrap();
    assert_refused_close(
        tokio::time::timeout(Duration::from_secs(10), waiting.closed())
            .await
            .expect("rotation must refuse a peer that proves a retired key"),
    );
    delayed.close().await;
    drop(session);
    tokio::time::timeout(Duration::from_secs(10), removal_connection.closed())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), surviving_connection.closed())
        .await
        .unwrap();
    drop(channel);

    // Exercise Cloud's actual SDK path. A replaced key's refusal is not confirmation;
    // the SDK must preserve the daemon's identity refusal.
    let session = ployz::sdk::connect_connections(vec![connection(&capability)], connector.clone())
        .await
        .unwrap();
    let removal_connection = observed.recv().await.unwrap();
    let replaced =
        match ployz::sdk::connect_answered(vec![connection(&abandoned)], connector.clone()).await {
            Ok(_) => panic!("abandoned capability must be refused"),
            Err(error) => refused(error),
        };
    assert_eq!(replaced.code, RpcErrorCode::Unauthenticated);
    assert_eq!(replaced.details, serde_json::Value::Null);
    // Cloud provisions a device's own slot, and a Clear of it leaves Cloud's slot alone.
    let device_label = ManagementClientLabel::parse("cli-device").unwrap();
    let device = session
        .set_management_client(device_label.clone())
        .await
        .unwrap();
    let device_session =
        ployz::sdk::connect_connections(vec![connection(&device)], connector.clone())
            .await
            .unwrap();
    let device_connection = observed.recv().await.unwrap();
    device_session.inspect().await.unwrap();
    let slots = owner
        .record()
        .management_clients()
        .cloned()
        .collect::<Vec<_>>();
    for refused in [
        device_session.set_management_client(cloud()).await,
        device_session
            .set_management_client(ManagementClientLabel::parse("cli-other").unwrap())
            .await,
    ] {
        let error = refused.expect_err("a cli- slot must not set a slot");
        assert!(!error.unanswered, "{error}");
        assert!(
            error.to_string().contains("only Cloud manages slots"),
            "{error}"
        );
    }
    let error = device_session
        .clear_management_client(device_label.clone())
        .await
        .expect_err("a cli- slot must not clear a slot");
    assert!(
        error.to_string().contains("only Cloud manages slots"),
        "{error}"
    );
    // Forwarding drops the caller's key, so a request routed at another Machine is
    // refused at entry rather than served there as a local call, even from Cloud.
    let routed = || {
        let mut routed = tonic::Request::new(
            op::SetManagementClient::into_request(SetManagementClientRequest::Set {
                label: cloud(),
            })
            .encode()
            .unwrap(),
        );
        routed.metadata_mut().insert(
            ployz_core::ONE_TARGET_HEADER,
            tonic::metadata::MetadataValue::from_static("elsewhere"),
        );
        routed
    };
    let device_channel = connector.connect(&connection(&device)).await.unwrap();
    let device_channel_connection = observed.recv().await.unwrap();
    let status = MachineRpcClient::new(device_channel)
        .set_management_client(routed())
        .await
        .expect_err("a routed slot change from a cli- slot must be refused");
    assert_eq!(status.code(), tonic::Code::PermissionDenied, "{status}");
    let cloud_channel = connector.connect(&connection(&capability)).await.unwrap();
    observed.recv().await.unwrap();
    let status = MachineRpcClient::new(cloud_channel)
        .set_management_client(routed())
        .await
        .expect_err("a routed slot change from Cloud must be refused");
    assert_eq!(status.code(), tonic::Code::InvalidArgument, "{status}");
    assert!(
        status
            .message()
            .contains("only on the Server that holds them"),
        "{status}"
    );
    assert_eq!(
        owner
            .record()
            .management_clients()
            .cloned()
            .collect::<Vec<_>>(),
        slots
    );
    device_session.inspect().await.unwrap();
    session.clear_management_client(device_label).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), device_connection.closed())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), device_channel_connection.closed())
        .await
        .unwrap();
    drop(device_session);
    let error =
        match ployz::sdk::connect_answered(vec![connection(&device)], connector.clone()).await {
            Ok(_) => panic!("a cleared device capability must be refused"),
            Err(error) => refused(error),
        };
    assert_eq!(
        error.details,
        serde_json::json!({ "management_client": "cleared" })
    );
    session.inspect().await.unwrap();
    // Clearing its own slot, the caller receives the response before the Machine
    // revokes the connection the session still holds open.
    session.clear_management_client(cloud()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), removal_connection.closed())
        .await
        .unwrap();
    drop(session);
    // A lost Clear response is confirmed by the redial.
    let error = match ployz::sdk::connect_answered(vec![connection(&capability)], connector.clone())
        .await
    {
        Ok(_) => panic!("removed capability must not reconnect through the SDK"),
        Err(error) => refused(error),
    };
    assert_eq!(error.code, RpcErrorCode::Unauthenticated);
    assert_eq!(
        error.details,
        serde_json::json!({ "management_client": "cleared" })
    );
    assert!(matches!(
        connector.connect(&connection(&capability)).await,
        Err(ConnectError::ClientCleared)
    ));

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("serve must finish once every client is gone")
        .unwrap();
    assert!(endpoint.is_closed());
}

/// A Build Grant reaches image ingest for one push into its one repository, never
/// Machine RPC, and nothing once its Build ends.
#[tokio::test(flavor = "multi_thread")]
async fn a_port_forward_tunnel_reaches_only_the_cluster_network_and_ends_on_revocation() {
    tokio::time::timeout(Duration::from_secs(120), tunnel_contract())
        .await
        .expect("tunnel test timed out");
}

async fn tunnel_contract() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let (_map, relay_url, _relay) = run_relay_server().await.unwrap();
    // Loopback stands in for the Cluster network, so an echo server is a "container".
    let (_dir, owner, local) = participating_in("127.0.0.0/16").await;
    let device = SecretKey::generate();
    let key = DeviceKey::from_bytes(*device.public().as_bytes());
    local
        .set_device(SetDeviceRequest::Approve { key })
        .await
        .unwrap();
    let capability = ManagementCapability::new(
        local.record().management_secret().public_key(),
        device.to_bytes(),
    );
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
        Arc::default(),
        None::<ReplicatedStore>,
        shutdown.clone(),
    ));
    let connector = SystemConnector::default().with_management_relay(ManagementRelay::custom(
        relay_url,
        CaTlsConfig::insecure_skip_verify(),
    ));
    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = echo.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut read, mut write) = tcp.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
    let managed = connection(&capability);
    let dial = |address: String| {
        let connector = &connector;
        let managed = &managed;
        async move { connector.dial_proxy(managed, "tcp", &address).await }
    };

    let mut tunnel = dial(target.clone()).await.unwrap();
    let payload = [0_u8, 0xff, b'\n', 7];
    tunnel.write_all(&payload).await.unwrap();
    let mut echoed = [0_u8; 4];
    tunnel.read_exact(&mut echoed).await.unwrap();
    assert_eq!(echoed, payload);

    // Outside the Cluster network, or nothing listening: refused with the reason.
    for (address, reason) in [
        ("192.0.2.1:80".to_owned(), "outside the Cluster network"),
        ("localhost:80".to_owned(), "not an ip:port"),
        (unused_port(), "refused the connection"),
    ] {
        match dial(address.clone()).await {
            Err(ConnectError::Attempt(message)) => {
                assert!(message.contains(reason), "{address}: {message}");
            }
            other => panic!("{address}: expected a refusal, got {:?}", other.err()),
        }
    }

    local
        .set_device(SetDeviceRequest::Remove { code: key.code() })
        .await
        .unwrap();
    let mut rest = Vec::new();
    let ended = tokio::time::timeout(Duration::from_secs(10), tunnel.read_to_end(&mut rest))
        .await
        .expect("revocation must end an open tunnel");
    assert!(ended.is_err() || rest.is_empty(), "{ended:?}");
    assert!(matches!(
        dial(target).await,
        Err(ConnectError::ClientCleared)
    ));
    shutdown.cancel();
    server.await.unwrap();
}

fn unused_port() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_build_grant_pushes_one_image_into_ingest_and_nothing_else() {
    tokio::time::timeout(Duration::from_secs(120), build_grant_contract())
        .await
        .expect("build grant test timed out");
}

async fn build_grant_contract() {
    use ployz_core::BuildGrant;
    use sha2::{Digest as _, Sha256};

    let machine = GrantMachine::start().await;
    let capability = machine
        .local
        .set_management_client(SetManagementClientRequest::Set { label: cloud() })
        .await
        .unwrap()
        .capability
        .unwrap();
    let (id, grant) = machine.mint();
    let relay = &machine.relay;
    let grants = &machine.grants;
    let seen = &machine.seen;

    // The grant key is not a Management Capability: Machine RPC refuses it.
    let connector = SystemConnector::default().with_management_relay(relay.clone());
    let as_capability = ManagementCapability::new(*grant.machine(), *grant.secret());
    assert_refused(connector.connect(&connection(&as_capability)).await);
    // A Management Capability's key holds no grant, so it cannot push.
    let stranger = open_grant_registry(
        &BuildGrant::new(*capability.machine(), *capability.client_secret()),
        relay,
    )
    .await
    .unwrap();
    let http = reqwest::Client::new();
    assert!(
        http.get(format!("http://{}/v2/", stranger.address()))
            .send()
            .await
            .is_err()
    );

    let registry = open_grant_registry(&grant, relay).await.unwrap();
    let base = format!("http://{}/v2/ployz-build/web", registry.address());
    let blob = format!("{base}/blobs/sha256:{}", "a".repeat(64));
    assert_eq!(http.head(&blob).send().await.unwrap().status(), 200);
    let upload = http
        .post(format!("{base}/blobs/uploads/"))
        .send()
        .await
        .unwrap();
    assert_eq!(upload.status(), 202);
    // Upload locations lead back through the grant, not to the ingest address.
    assert_eq!(
        upload.headers()["location"],
        "/v2/ployz-build/web/blobs/uploads/u1?_state=s"
    );
    // Reading content back, or writing another repository, is not the grant's push.
    assert_eq!(http.get(&blob).send().await.unwrap().status(), 403);
    let other = format!(
        "http://{}/v2/ployz-build/api/blobs/uploads/",
        registry.address()
    );
    assert_eq!(http.post(other).send().await.unwrap().status(), 403);
    let manifest = br#"{"schemaVersion":2}"#.to_vec();
    let hex = hex::encode(Sha256::digest(&manifest));
    let put = |tag: String| {
        http.put(format!("{base}/manifests/{tag}"))
            .body(manifest.clone())
            .send()
    };
    // A tag must name the digest of the manifest it moves.
    let wrong = put(format!("ployz-sha256-{}", "b".repeat(64)))
        .await
        .unwrap();
    assert_eq!(wrong.status(), 400);
    assert_eq!(
        put(format!("ployz-sha256-{hex}")).await.unwrap().status(),
        201
    );
    // One push per grant.
    assert_eq!(
        put(format!("ployz-sha256-{hex}")).await.unwrap().status(),
        403
    );
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .all(|request| !request.starts_with("GET ")),
        "{seen:?}"
    );

    // Reading the grant reports the push without spending anything more, and the
    // spent grant refuses a redial.
    let inspected = grants.inspect(&id).unwrap();
    assert_eq!(
        inspected
            .pushed
            .as_ref()
            .map(ployz_core::ImageDigest::as_str),
        Some(format!("sha256:{hex}").as_str())
    );
    assert!(inspected.expires_in_seconds > 0);
    assert_eq!(http.head(&blob).send().await.unwrap().status(), 403);
    let again = open_grant_registry(&grant, relay).await.unwrap();
    assert!(
        http.get(format!("http://{}/v2/", again.address()))
            .send()
            .await
            .is_err()
    );
    assert!(again.refusal().is_some());
    assert_eq!(grants.inspect(&id).unwrap().pushed, inspected.pushed);
    machine.stop().await;
}

/// The runner makes the key and keeps its secret: the Machine admits that key alone.
#[tokio::test(flavor = "multi_thread")]
async fn a_holder_grant_admits_only_the_holder_key() {
    tokio::time::timeout(Duration::from_secs(120), holder_contract())
        .await
        .expect("build grant test timed out");
}

async fn holder_contract() {
    use ployz_core::{BuildGrant, BuildGrantId, MintedKey};

    let machine = GrantMachine::start().await;
    let holder = iroh::SecretKey::generate();
    let id = BuildGrantId::parse(hex::encode(holder.public().as_bytes())).unwrap();
    let minted = machine.mint_for(Some(id));
    assert_eq!(minted.id, id);
    let MintedKey::Holder { machine: dial } = minted.key else {
        panic!("a holder mint returns the Machine to dial, never a secret");
    };
    let http = reqwest::Client::new();
    let blob = format!("/v2/ployz-build/web/blobs/sha256:{}", "a".repeat(64));
    let status = async |secret: [u8; 32], method: reqwest::Method, path: &str| {
        let registry = open_grant_registry(&BuildGrant::new(dial, secret), &machine.relay)
            .await
            .unwrap();
        http.request(method, format!("http://{}{path}", registry.address()))
            .send()
            .await
            .map(|response| response.status())
            .ok()
    };
    let stranger = iroh::SecretKey::generate().to_bytes();
    assert_eq!(status(stranger, reqwest::Method::HEAD, &blob).await, None);
    assert_eq!(
        status(stranger, reqwest::Method::GET, "/ployz/sealed").await,
        None
    );
    assert_eq!(
        status(holder.to_bytes(), reqwest::Method::HEAD, &blob).await,
        Some(reqwest::StatusCode::OK)
    );
    // This grant carries no Sealed values.
    assert_eq!(
        status(holder.to_bytes(), reqwest::Method::GET, "/ployz/sealed").await,
        Some(reqwest::StatusCode::NOT_FOUND)
    );
    machine.stop().await;
}

/// A Machine that already holds the image answers the pusher's manifest HEAD, and the
/// pusher never PUTs it; that HEAD of the grant's own tag is the grant's push.
#[tokio::test(flavor = "multi_thread")]
async fn a_build_grant_records_an_image_the_machine_already_holds() {
    tokio::time::timeout(Duration::from_secs(120), already_held_contract())
        .await
        .expect("build grant test timed out");
}

async fn already_held_contract() {
    let machine = GrantMachine::start().await;
    let http = reqwest::Client::new();
    let head = async |reference: String| {
        let (id, grant) = machine.mint();
        let registry = open_grant_registry(&grant, &machine.relay).await.unwrap();
        let manifests = format!("http://{}/v2/ployz-build/web/manifests", registry.address());
        let status = http
            .head(format!("{manifests}/{reference}"))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, 200, "{reference}");
        (id, manifests, registry)
    };

    // Another tag, the held digest by digest, or a tag whose digest the ingest does
    // not confirm is only a question, not a push.
    for reference in [
        "latest".to_owned(),
        format!("sha256:{HELD}"),
        format!("ployz-sha256-{}", "b".repeat(64)),
    ] {
        let (id, _, _registry) = head(reference.clone()).await;
        let inspected = machine.grants.inspect(&id).unwrap();
        assert_eq!(inspected.pushed, None, "{reference}");
    }

    let (id, manifests, _registry) = head(format!("ployz-sha256-{HELD}")).await;
    // One push per grant: the held image spent it.
    let put = http
        .put(format!("{manifests}/ployz-sha256-{HELD}"))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 403);
    let inspected = machine.grants.inspect(&id).unwrap();
    assert_eq!(
        inspected
            .pushed
            .as_ref()
            .map(ployz_core::ImageDigest::as_str),
        Some(format!("sha256:{HELD}").as_str())
    );
    machine.stop().await;
}

/// A participating Machine serving management, and so the Build Grant ALPN, over an
/// in-process relay, with a [`fake_ingest`] behind its grants.
struct GrantMachine {
    local: LocalMachine,
    grants: Arc<management::BuildGrants>,
    relay: ManagementRelay,
    ingest: std::net::SocketAddr,
    seen: Arc<Mutex<Vec<String>>>,
    shutdown: CancellationToken,
    server: tokio::task::JoinHandle<()>,
    /// The relay server and the Machine's state directory, alive while it serves.
    _alive: Box<dyn std::any::Any>,
}

impl GrantMachine {
    async fn start() -> Self {
        let (map, relay_url, relay_server) = run_relay_server().await.unwrap();
        let (dir, owner, local) = participating().await;
        let (ingest, seen) = fake_ingest().await;
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
        let grants = Arc::new(management::BuildGrants::default());
        let shutdown = CancellationToken::new();
        let server = tokio::spawn(management::serve(
            endpoint,
            local.clone(),
            MachineApi::builder(owner).build(),
            Arc::clone(&grants),
            None::<ReplicatedStore>,
            shutdown.clone(),
        ));
        Self {
            local,
            grants,
            relay: ManagementRelay::custom(relay_url, CaTlsConfig::insecure_skip_verify()),
            ingest,
            seen,
            shutdown,
            server,
            _alive: Box::new((map, relay_server, dir)),
        }
    }

    /// A grant for one push into `ployz-build/web`, held by `holder` when given.
    fn mint_for(&self, holder: Option<ployz_core::BuildGrantId>) -> ployz_core::BuildGrantMinted {
        let request = ployz_core::MintBuildGrantRequest {
            repository: ployz_core::BuildGrantRepository::parse("ployz-build/web").unwrap(),
            holder,
            sealed: None,
        };
        self.grants
            .mint(
                self.local.record().management_secret().public_key(),
                ployzd::management::Mint::check(request, None).unwrap(),
                self.ingest,
            )
            .unwrap()
    }

    /// A grant for one push into `ployz-build/web`.
    fn mint(&self) -> (ployz_core::BuildGrantId, ployz_core::BuildGrant) {
        let minted = self.mint_for(None);
        let ployz_core::MintedKey::Grant { grant } = minted.key else {
            panic!("a holderless mint returns its grant");
        };
        (minted.id, grant)
    }

    async fn stop(self) {
        self.shutdown.cancel();
        self.server.await.unwrap();
    }
}

/// The one image [`fake_ingest`] holds: a manifest HEAD answers with its digest.
const HELD: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

/// An OCI registry stand-in that records each request line it answers.
async fn fake_ingest() -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let recorded = Arc::clone(&seen);
    let app = axum::Router::new().fallback(move |request: axum::extract::Request| {
        let recorded = Arc::clone(&recorded);
        async move {
            let line = format!("{} {}", request.method(), request.uri());
            recorded.lock().unwrap().push(line);
            let mut response = axum::response::Response::builder();
            response = match *request.method() {
                http::Method::POST => response.status(202).header(
                    "location",
                    format!("http://{address}/v2/ployz-build/web/blobs/uploads/u1?_state=s"),
                ),
                http::Method::PUT => response.status(201),
                http::Method::HEAD if request.uri().path().contains("/manifests/") => response
                    .status(200)
                    .header("docker-content-digest", format!("sha256:{HELD}")),
                _ => response.status(200),
            };
            response.body(axum::body::Body::empty()).unwrap()
        }
    });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (address, seen)
}

/// A forgotten Server asks a member with its own Management Identity as the transport
/// key and reads the answer from the close code; nothing else about it is trusted.
#[tokio::test(flavor = "multi_thread")]
async fn forgotten_question_is_answered_by_the_asking_key_alone() {
    let (_map, relay_url, _relay) = run_relay_server().await.unwrap();
    let (_dir, owner, local) = participating().await;
    let member = local.record().management_secret().public_key();
    let forgotten = SecretKey::generate();
    let listed = SecretKey::generate();
    let roster = FixedRoster(vec![
        (identity(&forgotten), ForgottenAnswer::Forgotten),
        (identity(&listed), ForgottenAnswer::Listed),
    ]);
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
        Arc::default(),
        roster,
        shutdown.clone(),
    ));

    let ask = |key: SecretKey| {
        let relay_url = relay_url.clone();
        async move {
            let asker = keyed_endpoint(&relay_url, key).await;
            let answer = tokio::time::timeout(
                Duration::from_secs(30),
                management::forgotten::ask(&asker, member, &relay_url),
            )
            .await
            .expect("the member answers promptly");
            asker.close().await;
            answer
        }
    };
    assert_eq!(ask(forgotten).await, Some(ForgottenAnswer::Forgotten));
    assert_eq!(ask(listed).await, Some(ForgottenAnswer::Listed));
    assert_eq!(
        ask(SecretKey::generate()).await,
        Some(ForgottenAnswer::NoAnswer)
    );

    shutdown.cancel();
    server.await.unwrap();
}

/// A member without a roster, and a Machine running a daemon that predates the
/// forgotten ALPN, give the asker nothing to act on.
#[tokio::test(flavor = "multi_thread")]
async fn forgotten_question_without_a_roster_or_an_older_member_gives_nothing() {
    let (_map, relay_url, _relay) = run_relay_server().await.unwrap();
    let (_dir, owner, local) = participating().await;
    let member = local.record().management_secret().public_key();
    let asker_key = SecretKey::generate();
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
        Arc::default(),
        None::<ReplicatedStore>,
        shutdown.clone(),
    ));
    let asker = keyed_endpoint(&relay_url, asker_key.clone()).await;
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(30),
            management::forgotten::ask(&asker, member, &relay_url)
        )
        .await
        .unwrap(),
        Some(ForgottenAnswer::NoAnswer)
    );
    shutdown.cancel();
    server.await.unwrap();

    let older_key = SecretKey::generate();
    let older = identity(&older_key);
    let older_endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(older_key)
        .alpns(vec![MANAGEMENT_ALPN.to_vec()])
        .relay_mode(RelayMode::custom([relay_url.clone()]))
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .clear_ip_transports()
        .bind()
        .await
        .unwrap();
    let accepting = tokio::spawn({
        let older_endpoint = older_endpoint.clone();
        async move { while older_endpoint.accept().await.is_some() {} }
    });
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(30),
            management::forgotten::ask(&asker, older, &relay_url)
        )
        .await
        .unwrap(),
        None
    );
    older_endpoint.close().await;
    accepting.abort();
    asker.close().await;
}

/// Only a Participating Machine's view of the Cluster counts: one that never enrolled,
/// or is on its way out, gives the asker nothing even when its roster would.
#[tokio::test(flavor = "multi_thread")]
async fn forgotten_question_to_a_machine_outside_a_cluster_gives_nothing() {
    let (_map, relay_url, _relay) = run_relay_server().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let owner = RecordOwner::spawn(LocalMachineStore::open(dir.path()).unwrap()).unwrap();
    let local = LocalMachine::new(owner.clone());
    let member = local.record().management_secret().public_key();
    let asker_key = SecretKey::generate();
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
        Arc::default(),
        FixedRoster(vec![(identity(&asker_key), ForgottenAnswer::Forgotten)]),
        shutdown.clone(),
    ));
    let asker = keyed_endpoint(&relay_url, asker_key).await;
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(30),
            management::forgotten::ask(&asker, member, &relay_url)
        )
        .await
        .unwrap(),
        Some(ForgottenAnswer::NoAnswer)
    );
    asker.close().await;
    shutdown.cancel();
    server.await.unwrap();
}

/// The answer is bound to the member's key: an endpoint serving under another key, whether
/// reached through the relay or at its own address, never yields one for the key asked
/// about, so a stale or forged identity in a Machine row cannot speak for a member.
#[tokio::test(flavor = "multi_thread")]
async fn forgotten_question_for_one_key_is_never_answered_by_another() {
    let (_map, relay_url, _relay) = run_relay_server().await.unwrap();
    let (_dir, owner, local) = participating().await;
    let asker_key = SecretKey::generate();
    let asked_about = identity(&SecretKey::generate());
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
    let served_at = endpoint
        .bound_sockets()
        .into_iter()
        .find(|socket| socket.is_ipv4())
        .map(|socket| std::net::SocketAddr::from(([127, 0, 0, 1], socket.port())))
        .expect("the management endpoint binds IPv4");
    let shutdown = CancellationToken::new();
    let server = tokio::spawn(management::serve(
        endpoint,
        local.clone(),
        MachineApi::builder(owner).build(),
        Arc::default(),
        FixedRoster(vec![(identity(&asker_key), ForgottenAnswer::Forgotten)]),
        shutdown.clone(),
    ));

    let asker = keyed_endpoint(&relay_url, asker_key.clone()).await;
    let through_relay = tokio::time::timeout(
        Duration::from_secs(15),
        management::forgotten::ask(&asker, asked_about, &relay_url),
    )
    .await;
    assert!(
        matches!(through_relay, Ok(None) | Err(_)),
        "another key answered through the relay: {through_relay:?}"
    );
    asker.close().await;

    let direct_asker = Endpoint::builder(presets::Minimal)
        .secret_key(asker_key)
        .relay_mode(RelayMode::custom([relay_url.clone()]))
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .bind()
        .await
        .unwrap();
    let address = EndpointAddr::new(iroh::PublicKey::from_bytes(asked_about.as_bytes()).unwrap())
        .with_ip_addr(served_at);
    let direct = tokio::time::timeout(
        Duration::from_secs(15),
        direct_asker.connect(address, ployz_core::FORGOTTEN_ALPN),
    )
    .await;
    if let Ok(Ok(connection)) = direct {
        let close = connection.closed().await;
        let answered = match &close {
            iroh::endpoint::ConnectionError::ApplicationClosed(close) => {
                ForgottenAnswer::from_close_code(
                    u32::try_from(close.error_code.into_inner()).unwrap(),
                )
            }
            iroh::endpoint::ConnectionError::VersionMismatch
            | iroh::endpoint::ConnectionError::TransportError(_)
            | iroh::endpoint::ConnectionError::ConnectionClosed(_)
            | iroh::endpoint::ConnectionError::Reset
            | iroh::endpoint::ConnectionError::TimedOut
            | iroh::endpoint::ConnectionError::LocallyClosed
            | iroh::endpoint::ConnectionError::CidsExhausted => None,
        };
        assert_eq!(
            answered, None,
            "another key answered at its address: {close:?}"
        );
    }
    direct_asker.close().await;
    shutdown.cancel();
    server.await.unwrap();
}

struct FixedRoster(Vec<(ManagementIdentity, ForgottenAnswer)>);

impl management::Roster for FixedRoster {
    async fn forgotten(
        &self,
        identity: ManagementIdentity,
    ) -> Result<ForgottenAnswer, ployzd::corrosion::Error> {
        Ok(self
            .0
            .iter()
            .find(|(known, _)| *known == identity)
            .map_or(ForgottenAnswer::NoAnswer, |(_, answer)| *answer))
    }
}

fn identity(key: &SecretKey) -> ManagementIdentity {
    ManagementIdentity::from_bytes(*key.public().as_bytes())
}

/// An endpoint under `key`, dialing only through the relay.
async fn keyed_endpoint(relay_url: &iroh::RelayUrl, key: SecretKey) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(RelayMode::custom([relay_url.clone()]))
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .clear_ip_transports()
        .bind()
        .await
        .unwrap()
}

/// A management endpoint under a fresh key, dialing only through the relay.
async fn raw_endpoint(relay_url: &iroh::RelayUrl) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::custom([relay_url.clone()]))
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .clear_ip_transports()
        .bind()
        .await
        .unwrap()
}

fn assert_refused_close(reason: iroh::endpoint::ConnectionError) {
    let iroh::endpoint::ConnectionError::ApplicationClosed(close) = reason else {
        panic!("expected an application close, got {reason:?}");
    };
    assert!(
        [management::CLIENT_REFUSED, management::CLIENT_CLEARED].contains(&close.error_code),
        "expected a refusal, got {close:?}"
    );
}

fn assert_refused(result: Result<Channel, ConnectError>) {
    match result {
        Err(ConnectError::ClientRefused) => {}
        Err(error) => panic!("expected CLIENT_REFUSED, got: {error} ({error:?})"),
        Ok(_) => panic!("expected CLIENT_REFUSED, got a channel"),
    }
}

async fn participating() -> (tempfile::TempDir, RecordOwner, LocalMachine) {
    participating_in("10.210.0.0/16").await
}

async fn participating_in(network: &str) -> (tempfile::TempDir, RecordOwner, LocalMachine) {
    let dir = tempfile::tempdir().unwrap();
    let owner = RecordOwner::spawn(LocalMachineStore::open(dir.path()).unwrap()).unwrap();
    let local = LocalMachine::new(owner.clone());
    local
        .enroll(EnrollRequest {
            role: EnrollAs::Founder {
                initial_policy: Default::default(),
                name: MachineName::parse("first").unwrap(),
                cluster_network: network.parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .await
        .unwrap();
    (dir, owner, local)
}

fn cloud() -> ManagementClientLabel {
    ManagementClientLabel::parse("cloud").unwrap()
}

async fn revoked(channel: Channel) {
    eventually("a revoked key's connection must stop serving", async || {
        describe(channel.clone()).await.is_err()
    })
    .await;
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
        .inspect(
            op::Inspect::into_request(ployz_core::InspectRequest::default())
                .encode()
                .unwrap(),
        )
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap()
        .decode::<op::Inspect>()
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
    eventually("observable connection state did not settle", async || {
        predicate()
    })
    .await;
}

async fn eventually(stuck: &str, mut done: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !done().await {
            // real-wait: the daemon serves over real sockets on a multi-thread runtime; no in-process event exposes this state.
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect(stuck);
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

/// The Machine's own refusal, which the SDK must not mark unanswered.
fn refused(failure: ployz::sdk::CallError) -> ployz_core::RpcError {
    assert!(!failure.unanswered, "{failure}");
    failure.error
}
