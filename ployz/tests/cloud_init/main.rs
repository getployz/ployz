//! `ployz cloud enroll` join and initialize paths against fake enroll HTTP.

mod catch_up;
mod daemon_sync;
mod founder_resumption;
mod harness;

use harness::{
    CLUSTER_DOMAIN, EnrollListen, EventLog, JoinDaemon, PAIRING, RESET_PUBLIC_KEY, RelayListen,
    TOKEN, assert_not_held, founder_machine, ingress_on, registration, serve_ingress_probe,
    serve_machine, wait_for_held,
};
use ployz_core::{
    CloudPairing, InitializeRequest, InspectRequest, LocalMachinePhase, PairingCredential,
    Registered, SetCloudPairingRequest, op,
};
use serde_json::json;

#[tokio::test]
async fn cloud_init_join_participates_and_appears_on_list_held() {
    let registration = registration();
    let machine_id = registration.assigned_machine.id;
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "join",
        "storage": "none",
        "pairing": pairing,
        "registration": registration,
    }))
    .await;
    let daemon = JoinDaemon::new(registration.clone());
    let machine_addr = serve_machine(daemon.clone()).await;

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
            "joiner",
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Joined Machine joiner ({machine_id})")),
        "{stdout}"
    );
    let joined = daemon.join_request();
    let pairing_json = serde_json::to_value(joined.cloud_pairing.as_ref().unwrap()).unwrap();
    assert_eq!(
        pairing_json,
        json!({
            "relayUrl": format!("{}/", relay.url),
            "secret": PAIRING,
        })
    );
    assert_eq!(joined.registration.assigned_machine.id, machine_id);

    assert!(
        daemon.ensure_requests().is_empty(),
        "Join without observed Globals must not place slots"
    );

    let paths = enroll.paths();
    assert_eq!(paths, [format!("/api/enroll/{TOKEN}")]);
    assert!(
        enroll.callbacks().is_empty(),
        "join must not POST enroll callback"
    );

    wait_for_held(&relay.url, PAIRING, machine_id).await;
}

#[tokio::test]
async fn cloud_zfs_rejects_a_remote_machine_before_join() {
    let registration = registration();
    let pairing = CloudPairing::parse(
        "https://relay.example.invalid",
        PairingCredential::parse(PAIRING).unwrap(),
    )
    .unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "join",
        "storage": "zfs",
        "pairing": pairing,
        "registration": registration,
    }))
    .await;
    let daemon = JoinDaemon::new(registration);
    let machine_addr = serve_machine(daemon).await;

    let output = init_cloud(
        &format!("tcp://{machine_addr}"),
        &enroll.url,
        "joiner",
        false,
        true,
    )
    .await;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "zfs storage preparation requires running ployz cloud enroll on the Machine itself"
        ),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let details = connect_daemon(machine_addr)
        .await
        .call::<op::Inspect>(InspectRequest::default(), None)
        .await
        .unwrap();
    assert_eq!(details.phase, LocalMachinePhase::Uninitialized);
}

#[tokio::test]
async fn cloud_init_initialize_participates_and_appears_on_list_held() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let events = EventLog::default();
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
    .with_events(events.clone());
    let machine_addr = serve_machine(daemon.clone()).await;

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
            "--network",
            "10.210.0.0/16",
            "--wg-mtu",
            "1400",
            "--no-ingress",
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Initialised Machine founder ({machine_id})")),
        "{stdout}"
    );

    let initialized = daemon.initialize_request();
    assert_eq!(initialized.name.as_str(), "founder");
    assert_eq!(initialized.cluster_network.to_string(), "10.210.0.0/16");
    assert_eq!(initialized.wireguard_mtu, Some(1400));
    assert!(
        initialized.cloud_pairing.is_none(),
        "Cloud Pairing is published only after founder setup"
    );

    assert_eq!(
        enroll.paths(),
        [
            format!("/api/enroll/{TOKEN}"),
            format!("/api/enroll/{TOKEN}/callback"),
        ]
    );
    assert_eq!(
        enroll.callbacks(),
        [json!({
            "machineId": machine_id.as_str(),
            "pairingCredential": PAIRING,
        })]
    );

    assert!(
        daemon.reserve_request().is_none(),
        "initialize with --no-dns must not ReserveDomain"
    );
    assert_eq!(
        events.entries(),
        ["initialize", "set_cloud_pairing", "callback"]
    );

    wait_for_held(&relay.url, PAIRING, machine_id).await;
}

