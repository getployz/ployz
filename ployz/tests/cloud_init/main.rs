//! `ployz cloud enroll` join and initialize paths against fake enroll HTTP.

mod harness;

use harness::{
    CLUSTER_DOMAIN, EnrollListen, JoinDaemon, PAIRING, RelayListen, TOKEN, assert_not_held,
    founder_machine, registration, serve_machine, wait_for_held,
};
use ployz_core::{CloudPairing, PairingCredential, Registered};
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

    let paths = enroll.paths();
    assert_eq!(paths, [format!("/api/enroll/{TOKEN}")]);
    assert!(
        enroll.callbacks().is_empty(),
        "join must not POST enroll callback"
    );

    wait_for_held(&relay.url, PAIRING, machine_id).await;
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
        json!({ "kind": "initialize", "pairing": dead }),
        json!({
            "kind": "join",
            "pairing": live,
            "registration": registration,
        }),
    ])
    .await;
    let daemon = JoinDaemon::new(registration);
    let machine_addr = serve_machine(daemon.clone()).await;
    let connect = format!("tcp://{machine_addr}");

    let output = init_cloud(&connect, &enroll.url, "founder", true).await;
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
        json!({ "kind": "initialize", "pairing": dead }),
        json!({ "kind": "initialize", "pairing": live }),
    ])
    .await;
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder,
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    });
    let machine_addr = serve_machine(daemon.clone()).await;
    let connect = format!("tcp://{machine_addr}");

    let output = init_cloud(&connect, &enroll.url, "founder", true).await;
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
    let enroll = EnrollListen::start(json!({ "kind": "initialize", "pairing": dead })).await;
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder,
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    });
    let machine_addr = serve_machine(daemon.clone()).await;
    let connect = format!("tcp://{machine_addr}");

    let output = init_cloud(&connect, &enroll.url, "founder", false).await;
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
