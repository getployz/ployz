//! Founder completion loss, resumption, and convergent-tail regressions.

use super::*;

#[tokio::test]
async fn lost_completion_response_reruns_idempotently_when_cloud_is_ready() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let registration = Registered {
        assigned_machine: founder,
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    };
    let enroll = EnrollListen::script([
        json!({
            "kind": "initialize",
            "resumed": false,
            "storage": "none",
            "pairing": pairing,
        }),
        json!({
            "kind": "join",
            "storage": "none",
            "pairing": pairing,
            "registration": registration,
        }),
    ])
    .await;
    enroll.set_callback_status(500);
    let daemon = JoinDaemon::new(registration.clone());
    let machine_addr = serve_machine(daemon.clone()).await;

    let output = init_cloud(
        &format!("tcp://{machine_addr}"),
        &enroll.url,
        "founder",
        false,
        true,
    )
    .await;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("rerun the same ployz cloud enroll command"),
        "{stderr}"
    );
    assert_eq!(
        enroll.callbacks(),
        vec![json!({ "machineId": machine_id.as_str(), "pairingCredential": PAIRING }); 3]
    );
    wait_for_held(&relay.url, PAIRING, machine_id).await;

    let output = init_cloud(
        &format!("tcp://{machine_addr}"),
        &enroll.url,
        "founder",
        false,
        true,
    )
    .await;
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(daemon.initialize_requests().len(), 1);
    assert_eq!(daemon.reset_count(), 0);
    assert_eq!(enroll.callbacks().len(), 3);
}

#[tokio::test]
async fn new_founding_claim_with_reset_resets_then_initializes() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "initialize",
        "resumed": false,
        "storage": "none",
        "pairing": pairing,
    }))
    .await;
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder.clone(),
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    });
    let machine_addr = serve_machine(daemon.clone()).await;
    connect_daemon(machine_addr)
        .await
        .call::<op::Initialize>(
            InitializeRequest {
                name: founder.name,
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                ingress_proxy_backend: IngressProxyBackend::Caddy,
                public_ip: None,
                advertised_endpoints: founder.advertised_endpoints,
                wireguard_mtu: None,
                cloud_pairing: None,
            },
            None,
        )
        .await
        .unwrap();

    let output = init_cloud(
        &format!("tcp://{machine_addr}"),
        &enroll.url,
        "founder",
        true,
        true,
    )
    .await;

    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(daemon.reset_count(), 1);
    assert_eq!(daemon.initialize_requests().len(), 2);
    assert_eq!(
        enroll.callbacks(),
        [json!({
            "machineId": machine_id.as_str(),
            "pairingCredential": PAIRING,
        })]
    );
    wait_for_held(&relay.url, PAIRING, machine_id).await;
}

#[tokio::test]
async fn resumed_founder_uses_the_matching_participating_machine() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "initialize",
        "resumed": true,
        "storage": "none",
        "pairing": pairing,
    }))
    .await;
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder.clone(),
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    });
    let machine_addr = serve_machine(daemon.clone()).await;
    connect_daemon(machine_addr)
        .await
        .call::<op::Initialize>(
            InitializeRequest {
                name: founder.name,
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                ingress_proxy_backend: IngressProxyBackend::Caddy,
                public_ip: None,
                advertised_endpoints: founder.advertised_endpoints,
                wireguard_mtu: None,
                cloud_pairing: None,
            },
            None,
        )
        .await
        .unwrap();

    let output = init_cloud(
        &format!("tcp://{machine_addr}"),
        &enroll.url,
        "founder",
        false,
        true,
    )
    .await;

    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(daemon.initialize_requests().len(), 1);
    assert_eq!(
        enroll.callbacks(),
        [json!({
            "machineId": machine_id.as_str(),
            "pairingCredential": PAIRING,
        })]
    );
}