#[tokio::test]
async fn caddy_lookup_failure_happens_before_initialize() {
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
        assigned_machine: founder,
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    });
    let machine_addr = serve_machine(daemon.clone()).await;
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);

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
        .env("HTTPS_PROXY", &proxy)
        .env("https_proxy", &proxy)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .output()
        .await
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("list Docker Hub Caddy tags"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(daemon.initialize_requests().is_empty());
    assert!(enroll.callbacks().is_empty());
    assert_not_held(&relay.url, PAIRING, machine_id).await;
}

#[tokio::test]
async fn cloud_init_initialize_reserves_hosted_dns() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let events = EventLog::default();
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
    .with_events(events.clone());
    let machine_addr = serve_machine(daemon.clone()).await;

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
            "--no-ingress",
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Reserved Cluster domain: {CLUSTER_DOMAIN}")),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("Initialised Machine founder ({machine_id})")),
        "{stdout}"
    );

    let reserved = daemon.reserve_request().expect("ReserveDomain was called");
    assert_eq!(reserved.endpoint, "https://dns.uncloud.run/v1");
    assert_ne!(
        reserved.endpoint,
        format!("{}/api/dns/v1", enroll.url),
        "Cloud enroll must not derive hosted DNS from --cloud-url"
    );
    assert_eq!(
        enroll.callbacks(),
        [json!({
            "machineId": machine_id.as_str(),
            "pairingCredential": PAIRING,
        })]
    );
    assert_eq!(
        events.entries(),
        [
            "initialize",
            "reserve_domain",
            "set_cloud_pairing",
            "callback"
        ]
    );
}

#[tokio::test]
async fn cloud_init_retries_not_yet_then_joins() {
    let registration = registration();
    let machine_id = registration.assigned_machine.id;
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::script([
        json!({"kind": "not_yet", "retryAfter": 0}),
        json!({
            "kind": "join",
            "storage": "none",
            "pairing": pairing,
            "registration": registration,
        }),
    ])
    .await;
    let daemon = JoinDaemon::new(registration.clone());
    let machine_addr = serve_machine(daemon.clone()).await;

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
            "joiner",
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Joined Machine joiner ({machine_id})")),
        "{stdout}"
    );

    let posts = enroll.posts();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts.first(), posts.get(1));
    assert_eq!(
        posts.first().and_then(|post| post.get("protocolVersion")),
        Some(&json!(2))
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Another Machine is founding this Organization; waiting"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(enroll.paths(), vec![format!("/api/enroll/{TOKEN}"); 2]);
    assert!(
        enroll.callbacks().is_empty(),
        "join must not POST enroll callback"
    );
    wait_for_held(&relay.url, PAIRING, machine_id).await;
}

#[tokio::test]
async fn cloud_init_retries_not_yet_then_initializes() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::script([
        json!({"kind": "not_yet", "retryAfter": 0}),
        json!({
            "kind": "initialize",
            "resumed": false,
            "storage": "none",
            "pairing": pairing,
        }),
    ])
    .await;
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder,
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    });
    let machine_addr = serve_machine(daemon.clone()).await;

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
            "--no-ingress",
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Initialised Machine founder ({machine_id})")),
        "{stdout}"
    );

    let posts = enroll.posts();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts.first(), posts.get(1));
    assert_eq!(
        enroll.callbacks(),
        [json!({
            "machineId": machine_id.as_str(),
            "pairingCredential": PAIRING,
        })]
    );
    daemon.initialize_request();
    wait_for_held(&relay.url, PAIRING, machine_id).await;
}

