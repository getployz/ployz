//! `ployz cloud enroll` join and initialize paths against fake enroll HTTP.

mod catch_up;
mod daemon_sync;
mod harness;

use harness::{
    CLUSTER_DOMAIN, EnrollListen, JoinDaemon, PAIRING, RelayListen, TOKEN, assert_not_held,
    caddy_on, founder_machine, registration, serve_machine, wait_for_held,
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
            "relayUrl": relay.url,
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
    let enroll = EnrollListen::start(json!({
        "kind": "initialize",
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
            "--no-caddy",
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
    let pairing_json = serde_json::to_value(initialized.cloud_pairing.as_ref().unwrap()).unwrap();
    assert_eq!(
        pairing_json,
        json!({
            "relayUrl": relay.url,
            "secret": PAIRING,
        })
    );
    assert!(pairing_json.get("dial").is_none());

    assert_eq!(
        enroll.paths(),
        [
            format!("/api/enroll/{TOKEN}"),
            format!("/api/enroll/{TOKEN}/callback"),
        ]
    );
    assert_eq!(
        enroll.callbacks(),
        [json!({ "machineId": machine_id.as_str() })]
    );

    assert!(
        daemon.reserve_request().is_none(),
        "initialize with --no-dns must not ReserveDomain"
    );

    wait_for_held(&relay.url, PAIRING, machine_id).await;
}

#[tokio::test]
async fn cloud_init_initialize_reserves_hosted_dns() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "initialize",
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
            "--no-caddy",
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
        [json!({ "machineId": machine_id.as_str() })]
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
            "--no-caddy",
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
        [json!({ "machineId": machine_id.as_str() })]
    );
    daemon.initialize_request();
    wait_for_held(&relay.url, PAIRING, machine_id).await;
}

#[tokio::test]
async fn cloud_init_initialize_callback_failure_is_nonfatal_when_on_list() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "initialize",
        "storage": "none",
        "pairing": pairing,
    }))
    .await;
    enroll.fail_callbacks(500);
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
            "--no-caddy",
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
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("WARNING: enroll callback failed"),
        "{stderr}"
    );
    assert_eq!(
        enroll.callbacks(),
        [json!({ "machineId": machine_id.as_str() })]
    );
    wait_for_held(&relay.url, PAIRING, machine_id).await;
}

const DEAD: &str = "expired-pairing";

fn pairing_on(relay: &RelayListen, secret: &str) -> CloudPairing {
    CloudPairing::parse(&relay.url, PairingCredential::parse(secret).unwrap()).unwrap()
}

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
        "--no-caddy",
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
async fn revoked_pairing_resets_and_joins() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let registration = Registered {
        assigned_machine: founder.clone(),
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    };
    let relay = RelayListen::start().await;
    relay.revoke(DEAD).await;
    let dead = pairing_on(&relay, DEAD);
    let live = pairing_on(&relay, PAIRING);
    let enroll = EnrollListen::script([
        json!({ "kind": "initialize", "storage": "none", "pairing": dead }),
        json!({
            "kind": "join",
            "storage": "none",
            "pairing": live,
            "registration": registration,
        }),
    ])
    .await;
    let daemon = JoinDaemon::new(registration);
    let machine_addr = serve_machine(daemon.clone()).await;
    let connect = format!("tcp://{machine_addr}");

    let output = init_cloud(&connect, &enroll.url, "founder", true, true).await;
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Joined Machine founder ({machine_id})")),
        "{stdout}"
    );

    assert_eq!(daemon.reset_count(), 1);
    assert_eq!(enroll.posts().len(), 2);
    assert!(
        enroll.callbacks().is_empty(),
        "join must not POST enroll callback"
    );
    assert_eq!(
        daemon
            .initialize_requests()
            .first()
            .and_then(|init| init.cloud_pairing.as_ref()),
        Some(&dead)
    );
    assert_eq!(daemon.join_request().cloud_pairing.as_ref(), Some(&live));

    assert_not_held(&relay.url, DEAD, machine_id).await;
    wait_for_held(&relay.url, PAIRING, machine_id).await;
}

#[tokio::test]
async fn revoked_pairing_resets_and_initializes_with_new_pairing() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let relay = RelayListen::start().await;
    relay.revoke(DEAD).await;
    let dead = pairing_on(&relay, DEAD);
    let live = pairing_on(&relay, PAIRING);
    let enroll = EnrollListen::script([
        json!({ "kind": "initialize", "storage": "none", "pairing": dead }),
        json!({ "kind": "initialize", "storage": "none", "pairing": live }),
    ])
    .await;
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder,
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    });
    let machine_addr = serve_machine(daemon.clone()).await;
    let connect = format!("tcp://{machine_addr}");

    let output = init_cloud(&connect, &enroll.url, "founder", true, true).await;
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

    assert_eq!(daemon.reset_count(), 1);
    assert_eq!(enroll.posts().len(), 2);
    assert_eq!(
        enroll.callbacks(),
        [json!({ "machineId": machine_id.as_str() })]
    );
    let requests = daemon.initialize_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests
            .first()
            .and_then(|init| init.cloud_pairing.as_ref()),
        Some(&dead)
    );
    assert_eq!(
        requests.get(1).and_then(|init| init.cloud_pairing.as_ref()),
        Some(&live)
    );

    assert_not_held(&relay.url, DEAD, machine_id).await;
    wait_for_held(&relay.url, PAIRING, machine_id).await;
}

