//! A user Service goes from authoring to running, and from a staged removal to gone,
//! on a real Cluster through the hidden SQLite Config Store, with the CLI as the
//! Deployment's runner.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed JSON results use indexing; missing entries must fail the test."
)]

use std::{path::Path, process::Command, sync::Arc, time::Duration};

use ployz::{
    connect::{SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections},
};
use ployz_testkit::{Cluster, ClusterPlan, SERVICE_CONTAINER_IMAGE};
use serde_json::{Value, json};

#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image"]
async fn an_image_service_deploys_through_the_hidden_store() {
    let plan = ClusterPlan::new(&format!("l3-store-deploy-{}", std::process::id()), 1).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.initialize_entry().await.unwrap();
    let address = cluster.api_socket_address(0).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store.db");
    let events = dir.path().join("events.ndjson");
    let ployz = |args: &[&str]| ployz(address, &store, args);

    ployz(&["project", "new", "shop"]);
    ployz(&["service", "add", "web", "--image", SERVICE_CONTAINER_IMAGE]);
    ployz(&["set", "web.startCommand=sh -c 'echo started; sleep 600'"]);
    // A secret is sealed in the Store and unsealed only for its runner.
    let secrets = dir.path().join("secrets.env");
    std::fs::write(&secrets, "TOKEN=s3cr3t\n").unwrap();
    ployz(&[
        "set",
        "web",
        "--from-env-file",
        secrets.to_str().unwrap(),
        "--secret",
    ]);
    ployz(&["set", "web.env.GREETING=hi-${{ TOKEN }}"]);
    let deployed = ployz(&["deploy", "--events", events.to_str().unwrap()]);
    assert_eq!(deployed["status"], json!("applied"), "{deployed}");
    assert_eq!(deployed["nodes"][0]["outcome"], json!("applied"));
    assert!(deployed["preview"].is_object());
    assert!(!deployed.to_string().contains("s3cr3t"));
    let progress = std::fs::read_to_string(&events).unwrap();
    assert!(progress.lines().count() > 0);
    for line in progress.lines() {
        serde_json::from_str::<Value>(line).unwrap();
    }
    assert_eq!(ployz(&["diff"])["changes"], json!([]));

    // A staged edit ships with a targeted deploy of the reviewed version.
    ployz(&["set", "web.replicas=2"]);
    let plan = ployz(&["deploy", "web", "--plan"]);
    let version = plan["version"].as_str().unwrap();
    let scaled = ployz(&["deploy", "web", "--expect-version", version]);
    assert_eq!(scaled["status"], json!("applied"), "{scaled}");
    assert_eq!(scaled["saved"], json!(2));
    let listed = ployz(&["deployment", "ls"]);
    assert_eq!(listed["deployments"].as_array().unwrap().len(), 2);

    let mut client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::tcp(address)],
        },
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap();
    let web_containers = |live: &ployz_core::LiveServices<ployz_core::RpcError>| {
        live.services()
            .iter()
            .find(|service| service.has_name("web"))
            .map_or(0, |service| {
                assert!(service.containers.iter().all(|container| {
                    container.as_observation().namespace.as_str() == "shop-production"
                }));
                service.containers.len()
            })
    };
    wait_for_web(&mut client, &web_containers, 2).await;

    // A Deployment's logs are those of the containers it created.
    let scaled_id = scaled["id"].as_str().unwrap();
    let logs = run(address, &store, &["logs", "--deployment", scaled_id]);
    assert!(logs.status.success(), "{logs:?}");
    assert!(
        String::from_utf8_lossy(&logs.stdout).contains("started"),
        "{logs:?}"
    );
    let live = client
        .live_services(ployz_core::EnvironmentValues::Included)
        .await
        .unwrap();
    let services = live.services();
    let web = services
        .iter()
        .find(|service| service.has_name("web"))
        .unwrap();
    assert!(web.containers.iter().all(|container| {
        container
            .as_observation()
            .resolved_spec
            .container
            .environment
            .get("GREETING")
            == Some(&"hi-s3cr3t".to_owned())
    }));

    // A staged removal leaves the running Service alone until a Deploy removes it.
    ployz(&["service", "rm", "web"]);
    let live = client
        .live_services(ployz_core::EnvironmentValues::Redacted)
        .await
        .unwrap();
    assert_eq!(web_containers(&live), 2);
    let removed = ployz(&["deploy"]);
    assert_eq!(removed["status"], json!("applied"), "{removed}");
    wait_for_web(&mut client, &web_containers, 0).await;
    let gone = run(address, &store, &["logs", "--deployment", scaled_id]);
    let gone: Value = serde_json::from_slice(&gone.stdout).unwrap();
    assert_eq!(gone["error"]["code"], json!("not_found"), "{gone}");
}