const DEAD: &str = "expired-pairing";

async fn init_cloud(
    connect: &str,
    enroll_url: &str,
    name: &str,
    reset: bool,
    yes: bool,
) -> std::process::Output {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"));
    command.args([
        "--connect",
        connect,
        "cloud",
        "enroll",
        TOKEN,
        "--cloud-url",
        enroll_url,
        "--name",
        name,
        "--no-ingress",
        "--no-dns",
    ]);
    if reset {
        command.arg("--reset");
    }
    if yes {
        command.arg("--yes");
    }
    command.output().await.unwrap()
}

#[tokio::test]
async fn revoked_pairing_does_not_release_or_transfer_founding() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let relay = RelayListen::start().await;
    relay.revoke(DEAD).await;
    let dead = CloudPairing::parse(&relay.url, PairingCredential::parse(DEAD).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "initialize",
        "resumed": false,
        "storage": "none",
        "pairing": dead,
    }))
    .await;
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder,
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    });
    let machine_addr = serve_machine(daemon.clone()).await;
    let connect = format!("tcp://{machine_addr}");

    let output = init_cloud(&connect, &enroll.url, "founder", false, true).await;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("invalid Pairing Credential"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(daemon.reset_count(), 0);
    assert_eq!(daemon.initialize_requests().len(), 1);
    assert_eq!(enroll.posts().len(), 1);
    assert!(enroll.callbacks().is_empty());
    assert_not_held(&relay.url, DEAD, machine_id).await;
}

#[tokio::test]
async fn initialized_machine_yes_refuses_reset_without_explicit_reset() {
    let founder = founder_machine();
    let pairing = CloudPairing::parse(
        "https://relay.example.invalid",
        PairingCredential::parse(PAIRING).unwrap(),
    )
    .unwrap();
    let enroll = EnrollListen::start(
        json!({ "kind": "initialize", "resumed": false, "storage": "none", "pairing": pairing }),
    )
    .await;
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder.clone(),
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    });
    let machine_addr = serve_machine(daemon.clone()).await;
    let mut client = connect_daemon(machine_addr).await;
    client
        .call::<op::Initialize>(
            InitializeRequest {
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
        &format!("tcp://{machine_addr}"),
        &enroll.url,
        "founder",
        false,
        true,
    )
    .await;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("new founding claim requires an uninitialized Machine"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(daemon.reset_count(), 0);
    assert_eq!(enroll.posts().len(), 1);
}

#[tokio::test]
async fn invalid_cluster_network_does_not_reset_an_initialized_machine() {
    let founder = founder_machine();
    let enroll = EnrollListen::start(json!({
        "kind": "initialize",
        "resumed": false,
        "storage": "none",
        "pairing": CloudPairing::parse(
            "https://relay.example.invalid",
            PairingCredential::parse(PAIRING).unwrap()
        )
        .unwrap(),
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
                public_ip: None,
                advertised_endpoints: founder.advertised_endpoints,
                wireguard_mtu: None,
                cloud_pairing: None,
            },
            None,
        )
        .await
        .unwrap();

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
            "--network",
            "not-a-cidr",
            "--reset",
            "--yes",
            "--no-ingress",
            "--no-dns",
        ])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(daemon.reset_count(), 0);
    assert!(enroll.posts().is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid value 'not-a-cidr' for '--network <network>'"),
        "{stderr}"
    );
}