#[tokio::test]
async fn resumed_founder_converges_before_pairing_and_final_completion() {
    let mut founder = founder_machine();
    founder.public_ip = Some("192.0.2.1".parse().unwrap());
    let machine_id = founder.id;
    let requested = IngressProxyBackend::Zentinel
        .requested_service_spec(ployz::ingress::ZENTINEL_IMAGE.to_owned(), Vec::new(), None)
        .unwrap();
    let ingress = container_on(
        &founder,
        requested.to_resolved(
            ployz_core::ServiceId::parse("c".repeat(32)).unwrap(),
            ployz_core::ResolvedUpdateConfig {
                order: ployz_core::UpdateOrder::StopFirst,
                monitor_millis: None,
            },
        ),
        ployz_core::ProjectName::system(),
        'c',
    );
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder.clone(),
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    })
    .with_containers(vec![ingress])
    .with_reserved_domain();
    let machine_addr = serve_machine(daemon.clone()).await;
    connect_daemon(machine_addr)
        .await
        .call::<op::Initialize>(
            InitializeRequest {
                name: founder.name,
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                ingress_proxy_backend: IngressProxyBackend::Zentinel,
                public_ip: founder.public_ip,
                advertised_endpoints: founder.advertised_endpoints,
                wireguard_mtu: None,
                cloud_pairing: None,
            },
            None,
        )
        .await
        .unwrap();
    let events = EventLog::default();
    let daemon = daemon.with_events(events.clone());
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::script_recording(
        [json!({
            "kind": "initialize",
            "resumed": true,
            "storage": "none",
            "pairing": pairing,
        })],
        events.clone(),
    )
    .await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{machine_addr}"),
            "cloud",
            "enroll",
            TOKEN,
            "--cloud-url",
            &enroll.url,
            "--name",
            "founder",
            "--no-dns",
            "--yes",
        ])
        .output()
        .await
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(daemon.initialize_requests().len(), 1);
    assert!(daemon.reserve_request().is_none());
    assert_eq!(events.entries(), ["set_cloud_pairing", "callback"]);
    wait_for_held(&relay.url, PAIRING, machine_id).await;
}

#[tokio::test]
async fn founder_tail_retries_transport_and_converges_in_order() {
    let mut founder = founder_machine();
    founder.public_ip = Some("127.0.0.1".parse().unwrap());
    let machine_id = founder.id;
    let events = EventLog::default();
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::script_recording(
        [json!({
            "kind": "initialize",
            "resumed": false,
            "storage": "none",
            "pairing": pairing,
        })],
        events.clone(),
    )
    .await;
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder,
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    })
    .with_events(events.clone())
    .transient_founder_tail_failures(1);
    let machine_addr = serve_machine(daemon.clone()).await;
    let (probe, probe_port) = serve_ingress_probe(machine_id).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{machine_addr}"),
            "cloud",
            "enroll",
            TOKEN,
            "--cloud-url",
            &enroll.url,
            "--name",
            "founder",
            "--ingress-backend",
            "zentinel",
            "--yes",
        ])
        .env("PLOYZ_INGRESS_VERIFY_PORT", probe_port.to_string())
        .output()
        .await
        .unwrap();
    probe.abort();

    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(daemon.founder_tail_attempts(), [2, 2, 2, 2]);
    let containers = daemon.containers();
    assert_eq!(containers.len(), 1);
    assert_eq!(containers.first().unwrap().service_name.as_str(), "ingress");
    assert_eq!(
        serde_json::to_value(daemon.domain_record_requests()).unwrap(),
        json!([{
            "records": [{ "name": "*", "type": "A", "values": ["127.0.0.1"] }]
        }])
    );
    assert_eq!(
        events.entries(),
        [
            "initialize",
            "reserve_domain",
            "deploy_ingress",
            "publish_dns",
            "set_cloud_pairing",
            "callback",
        ]
    );
    assert_eq!(daemon.reset_count(), 0);
    assert_eq!(enroll.posts().len(), 1);
    assert_eq!(enroll.callbacks().len(), 1);
    wait_for_held(&relay.url, PAIRING, machine_id).await;
}