/// Wait until the Cluster runs `count` containers of `web`.
async fn wait_for_web(
    client: &mut ployz::connect::Client,
    web_containers: &impl Fn(&ployz_core::LiveServices<ployz_core::RpcError>) -> usize,
    count: usize,
) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(live) = client
                .live_services(ployz_core::EnvironmentValues::Included)
                .await
                && web_containers(&live) == count
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image with Buildx"]
async fn a_directory_without_git_builds_on_a_server_through_the_hidden_store() {
    let plan = ClusterPlan::new(&format!("l3-store-upload-{}", std::process::id()), 1).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.initialize_entry().await.unwrap();
    let address = cluster.api_socket_address(0).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store.db");
    let ployz = |args: &[&str]| ployz(address, &store, args);
    let source = tempfile::tempdir().unwrap();
    std::fs::write(
        source.path().join("Dockerfile"),
        "FROM alpine:3.23.3\nCOPY payload /payload\nCMD [\"sleep\", \"600\"]\n",
    )
    .unwrap();
    std::fs::write(source.path().join("payload"), "uploaded").unwrap();
    let upload = source.path().to_str().unwrap();

    ployz(&["project", "new", "shop"]);
    ployz(&["service", "add", "app"]);
    ployz(&["set", "app.buildMethod=dockerfile"]);
    let deployed = ployz(&["deploy", "--upload", upload]);
    assert_eq!(deployed["status"], json!("applied"), "{deployed}");
    assert_eq!(deployed["upload"]["base"], Value::Null);
    let payload = cluster
        .machine_shell(
            0,
            "docker exec $(docker ps -q --filter label=ployz.namespace=shop-production | head -1) \
             cat /payload",
        )
        .unwrap();
    assert_eq!(payload.trim(), "uploaded");

    // Without the upload, a later Deployment reuses the image while its build inputs hold.
    ployz(&["set", "app.replicas=2"]);
    let reused = ployz(&["deploy"]);
    assert_eq!(reused["status"], json!("applied"), "{reused}");
    assert_eq!(reused["upload"], deployed["upload"]);
    // A changed build input needs a new upload.
    ployz(&["set", "app.env.MESSAGE=changed"]);
    let (code, refused) = attempt(address, &store, &["deploy"]);
    assert_eq!(code, Some(3), "{refused}");
    assert_eq!(refused["outcome"]["type"], json!("not_executed"));
    assert_eq!(refused["next"], json!("ployz deploy --upload ."));
    let rebuilt = ployz(&["deploy", "--upload", upload]);
    assert_eq!(rebuilt["status"], json!("applied"), "{rebuilt}");
}