#[tokio::test]
async fn reset_enroll_posts_the_rotated_public_key() {
    let local = registration();
    let mut assigned = local.clone();
    assigned.assigned_machine.id = ployz_core::MachineId::parse("c".repeat(32)).unwrap();
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let mut rotated = assigned.clone();
    rotated.assigned_machine.public_key = RESET_PUBLIC_KEY;
    let enroll = EnrollListen::start(json!({
        "kind": "join",
        "storage": "none",
        "pairing": pairing,
        "registration": rotated,
    }))
    .await;
    let daemon = JoinDaemon::new(local.clone());
    let machine_addr = serve_machine(daemon.clone()).await;
    let mut client = connect_daemon(machine_addr).await;
    client
        .call::<op::Initialize>(
            InitializeRequest {
                name: local.assigned_machine.name,
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: local.assigned_machine.advertised_endpoints,
                wireguard_mtu: None,
                cloud_pairing: None,
            },
            None,
        )
        .await
        .unwrap();
    let before = daemon.public_key();

    let output = init_cloud(
        &format!("tcp://{machine_addr}"),
        &enroll.url,
        "rejoined",
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
    assert_ne!(daemon.public_key(), before);
    let posted = enroll.posts();
    assert_eq!(posted.len(), 1);
    assert_eq!(
        posted.first().and_then(|post| post.get("publicKey")),
        Some(&json!(daemon.public_key().to_string()))
    );
    assert_eq!(
        daemon
            .join_request()
            .registration
            .assigned_machine
            .public_key,
        daemon.public_key()
    );
}

#[tokio::test]
async fn reset_enroll_does_not_occupy_the_name_with_the_pre_reset_key() {
    let local = registration();
    let mut assigned = local.clone();
    assigned.assigned_machine.id = ployz_core::MachineId::parse("c".repeat(32)).unwrap();
    assigned.assigned_machine.name = ployz_core::MachineName::parse("rejoined").unwrap();
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::occupying_join(json!({
        "kind": "join",
        "storage": "none",
        "pairing": pairing,
        "registration": assigned,
    }))
    .await;
    let daemon = JoinDaemon::new(local.clone());
    let machine_addr = serve_machine(daemon.clone()).await;
    let mut client = connect_daemon(machine_addr).await;
    client
        .call::<op::Initialize>(
            InitializeRequest {
                name: local.assigned_machine.name,
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: local.assigned_machine.advertised_endpoints,
                wireguard_mtu: None,
                cloud_pairing: None,
            },
            None,
        )
        .await
        .unwrap();

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        init_cloud(
            &format!("tcp://{machine_addr}"),
            &enroll.url,
            "rejoined",
            true,
            true,
        ),
    )
    .await
    .expect("enroll must finish; not_yet means the first POST occupied the name");
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let posted: Vec<String> = enroll
        .posts()
        .iter()
        .map(|post| {
            post.get("publicKey")
                .and_then(serde_json::Value::as_str)
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(posted, [daemon.public_key().to_string()]);
    assert_eq!(
        daemon
            .join_request()
            .registration
            .assigned_machine
            .public_key,
        daemon.public_key()
    );
}

#[tokio::test]
async fn initialize_without_pairing_stays_off_list_until_set_cloud_pairing() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder.clone(),
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    });
    let machine_addr = serve_machine(daemon).await;
    let mut client = connect_daemon(machine_addr).await;

    client
        .call::<op::Initialize>(
            InitializeRequest {
                name: founder.name.clone(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: founder.advertised_endpoints.clone(),
                wireguard_mtu: None,
                cloud_pairing: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_not_held(&relay.url, PAIRING, machine_id).await;

    client
        .call::<op::SetCloudPairing>(
            SetCloudPairingRequest {
                cloud_pairing: Some(pairing),
            },
            None,
        )
        .await
        .unwrap();
    wait_for_held(&relay.url, PAIRING, machine_id).await;
}

#[tokio::test]
async fn set_cloud_pairing_none_leaves_relay_list() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder.clone(),
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    });
    let machine_addr = serve_machine(daemon).await;
    let mut client = connect_daemon(machine_addr).await;

    client
        .call::<op::Initialize>(
            InitializeRequest {
                name: founder.name.clone(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: founder.advertised_endpoints.clone(),
                wireguard_mtu: None,
                cloud_pairing: None,
            },
            None,
        )
        .await
        .unwrap();
    client
        .call::<op::SetCloudPairing>(
            SetCloudPairingRequest {
                cloud_pairing: Some(pairing),
            },
            None,
        )
        .await
        .unwrap();
    wait_for_held(&relay.url, PAIRING, machine_id).await;

    client
        .call::<op::SetCloudPairing>(
            SetCloudPairingRequest {
                cloud_pairing: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_not_held(&relay.url, PAIRING, machine_id).await;
}

#[tokio::test]
async fn join_places_observed_ingress_on_this_machine() {
    let founder = founder_machine();
    let mut registration = registration();
    registration.visible_peers = vec![founder.clone()];
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "join",
        "storage": "none",
        "pairing": pairing,
        "registration": registration,
    }))
    .await;
    let daemon = JoinDaemon::new(registration.clone()).with_containers(vec![ingress_on(&founder)]);
    let machine_addr = serve_machine(daemon.clone()).await;

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
            "joiner",
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Joined Machine joiner"), "{stdout}");
    let ensured = daemon.ensure_requests();
    assert_eq!(ensure_names(&ensured), [("ployz-system", "ingress")]);
}

