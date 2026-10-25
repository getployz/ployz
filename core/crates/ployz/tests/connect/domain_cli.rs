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

/// How the CLI finds the Standalone Cluster's Server.
#[derive(Clone, Copy)]
enum Reach {
    /// `--connect`.
    Flag,
    /// The current context, on a device also signed in to Cloud.
    SignedInContext,
}

/// `ployz --json ARGS` reaching the Server at `address` as `reach` says, with its own
/// home and the in-process Store in `store`.
async fn ployz(store: &Path, address: std::net::SocketAddr, reach: Reach, args: &[&str]) -> Value {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.yaml");
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"));
    command
        .current_dir(home.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env("PLOYZ_CONFIG", &config)
        .env("PLOYZ_STORE", format!("sqlite:{}", store.display()))
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .env_remove("PLOYZ_PROJECT")
        .env_remove("PLOYZ_ENV")
        .arg("--json");
    match reach {
        Reach::Flag => {
            command.arg("--connect").arg(format!("tcp://{address}"));
        }
        Reach::SignedInContext => {
            std::fs::write(
                &config,
                format!(
                    "current_context: lab\ncontexts:\n  lab:\n    connections:\n      - tcp: {address}\n        machine_id: 0123456789abcdef0123456789abcdef\n        name: machine-1\n"
                ),
            )
            .unwrap();
            command.env("PLOYZ_TOKEN", "ployz_pat_not_this_cluster");
        }
    }
    let output = command.args(args).output().await.unwrap();
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
        no_prune: false,
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

/// An ingress Server with a public IP and an issued certificate for [`HOST`], and a Store whose deployed
/// Service `web` has that domain.
async fn live_domain() -> (
    tempfile::TempDir,
    std::net::SocketAddr,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let service = DiscoveryService::new(test_description());
    let mut frame = empty_watch_frame();
    let mut ingress = machine('a', "one");
    ingress.machine.public_ip = Some("203.0.113.7".parse().unwrap());
    frame.machines.push(ingress);
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
    for args in [
        &["project", "new", "shop"][..],
        &["service", "add", "web", "--image", "nginx:1"],
        &["domain", "add", "web", HOST, "--port", "80"],
    ] {
        ployz(&store, address, Reach::Flag, args).await;
    }
    applied(&store);
    (dir, address, server)
}

#[tokio::test]
async fn a_live_domain_reads_ready_without_a_cloud() {
    let (dir, address, server) = live_domain().await;
    let store = dir.path().join("store.db");
    let listed = ployz(&store, address, Reach::Flag, &["domain", "ls"]).await;
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
    let checked = ployz(&store, address, Reach::Flag, &["domain", "check", HOST]).await;
    assert_eq!(
        checked.pointer("/domain/status"),
        Some(&json!("ready")),
        "{checked}"
    );
    server.abort();
}

/// A device signed in to Cloud still reads its Standalone Cluster through the current
/// context, never the Cloud Organization's Servers.
#[tokio::test]
async fn a_signed_in_device_reads_its_standalone_cluster() {
    let (dir, address, server) = live_domain().await;
    let store = dir.path().join("store.db");
    let checked = ployz(
        &store,
        address,
        Reach::SignedInContext,
        &["domain", "check", HOST],
    )
    .await;
    assert_eq!(
        checked.pointer("/domain/status"),
        Some(&json!("ready")),
        "{checked}"
    );
    server.abort();
}