#[tokio::test]
async fn revoked_pairing_reset_confirms_unless_yes() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let relay = RelayListen::start().await;
    relay.revoke(DEAD).await;
    let dead = pairing_on(&relay, DEAD);
    let enroll =
        EnrollListen::start(json!({ "kind": "initialize", "storage": "none", "pairing": dead }))
            .await;
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder,
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    });
    let machine_addr = serve_machine(daemon.clone()).await;
    let connect = format!("tcp://{machine_addr}");

    let output = init_cloud(&connect, &enroll.url, "founder", true, false).await;
    assert!(
        !output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("pass --yes"), "{stderr}");
    assert_eq!(daemon.reset_count(), 0);
    assert_eq!(enroll.posts().len(), 1);
    assert!(enroll.callbacks().is_empty());
    assert_eq!(
        daemon
            .initialize_requests()
            .first()
            .and_then(|init| init.cloud_pairing.as_ref()),
        Some(&dead)
    );
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
    let enroll =
        EnrollListen::start(json!({ "kind": "initialize", "storage": "none", "pairing": pairing }))
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
        String::from_utf8_lossy(&output.stderr).contains("rerun with --reset"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(daemon.reset_count(), 0);
    assert!(enroll.posts().is_empty());
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
async fn join_places_observed_caddy_on_this_machine() {
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
    let daemon = JoinDaemon::new(registration.clone()).with_containers(vec![caddy_on(&founder)]);
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
    assert_eq!(ensure_names(&ensured), [("ployz-system", "caddy")]);
}

#[tokio::test]
async fn partial_peer_observation_is_silent_after_verified_caddy_catch_up() {
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
        .with_containers(vec![caddy_on(&founder)])
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
        [("ployz-system", "caddy")]
    );
}

#[tokio::test]
async fn join_no_caddy_skips_caddy_and_still_places_other_globals() {
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
    let daemon = JoinDaemon::new(registration)
        .with_containers(vec![caddy_on(&founder), global_on(&founder, "app", "api")]);
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
            "--no-caddy",
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
async fn join_fails_visibly_when_expected_caddy_cannot_be_placed() {
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
        .with_containers(vec![caddy_on(&founder)])
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
        "Join must fail when Caddy cannot be placed"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains("Joined Machine"),
        "success must not print when Caddy is missing, got {stdout}"
    );
    assert!(
        stderr.contains("Global catch-up is incomplete"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("ployz-system/caddy: run `ployz caddy deploy`"),
        "stderr: {stderr}"
    );
}

#[tokio::test]
async fn join_starts_created_caddy_before_success() {
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
    let mut created = caddy_on(&joiner);
    created.runtime = ployz_core::ContainerRuntimeObservation::Created;
    let daemon = JoinDaemon::new(registration).with_containers(vec![caddy_on(&founder), created]);
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
        [("ployz-system", "caddy")]
    );
}

#[tokio::test]
async fn two_concurrent_joins_each_ensure_caddy_locally() {
    let founder = founder_machine();
    let (first, second) = tokio::join!(
        join_against_founder(&founder),
        join_against_founder(&founder),
    );
    assert_eq!(
        ensure_names(&first),
        [("ployz-system", "caddy")],
        "first Joiner must place Caddy here"
    );
    assert_eq!(
        ensure_names(&second),
        [("ployz-system", "caddy")],
        "second Joiner must place Caddy here"
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
    let daemon = JoinDaemon::new(registration.clone()).with_containers(vec![caddy_on(founder)]);
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
    let mut spec = ployz::caddy::service_spec("caddy:2.10.0".into(), Vec::new(), None);
    spec.name = ployz_core::ServiceName::parse(name).unwrap();
    spec.ports.clear();
    container_on(
        machine,
        spec.to_resolved(
            ployz_core::ServiceId::parse("a".repeat(32)).unwrap(),
            ployz_core::ResolvedUpdateConfig::default(),
        ),
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
    ployz_core::ContainerObservation {
        container_id: ployz_core::ContainerId::parse(hex.to_string().repeat(64)).unwrap(),
        display_name: format!("{}-{hex}", spec.name),
        created_at_unix_nanos: 1,
        machine_id: machine.id,
        project_name: project,
        service_id: spec.service_id,
        service_name: spec.name.clone(),
        kind: ployz_core::ContainerKind::ServiceContainer,
        runtime: ployz_core::ContainerRuntimeObservation::Running {
            health: ployz_core::HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec: spec,
        address: None,
        labels: Default::default(),
    }
}
