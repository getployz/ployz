//! Founder completion loss, resumption, and convergent-tail regressions.

use super::*;
use harness::CallbackReply;

#[tokio::test]
async fn lost_completion_response_reruns_idempotently_when_cloud_is_ready() {
    let mut founder = founder_machine();
    let machine_id = founder.id;
    founder.accepts_ingress = false;
    let pairing = json!({ "secret": PAIRING });
    let registration = Registered {
        assigned_machine: founder,
        visible_peers: Vec::new(),
        target_versions: Default::default(),
        sealing_key: None,
        devices: Vec::new(),
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
        stderr.contains("rerun the same ployz server add command"),
        "{stderr}"
    );
    assert_eq!(
        enroll.callbacks(),
        vec![json!({ "machineId": machine_id.as_str(), "pairingCredential": PAIRING }); 1]
    );

    enroll.set_callback_reply(CallbackReply::Echo);
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
    let pairing = json!({ "secret": PAIRING });
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
        sealing_key: None,
        devices: Vec::new(),
    })
    .lose_lifecycle_reply();
    let machine_addr = serve_machine(daemon.clone()).await;
    connect_daemon(machine_addr)
        .await
        .call::<op::Enroll>(
            EnrollRequest {
                role: EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: founder.name,
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: None,
                    advertised_endpoints: founder.advertised_endpoints,
                    grant: Some(ployz_core::FoundingGrant::Claim {
                        claim: ployz_core::FoundingClaim::of_pairing(PAIRING),
                    }),
                },
                wireguard_mtu: None,
                pinned: Default::default(),
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
        daemon
            .initialize_requests()
            .last()
            .and_then(|founded| founded.grant),
        Some(ployz_core::FoundingGrant::Claim {
            claim: ployz_core::FoundingClaim::of_pairing(PAIRING)
        }),
        "the founding daemon keeps the claim it founded under"
    );
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
    let pairing = json!({ "secret": PAIRING });
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
        sealing_key: None,
        devices: Vec::new(),
    });
    let machine_addr = serve_machine(daemon.clone()).await;
    connect_daemon(machine_addr)
        .await
        .call::<op::Enroll>(
            EnrollRequest {
                role: EnrollAs::Founder {
                    initial_policy: ployz_core::InitialMachinePolicy {
                        accepts_ingress: false,
                        ..Default::default()
                    },
                    name: founder.name,
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: None,
                    advertised_endpoints: founder.advertised_endpoints,
                    grant: Some(ployz_core::FoundingGrant::Claim {
                        claim: ployz_core::FoundingClaim::of_pairing(PAIRING),
                    }),
                },
                wireguard_mtu: None,
                pinned: Default::default(),
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
    let requested = ployz_core::caddy_service_spec("caddy:2.10.0".into(), Default::default());
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
        ployz_core::Namespace::system(),
        'c',
    );
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder.clone(),
        visible_peers: Vec::new(),
        target_versions: Default::default(),
        sealing_key: None,
        devices: Vec::new(),
    })
    .with_containers(vec![ingress]);
    let machine_addr = serve_machine(daemon.clone()).await;
    connect_daemon(machine_addr)
        .await
        .call::<op::Enroll>(
            EnrollRequest {
                role: EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: founder.name,
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: founder.public_ip,
                    advertised_endpoints: founder.advertised_endpoints,
                    grant: Some(ployz_core::FoundingGrant::Claim {
                        claim: ployz_core::FoundingClaim::of_pairing(PAIRING),
                    }),
                },
                wireguard_mtu: None,
                pinned: Default::default(),
            },
            None,
        )
        .await
        .unwrap();
    let events = EventLog::default();
    let daemon = daemon.with_events(events.clone());
    let pairing = json!({ "secret": PAIRING });
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
            "server",
            "add",
            "--token-file",
            super::harness::token_file(),
            "--cloud-url",
            &enroll.url,
            "--name",
            "founder",
            "--ingress-image",
            "caddy:2.10.0",
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
    assert_eq!(
        events.entries(),
        ["set_management_client", "publish", "callback"]
    );
}

