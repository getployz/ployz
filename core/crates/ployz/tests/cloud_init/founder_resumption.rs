//! Founder completion loss, resumption, and convergent-tail regressions.

use super::*;

#[tokio::test]
async fn lost_completion_response_reruns_idempotently_when_cloud_is_ready() {
    let mut founder = founder_machine();
    let machine_id = founder.id;
    founder.accepts_ingress = false;
    let pairing = CloudPairing::new(PairingCredential::parse(PAIRING).unwrap());
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
            "kind": "initialize",
            "resumed": true,
            "storage": "none",
            "pairing": pairing,
        }),
    ])
    .await;
    enroll.set_callback_status(500);
    let daemon = JoinDaemon::new(registration.clone());
    let machine_addr = serve_machine(daemon.clone()).await;

    let output = init_cloud(
        &format!("ssh://root@{machine_addr}"),
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
        vec![json!({ "machineId": machine_id.as_str(), "pairingCredential": PAIRING }); 1]
    );

    enroll.set_callback_status(200);
    let output = init_cloud(
        &format!("ssh://root@{machine_addr}"),
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
    assert_eq!(enroll.callbacks().len(), 2);
    assert_eq!(enroll.publications().len(), 2);
}

#[tokio::test]
async fn new_founding_claim_with_reset_resets_then_initializes() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let pairing = CloudPairing::new(PairingCredential::parse(PAIRING).unwrap());
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
    })
    .lose_lifecycle_reply();
    let machine_addr = serve_machine(daemon.clone()).await;
    connect_daemon(machine_addr)
        .await
        .call::<op::Initialize>(
            InitializeRequest {
                initial_policy: Default::default(),
                name: founder.name,
                cluster_network: "10.210.0.0/16".parse().unwrap(),
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
        &format!("ssh://root@{machine_addr}"),
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
}

#[tokio::test]
async fn resumed_founder_uses_the_matching_participating_machine() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let pairing = CloudPairing::new(PairingCredential::parse(PAIRING).unwrap());
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
                initial_policy: ployz_core::InitialMachinePolicy {
                    accepts_ingress: false,
                    ..Default::default()
                },
                name: founder.name,
                cluster_network: "10.210.0.0/16".parse().unwrap(),
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
        &format!("ssh://root@{machine_addr}"),
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
    let requested = ployz_core::caddy_service_spec("caddy:2.10.0".into(), Default::default(), None);
    let ingress = container_on(
        &founder,
        requested
            .to_resolved(
                ployz_core::ServiceId::parse("c".repeat(32)).unwrap(),
                ployz_core::ResolvedUpdateConfig {
                    order: ployz_core::UpdateOrder::StopFirst,
                    monitor_millis: None,
                },
            )
            .expect("volume graph is scoped"),
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
                initial_policy: Default::default(),
                name: founder.name,
                cluster_network: "10.210.0.0/16".parse().unwrap(),
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
    let pairing = CloudPairing::new(PairingCredential::parse(PAIRING).unwrap());
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

    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let output = super::harness::cli()
        .args([
            "--connect",
            &format!("ssh://root@{machine_addr}"),
            "cloud",
            "enroll",
            TOKEN,
            "--cloud-url",
            &enroll.url,
            "--name",
            "founder",
            "--ingress-image",
            "caddy:2.10.0",
            "--no-dns",
            "--yes",
        ])
        .env("HTTPS_PROXY", &proxy)
        .env("https_proxy", &proxy)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
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
    assert_eq!(
        events.entries(),
        ["set_cloud_pairing", "publish", "callback"]
    );
}

#[tokio::test]
async fn founder_tail_recovers_lost_replies_without_replaying_mutations() {
    let mut founder = founder_machine();
    let machine_id = founder.id;
    founder.public_ip = Some("127.0.0.1".parse().unwrap());
    let events = EventLog::default();
    let pairing = CloudPairing::new(PairingCredential::parse(PAIRING).unwrap());
    let enroll = EnrollListen::script_recording(
        [
            json!({
                "kind": "initialize", "resumed": false, "storage": "none", "pairing": pairing,
            }),
            json!({
                "kind": "initialize", "resumed": true, "storage": "none", "pairing": pairing,
            }),
        ],
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

    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let command = || {
        let mut command = super::harness::cli();
        command
            .args([
                "--connect",
                &format!("ssh://root@{machine_addr}"),
                "cloud",
                "enroll",
                TOKEN,
                "--cloud-url",
                &enroll.url,
                "--name",
                "founder",
                "--ingress-image",
                "caddy:2.10.0",
                "--yes",
            ])
            // Health-monitor timing is covered under virtual time in deploy tests.
            .env("PLOYZ_HEALTH_MONITOR_PERIOD", "0s")
            .env("PLOYZ_INGRESS_VERIFY_PORT", probe_port.to_string())
            .env("HTTPS_PROXY", &proxy)
            .env("https_proxy", &proxy)
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost");
        command
    };
    let first = command().arg("--reset").output().await.unwrap();
    assert!(!first.status.success());
    assert!(String::from_utf8_lossy(&first.stderr).contains(
        "rerun the same ployz cloud enroll command without --reset (keep all other options)"
    ));
    assert!(
        String::from_utf8_lossy(&first.stderr).contains("lost Ingress container creation reply")
    );
    assert_eq!(daemon.founder_tail_attempts(), [1, 1, 0, 0]);
    assert_eq!(
        daemon.initialize_requests().len(),
        1,
        "lost Initialize reply must be recovered by Inspect"
    );
    assert_eq!(daemon.reset_count(), 0);
    assert_eq!(
        daemon.containers().len(),
        1,
        "lost create reply must not cause an automatic second Create"
    );

    let output = command().output().await.unwrap();
    probe.abort();

    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(daemon.reset_count(), 0, "resume must omit --reset");
    assert_eq!(daemon.founder_tail_attempts(), [1, 2, 2, 2]);
    let containers = daemon.containers();
    assert_eq!(containers.len(), 1);
    assert_eq!(
        containers.first().unwrap().service_name().as_str(),
        "ingress"
    );
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
            "publish",
            "callback",
        ]
    );
    assert_eq!(daemon.reset_count(), 0);
    assert_eq!(enroll.posts().len(), 2);
    assert_eq!(enroll.callbacks().len(), 1);
}