#[tokio::test]
async fn partial_peer_observation_is_silent_after_verified_ingress_catch_up() {
    let founder = founder_machine();
    let mut unreachable = founder.clone();
    unreachable.id = ployz_core::MachineId::parse("d".repeat(32)).unwrap();
    unreachable.name = ployz_core::MachineName::parse("unreachable").unwrap();
    let mut registration = registration();
    registration.visible_peers = vec![founder.clone(), unreachable.clone()];
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "join",
        "storage": "none",
        "pairing": pairing,
        "registration": registration,
    }))
    .await;
    let daemon = JoinDaemon::new(registration)
        .with_containers(vec![ingress_on(&founder)])
        .fail_list_on(unreachable.id);
    let machine_addr = serve_machine(daemon.clone()).await;

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
            "joiner",
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
    assert!(
        !String::from_utf8_lossy(&output.stderr)
            .contains("Global catch-up used partial Service observations")
    );
    assert_eq!(
        ensure_names(&daemon.ensure_requests()),
        [("ployz-system", "ingress")]
    );
}

#[tokio::test]
async fn join_no_ingress_skips_ingress_and_still_places_other_globals() {
    let founder = founder_machine();
    let mut registration = registration();
    registration.visible_peers = vec![founder.clone()];
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "join",
        "storage": "none",
        "pairing": pairing,
        "registration": registration,
    }))
    .await;
    let daemon = JoinDaemon::new(registration).with_containers(vec![
        ingress_on(&founder),
        global_on(&founder, "app", "api"),
    ]);
    let machine_addr = serve_machine(daemon.clone()).await;

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
            "joiner",
            "--no-ingress",
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
    let ensured = daemon.ensure_requests();
    assert_eq!(ensure_names(&ensured), [("app", "api")]);
}

#[tokio::test]
async fn join_fails_visibly_when_expected_ingress_cannot_be_placed() {
    let founder = founder_machine();
    let mut registration = registration();
    registration.visible_peers = vec![founder.clone()];
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "join",
        "storage": "none",
        "pairing": pairing,
        "registration": registration,
    }))
    .await;
    let daemon = JoinDaemon::new(registration.clone())
        .with_containers(vec![ingress_on(&founder)])
        .fail_ensure();
    let machine_addr = serve_machine(daemon).await;

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
            "joiner",
            "--yes",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        !output.status.success(),
        "Join must fail when Ingress Proxy cannot be placed"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains("Joined Machine"),
        "success must not print when Ingress Proxy is missing, got {stdout}"
    );
    assert!(
        stderr.contains("Global catch-up is incomplete"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("ployz-system/ingress: run `ployz ingress deploy`"),
        "stderr: {stderr}"
    );
}