/// Cloud's runner builds each Git Service from its checkout at its pinned commit, all
/// at once, before preparing. A failed build fails the Deployment but keeps the others'
/// receipts, so a retry rebuilds only what failed.
#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image with Buildx"]
async fn cloud_s_runner_builds_git_services_and_a_retry_rebuilds_only_what_failed() {
    use std::collections::BTreeMap;

    use ployz_core::ServiceName;
    use ployz_core::config::ServiceGitAccess;
    use ployz_store::{
        Actor, Admit, AuthorizedRepository, BuildLogQuery, BuildStatus, Change, ConfigStore,
        CreateGitService, CreateProject, DeploymentId, DeploymentStatus, Edit, EnvironmentId,
        EnvironmentRef, OrganizationId, ProjectId, ProjectName, RunnerId, SealingKey, ServiceId,
        SettingPath, Trusted,
    };

    let plan = ClusterPlan::new(&format!("l3-store-git-{}", std::process::id()), 1).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.initialize_entry().await.unwrap();
    let address = cluster.api_socket_address(0).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}", dir.path().join("store.db").display());
    let store = Arc::new(ConfigStore::open(&url, SealingKey::new(b"rung4").unwrap()).unwrap());
    let who = Actor {
        organization: OrganizationId::parse("org").unwrap(),
    };
    store
        .create_project(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
            },
        )
        .unwrap();
    let names = ["web", "api"];
    let evidence = Trusted {
        repositories: names
            .iter()
            .zip(11..)
            .map(|(name, repository_id)| AuthorizedRepository {
                repository: format!("acme/{name}"),
                repository_id,
                access: ServiceGitAccess::Public,
                default_branch: "main".into(),
                branches: Vec::new(),
            })
            .collect(),
        ..Trusted::default()
    };
    let mut checkouts = BTreeMap::new();
    let sources = tempfile::tempdir().unwrap();
    for (n, name) in names.iter().enumerate() {
        store
            .create_git_service(
                &who,
                &CreateGitService {
                    id: ServiceId::parse(format!("00000000-0000-4000-8000-00000000001{n}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(*name).unwrap(),
                    repository: format!("acme/{name}"),
                    branch: None,
                },
                &evidence,
            )
            .unwrap();
        let checkout = sources.path().join(name);
        std::fs::create_dir(&checkout).unwrap();
        checkouts.insert(ServiceName::parse(*name).unwrap(), checkout);
    }
    let dockerfile = |name: &str, run: &str| {
        std::fs::write(
            sources.path().join(name).join("Dockerfile"),
            format!("FROM alpine:3.23.3\nRUN {run}\nCMD [\"sleep\", \"600\"]\n"),
        )
        .unwrap();
    };
    dockerfile("web", "echo built-web");
    dockerfile("api", "echo broken-api && exit 1");
    store
        .edit(
            &who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: names
                    .iter()
                    .map(|name| Change::Set {
                        path: SettingPath::parse(&format!("{name}.buildMethod")).unwrap(),
                        value: json!("dockerfile"),
                    })
                    .collect(),
            },
        )
        .unwrap();

    let commit = "c".repeat(40);
    let deploy = |n: u8| {
        let store = Arc::clone(&store);
        let (who, checkouts, commit) = (who.clone(), checkouts.clone(), commit.clone());
        async move {
            let id =
                DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap();
            store
                .admit(
                    &who,
                    &Admit {
                        id: id.clone(),
                        environment: EnvironmentRef::default(),
                        services: Vec::new(),
                        version: None,
                        upload: None,
                        retry: None,
                    },
                )
                .unwrap();
            let pins = names
                .iter()
                .map(|name| (ServiceName::parse(*name).unwrap(), commit.clone()))
                .collect();
            store.pin(&id, &pins).unwrap();
            ployz::sdk::run_deployment(
                Arc::clone(&store),
                who.clone(),
                id.clone(),
                RunnerId::parse(format!("cloud-{n}")).unwrap(),
                vec![Connection::tcp(address)],
                Ok(checkouts),
            )
            .await
            .unwrap();
            store.deployment(&who, &id).unwrap()
        }
    };
    let statuses = |view: &ployz_store::DeploymentView| {
        view.builds
            .iter()
            .map(|build| (build.service.clone(), build.status))
            .collect::<BTreeMap<_, _>>()
    };

    let failed = deploy(1).await;
    assert_eq!(
        failed.deployment.status,
        DeploymentStatus::Failed,
        "{failed:?}"
    );
    assert_eq!(
        statuses(&failed),
        BTreeMap::from([
            ("api".to_owned(), BuildStatus::Failed),
            ("web".to_owned(), BuildStatus::Built),
        ])
    );
    let log = store
        .build_log(
            &who,
            &BuildLogQuery {
                deployment: failed.deployment.id.clone(),
                service: ServiceName::parse("api").unwrap(),
            },
        )
        .unwrap();
    assert!(log.log.contains("broken-api"), "{}", log.log);

    dockerfile("api", "echo fixed-api");
    let retried = deploy(2).await;
    assert_eq!(
        retried.deployment.status,
        DeploymentStatus::Applied,
        "{retried:?}"
    );
    assert_eq!(
        statuses(&retried),
        BTreeMap::from([
            ("api".to_owned(), BuildStatus::Built),
            ("web".to_owned(), BuildStatus::Reused),
        ])
    );
}

/// [`run`], with its exit code and JSON (null when stdout isn't one).
fn attempt(address: std::net::SocketAddr, store: &Path, args: &[&str]) -> (Option<i32>, Value) {
    let output = run(address, store, args);
    let json = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    (output.status.code(), json)
}

/// Run `ployz --json ARGS` against `store` and the Cluster at `address`.
fn run(address: std::net::SocketAddr, store: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(args)
        .args(["--json", "--connect", &format!("tcp://{address}")])
        .env("PLOYZ_STORE", format!("sqlite:{}", store.display()))
        .env_remove("PLOYZ_PROJECT")
        .env_remove("PLOYZ_ENV")
        .output()
        .unwrap()
}

/// [`run`], which must succeed.
fn ployz(address: std::net::SocketAddr, store: &Path, args: &[&str]) -> Value {
    let output = run(address, store, args);
    assert!(
        output.status.success(),
        "{args:?}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