#[tokio::test]
async fn founder_tail_recovers_lost_replies_without_replaying_mutations() {
    let mut founder = founder_machine();
    founder.public_ip = Some("127.0.0.1".parse().unwrap());
    let events = EventLog::default();
    let pairing = json!({ "secret": PAIRING });
    let enroll = EnrollListen::script_recording(
        [json!({
            "kind": "initialize", "resumed": false, "storage": "none", "pairing": pairing,
        })],
        events.clone(),
    )
    .await;
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder,
        visible_peers: Vec::new(),
        target_versions: Default::default(),
        sealing_key: None,
        devices: Vec::new(),
    })
    .with_events(events.clone())
    .transient_founder_tail_failures(1);
    let machine_addr = serve_machine(daemon.clone()).await;

    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let output = super::harness::cli()
        .args([
            "--connect",
            &format!("ssh://root@{machine_addr}"),
            "server",
            "add",
            "--token-file",
            super::harness::token_file(),
            "--cloud-url",
            &enroll.url,
            "--name",
            "founder",
            "--ingress-image",
            "caddy:2.10.0",
            "--yes",
            "--reset",
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
    assert_eq!(
        daemon.initialize_requests().len(),
        1,
        "lost Initialize reply must be recovered by Inspect"
    );
    assert_eq!(daemon.reset_count(), 0);
    assert_eq!(daemon.founder_tail_attempts(), [2, 2]);
    let containers = daemon.containers();
    assert_eq!(
        containers.len(),
        1,
        "a resent create must land on the same slot"
    );
    assert_eq!(
        containers.first().unwrap().service_name().as_str(),
        "ingress"
    );
    assert_eq!(
        events.entries(),
        [
            "initialize",
            "deploy_ingress",
            "set_management_client",
            "publish",
            "callback",
        ]
    );
    assert_eq!(enroll.posts().len(), 1);
    assert_eq!(enroll.callbacks().len(), 1);
}

#[tokio::test]
async fn founder_recovery_rejects_replaced_identity() {
    {
        let pairing = json!({ "secret": PAIRING });
        let enroll = EnrollListen::start(json!({
            "kind": "initialize", "resumed": false, "storage": "none", "pairing": pairing,
        }))
        .await;
        let daemon = JoinDaemon::new(Registered {
            assigned_machine: founder_machine(),
            visible_peers: Vec::new(),
            target_versions: Default::default(),
            sealing_key: None,
            devices: Vec::new(),
        });
        let daemon = daemon.replace_identity_on_initialize();
        let address = serve_machine(daemon.clone()).await;
        let output = super::harness::cli()
            .args([
                "--connect",
                &format!("ssh://root@{address}"),
                "server",
                "add",
                "--token-file",
                super::harness::token_file(),
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
        assert!(error.contains("different enrollment"), "{error}");
        assert_eq!(daemon.initialize_requests().len(), 1);
        assert_eq!(daemon.reset_count(), 0);
        assert!(enroll.callbacks().is_empty());
    }
}

#[tokio::test]
async fn publication_failure_does_not_complete_and_resumes_the_same_founder() {
    let mut founder = founder_machine();
    founder.accepts_ingress = false;
    let pairing = json!({ "secret": PAIRING });
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
        sealing_key: None,
        devices: Vec::new(),
    });
    let address = serve_machine(daemon.clone()).await;
    let connect = format!("ssh://root@{address}");
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

/// A participating Server, answering as the founder Cloud resumes, whose daemon reports `founding`.
async fn participating_founder(
    resumed: bool,
    founding: ployz_core::Founding,
) -> (EnrollListen, JoinDaemon, std::process::Output) {
    let mut founder = founder_machine();
    let policy = ployz_core::InitialMachinePolicy {
        accepts_ingress: false,
        ..Default::default()
    };
    founder.labels = policy.labels;
    founder.accepts_builds = policy.accepts_builds;
    founder.accepts_services = policy.accepts_services;
    founder.accepts_ingress = policy.accepts_ingress;
    let enroll = EnrollListen::start(json!({
        "kind": "initialize",
        "resumed": resumed,
        "storage": "none",
        "pairing": { "secret": PAIRING },
    }))
    .await;
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder,
        visible_peers: Vec::new(),
        target_versions: Default::default(),
        sealing_key: None,
        devices: Vec::new(),
    })
    .participating()
    .founded(founding);
    let machine_addr = serve_machine(daemon.clone()).await;
    let output = init_cloud(
        &format!("ssh://root@{machine_addr}"),
        &enroll.url,
        "founder",
        false,
        true,
    )
    .await;
    (enroll, daemon, output)
}

fn release_callback() -> serde_json::Value {
    json!({
        "stage": "release",
        "machineId": founder_machine().id.as_str(),
        "pairingCredential": PAIRING,
    })
}

#[tokio::test]
async fn resumed_founder_refuses_a_cluster_its_claim_did_not_found() {
    for founding in [
        ployz_core::Founding::Standalone,
        ployz_core::Founding::Joined,
        ployz_core::Founding::Claim {
            claim: ployz_core::FoundingClaim::of_pairing("another-pairing"),
        },
    ] {
        let (enroll, daemon, output) = participating_founder(true, founding).await;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{founding:?} resumed: {stderr}");
        assert!(stderr.contains("didn't found"), "{founding:?}: {stderr}");
        assert!(
            stderr.contains("let go of this founding attempt"),
            "{founding:?}: {stderr}"
        );
        assert!(!stderr.contains("--reset"), "{founding:?}: {stderr}");
        assert!(daemon.initialize_requests().is_empty(), "{founding:?}");
        assert_eq!(enroll.callbacks(), [release_callback()], "{founding:?}");
    }
}

#[tokio::test]
async fn resumed_founder_resumes_a_founding_its_daemon_never_recorded() {
    let (enroll, _daemon, output) =
        participating_founder(true, ployz_core::Founding::Unrecorded).await;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !enroll.callbacks().contains(&release_callback()),
        "a resumed founder keeps its claim"
    );
}

#[tokio::test]
async fn new_claim_on_a_participating_server_lets_go_of_the_claim() {
    let (enroll, daemon, output) = participating_founder(
        false,
        ployz_core::Founding::Claim {
            claim: ployz_core::FoundingClaim::of_pairing(PAIRING),
        },
    )
    .await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("let go of this founding attempt"),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "To add this Server instead, make a new token with `ployz server add --command --reset` and run the line it prints on this Server"
        ),
        "{stderr}"
    );
    assert!(daemon.initialize_requests().is_empty());
    assert_eq!(enroll.callbacks(), [release_callback()]);
}