#[tokio::test]
async fn join_starts_created_ingress_before_success() {
    let founder = founder_machine();
    let mut registration = registration();
    let joiner = registration.assigned_machine.clone();
    registration.visible_peers = vec![founder.clone()];
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "join",
        "storage": "none",
        "pairing": pairing,
        "registration": registration,
    }))
    .await;
    let mut created = ingress_on(&joiner);
    created
        .try_update(|parts| parts.runtime = ployz_core::ContainerRuntimeObservation::Created)
        .unwrap();
    let daemon = JoinDaemon::new(registration).with_containers(vec![ingress_on(&founder), created]);
    let machine_addr = serve_machine(daemon.clone()).await;

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
            "joiner",
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
    assert_eq!(
        ensure_names(&daemon.ensure_requests()),
        [("ployz-system", "ingress")]
    );
}

#[tokio::test]
async fn two_concurrent_joins_each_ensure_ingress_locally() {
    let founder = founder_machine();
    let (first, second) = tokio::join!(
        join_against_founder(&founder),
        join_against_founder(&founder),
    );
    assert_eq!(
        ensure_names(&first),
        [("ployz-system", "ingress")],
        "first Joiner must place Ingress Proxy here"
    );
    assert_eq!(
        ensure_names(&second),
        [("ployz-system", "ingress")],
        "second Joiner must place Ingress Proxy here"
    );
}

async fn join_against_founder(
    founder: &ployz_core::Machine,
) -> Vec<ployz_core::EnsureGlobalSlotRequest> {
    let mut registration = registration();
    registration.assigned_machine.id = ployz_core::MachineId::random();
    registration.visible_peers = vec![founder.clone()];
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "join",
        "storage": "none",
        "pairing": pairing,
        "registration": registration,
    }))
    .await;
    let daemon = JoinDaemon::new(registration.clone()).with_containers(vec![ingress_on(founder)]);
    let machine_addr = serve_machine(daemon.clone()).await;
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
            "joiner",
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
    daemon.ensure_requests()
}

fn ensure_names(requests: &[ployz_core::EnsureGlobalSlotRequest]) -> Vec<(&str, &str)> {
    requests
        .iter()
        .map(|request| {
            (
                request.project_name.as_str(),
                request.resolved_spec.name.as_str(),
            )
        })
        .collect()
}

async fn connect_daemon(address: std::net::SocketAddr) -> ployz::connect::Client {
    ployz::connect::connect_selected_with(
        ployz::context::SelectedConnections {
            source: ployz::context::ConnectionSource::Direct,
            connections: vec![ployz::context::Connection::tcp(address)],
        },
        std::sync::Arc::new(ployz::connect::SystemConnector::default()),
    )
    .await
    .unwrap()
}

fn global_on(
    machine: &ployz_core::Machine,
    project: &str,
    name: &str,
) -> ployz_core::ContainerObservation {
    let spec: ployz_core::RequestedServiceSpec = serde_json::from_value(json!({
        "name": name,
        "mode": { "mode": "global" },
        "container": { "image": "nginx", "pull_policy": "missing" }
    }))
    .unwrap();
    container_on(
        machine,
        spec.to_resolved(
            ployz_core::ServiceId::parse("a".repeat(32)).unwrap(),
            ployz_core::ResolvedUpdateConfig::default(),
        )
        .expect("volume graph is scoped"),
        ployz_core::ProjectName::parse(project).unwrap(),
        'b',
    )
}

fn container_on(
    machine: &ployz_core::Machine,
    spec: ployz_core::ResolvedServiceSpec,
    project: ployz_core::ProjectName,
    hex: char,
) -> ployz_core::ContainerObservation {
    ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        container_id: ployz_core::ContainerId::parse(hex.to_string().repeat(64)).unwrap(),
        display_name: format!("{}-{hex}", spec.name),
        created_at_unix_nanos: 1,
        machine_id: machine.id,
        project_name: project,
        kind: ployz_core::ContainerKind::ServiceContainer,
        runtime: ployz_core::ContainerRuntimeObservation::Running {
            health: ployz_core::HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec: spec,
        address: None,
        labels: Default::default(),
    })
    .unwrap()
}
