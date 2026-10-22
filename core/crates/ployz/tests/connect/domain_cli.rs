//! `domain ls` and `domain check` in a Standalone Cluster: with no Cloud to observe the
//! Servers, the CLI reads their certificates itself.

use std::path::Path;

use ployz_core::{CertificateAvailability, CertificateHost, CertificateObservation, IngressHost};
use ployz_store::{
    Actor, Admit, ConfigStore, Deploy, DeploymentId, EnvironmentName, EnvironmentRef,
    OrganizationId, ProjectName, RunEvidence, RunnerId, SealingKey, Trusted,
};

use super::*;

const HOST: &str = "web.example.com";

/// `ployz --json ARGS` over `address`, with its own home and the in-process Store in `store`.
async fn ployz(store: &Path, address: std::net::SocketAddr, args: &[&str]) -> Value {
    let home = tempfile::tempdir().unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .current_dir(home.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env("PLOYZ_CONFIG", home.path().join("config.yaml"))
        .env("PLOYZ_STORE", format!("sqlite:{}", store.display()))
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .env_remove("PLOYZ_PROJECT")
        .env_remove("PLOYZ_ENV")
        .arg("--json")
        .arg("--connect")
        .arg(format!("tcp://{address}"))
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).unwrap()
}

/// Deploy Project shop's Default Environment and run it to success, as the CLI's own
/// runner would once it reached the Servers.
fn applied(store: &Path) {
    let key = SealingKey::from_file(&store.with_extension("db.key")).unwrap();
    let local = ConfigStore::open(&format!("sqlite:{}", store.display()), key).unwrap();
    let who = Actor::system(OrganizationId::parse("local").unwrap());
    let id = DeploymentId::parse(uuid::Uuid::new_v4().to_string()).unwrap();
    let deploy = Admit::Deploy(Deploy {
        id: id.clone(),
        environment: EnvironmentRef {
            project: Some(ProjectName::parse("shop").unwrap()),
            environment: Some(EnvironmentName::parse("production").unwrap()),
        },
        services: Vec::new(),
        version: None,
        upload: None,
        accept_volume_loss: Vec::new(),
        message: None,
        prune: false,
    });
    local
        .write_trusted(&who, &deploy, &Trusted::default())
        .unwrap();
    let runner = RunnerId::parse("standalone-cli").unwrap();
    let claimed = local.claim(&id, &runner).unwrap();
    let preview = serde_json::from_value(json!({
        "namespace": claimed.intent.namespace, "operations": [],
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    local
        .record(&id, &runner, RunEvidence::Prepared(preview))
        .unwrap();
    let outcome = serde_json::from_value(json!({"type": "success", "completed": []})).unwrap();
    local
        .record(
            &id,
            &runner,
            RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
}

#[tokio::test]
async fn a_live_domain_reads_ready_without_a_cloud() {
    let service = DiscoveryService::new(test_description());
    let mut frame = empty_watch_frame();
    frame.machines.push(machine('a', "one"));
    frame.certificates.push(CertificateObservation {
        hostname: CertificateHost::from(IngressHost::parse(HOST).unwrap()),
        status: CertificateAvailability::Available,
        last_error: None,
        backoff: None,
        via_proxy: false,
    });
    service.emit_watch_frame_on_open(frame);
    let (address, server) = serve_discovery(service).await;
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store.db");

    ployz(&store, address, &["project", "new", "shop"]).await;
    ployz(
        &store,
        address,
        &["service", "add", "web", "--image", "nginx:1"],
    )
    .await;
    ployz(
        &store,
        address,
        &["domain", "add", "web", HOST, "--port", "80"],
    )
    .await;
    applied(&store);

    let listed = ployz(&store, address, &["domain", "ls"]).await;
    assert_eq!(
        listed.pointer("/domains/0/status"),
        Some(&json!("ready")),
        "{listed}"
    );
    assert_eq!(
        listed.pointer("/domains/0/reason"),
        Some(&Value::Null),
        "{listed}"
    );
    let checked = ployz(&store, address, &["domain", "check", HOST]).await;
    assert_eq!(
        checked.pointer("/domain/status"),
        Some(&json!("ready")),
        "{checked}"
    );
    server.abort();
}