#[tokio::test]
async fn founder_recovery_rejects_replaced_identity_and_guides_failed_reservation() {
    for replaced in [true, false] {
        let pairing = CloudPairing::new(PairingCredential::parse(PAIRING).unwrap());
        let enroll = EnrollListen::start(json!({
            "kind": "initialize", "resumed": false, "storage": "none", "pairing": pairing,
        }))
        .await;
        let daemon = JoinDaemon::new(Registered {
            assigned_machine: founder_machine(),
            visible_peers: Vec::new(),
            target_versions: Default::default(),
        });
        let daemon = if replaced {
            daemon.replace_identity_on_initialize()
        } else {
            daemon.fail_reservation()
        };
        let address = serve_machine(daemon.clone()).await;
        let output = super::harness::cli()
            .args([
                "--connect",
                &format!("ssh://root@{address}"),
                "cloud",
                "enroll",
                TOKEN,
                "--cloud-url",
                &enroll.url,
                "--name",
                "founder",
                "--accepts-ingress=false",
                "--reset",
                "--yes",
            ])
            .output()
            .await
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        if replaced {
            assert!(error.contains("different Machine identity"), "{error}");
        } else {
            assert!(error.contains("DNS reservation pending"), "{error}");
            assert!(
                error.contains("without --reset (keep all other options)"),
                "{error}"
            );
        }
        assert_eq!(daemon.initialize_requests().len(), 1);
        assert_eq!(daemon.reset_count(), 0);
        assert!(enroll.callbacks().is_empty());
    }
}

#[tokio::test]
async fn publication_failure_does_not_complete_and_resumes_the_same_founder() {
    let mut founder = founder_machine();
    founder.accepts_ingress = false;
    let pairing = CloudPairing::new(PairingCredential::parse(PAIRING).unwrap());
    let enroll = EnrollListen::script([
        json!({ "kind": "initialize", "resumed": false, "pairing": pairing }),
        json!({ "kind": "initialize", "resumed": true, "pairing": pairing }),
        json!({ "kind": "initialize", "resumed": true, "pairing": pairing }),
    ])
    .await;
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder,
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    });
    let address = serve_machine(daemon.clone()).await;
    let connect = format!("ssh://root@{address}");
    let unsupported = init_cloud(
        &format!("tcp://{address}"),
        &enroll.url,
        "founder",
        false,
        true,
    )
    .await;
    assert!(!unsupported.status.success());
    assert!(
        String::from_utf8_lossy(&unsupported.stderr)
            .contains("requires local Unix, SSH, or Tailcat")
    );
    assert!(daemon.initialize_requests().is_empty());
    assert!(enroll.publications().is_empty());
    enroll.set_publication_status(409);
    let output = init_cloud(&connect, &enroll.url, "founder", false, true).await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("candidate publication"));
    assert!(enroll.callbacks().is_empty());
    enroll.set_publication_status(200);
    let output = init_cloud(&connect, &enroll.url, "founder", false, true).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(daemon.initialize_requests().len(), 1);
    assert_eq!(daemon.reset_count(), 0);
    assert_eq!(enroll.publications().len(), 2);
    assert_eq!(enroll.publications().first(), enroll.publications().get(1));
    assert_eq!(enroll.callbacks().len(), 1);
}
