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
    assert_eq!(deployed["nodes"][0]["outcome"], json!("deployed"));
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

    // A Volume mounted into web is created by its Deploy; removing it deletes its
    // Docker Volume only once the loss is accepted by name.
    let added = ployz(&["volume", "add", "data", "--mount", "web:/data"]);
    let docker = format!(
        "shop-production_vol-{}",
        added["volume"]["id"].as_str().unwrap()
    );
    assert_eq!(ployz(&["deploy"])["status"], json!("applied"));
    assert!(
        held(&mut client, &docker).await,
        "the Deploy created the Volume"
    );
    ployz(&["volume", "rm", "data"]);
    let (code, refused) = attempt(address, &store, &["deploy"]);
    assert_eq!(code, Some(1), "{refused}");
    let refused = &refused["error"];
    assert_eq!(refused["code"], json!("confirmation_required"), "{refused}");
    assert_eq!(refused["details"]["accept"], json!(["data"]));
    let version = refused["details"]["version"].as_str().unwrap();
    assert!(
        held(&mut client, &docker).await,
        "a refusal deletes nothing"
    );
    let accepted = ployz(&[
        "deploy",
        "--expect-version",
        version,
        "--accept-volume-loss",
        "data",
    ]);
    assert_eq!(accepted["status"], json!("applied"), "{accepted}");
    assert!(
        !held(&mut client, &docker).await,
        "the accepted Docker Volume is gone"
    );
    assert_eq!(ployz(&["volume", "ls"])["volumes"], json!([]));

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

    // Removing an Environment takes what it runs off the Servers, deleting each
    // Volume accepted by name, and only then deletes it.
    ployz(&["env", "new", "staging"]);
    let staging = |args: &[&str]| {
        let mut args = args.to_vec();
        args.extend(["--env", "staging"]);
        ployz(&args)
    };
    staging(&["service", "add", "api", "--image", SERVICE_CONTAINER_IMAGE]);
    staging(&["set", "api.startCommand=sh -c 'sleep 600'"]);
    let cache = staging(&["volume", "add", "cache", "--mount", "api:/cache"]);
    let cache = format!(
        "shop-staging_vol-{}",
        cache["volume"]["id"].as_str().unwrap()
    );
    assert_eq!(staging(&["deploy"])["status"], json!("applied"));
    let api_containers = |live: &ployz_core::LiveServices<ployz_core::RpcError>| {
        live.services()
            .iter()
            .find(|service| service.has_name("api"))
            .map_or(0, |service| service.containers.len())
    };
    wait_for_web(&mut client, &api_containers, 1).await;
    let (code, refused) = attempt(
        address,
        &store,
        &["env", "rm", "staging", "--confirm", "shop/staging"],
    );
    assert_eq!(code, Some(1), "{refused}");
    assert_eq!(
        refused["error"]["details"]["next"],
        json!("ployz env rm staging --confirm shop/staging --accept-volume-loss cache")
    );
    let removed = ployz(&[
        "env",
        "rm",
        "staging",
        "--confirm",
        "shop/staging",
        "--accept-volume-loss",
        "cache",
    ]);
    assert_eq!(removed["deployment"]["remove"], json!(true), "{removed}");
    wait_for_web(&mut client, &api_containers, 0).await;
    assert!(
        !held(&mut client, &cache).await,
        "the accepted Volume is gone"
    );
    assert_eq!(
        ployz(&["env", "ls"])["environments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Removing the Project takes each Environment off the Servers, a Branch before
    // its Parent and the Default Environment last, then deletes it all.
    ployz(&["service", "add", "api", "--image", SERVICE_CONTAINER_IMAGE]);
    ployz(&["set", "api.startCommand=sh -c 'sleep 600'"]);
    assert_eq!(ployz(&["deploy"])["status"], json!("applied"));
    ployz(&["env", "branch", "fix", "--copy", "api"]);
    assert_eq!(
        ployz(&["deploy", "--env", "fix"])["status"],
        json!("applied")
    );
    let removed = ployz(&["project", "rm", "shop", "--confirm", "shop"]);
    assert_eq!(
        removed["environments"],
        json!(["fix", "production"]),
        "{removed}"
    );
    assert_eq!(removed["deployments"].as_array().unwrap().len(), 2);
    wait_for_web(&mut client, &api_containers, 0).await;
    assert_eq!(ployz(&["project", "ls"])["projects"], json!([]));
}

/// Whether a Server holds Docker Volume `name`.
async fn held(client: &mut ployz::connect::Client, name: &str) -> bool {
    let machines = client
        .call::<ployz_core::op::ListMachines>(ployz_core::ListMachinesRequest {}, None)
        .await
        .unwrap()
        .machines;
    client
        .list_volumes(&machines)
        .await
        .successes
        .iter()
        .flat_map(|success| &success.value.volumes)
        .any(|volume| volume.id.name.as_str() == name)
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
    assert_eq!(
        refused["next"],
        json!("ployz up --project shop --env production")
    );
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
        CreateGitService, CreateProject, Deploy, DeploymentId, DeploymentStatus, Edit,
        EnvironmentId, EnvironmentRef, OrganizationId, ProjectId, ProjectName, RunnerId,
        SealingKey, ServiceLineageId, SettingPath, Trusted,
    };

    let plan = ClusterPlan::new(&format!("l3-store-git-{}", std::process::id()), 1).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.initialize_entry().await.unwrap();
    let address = cluster.api_socket_address(0).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}", dir.path().join("store.db").display());
    let store = Arc::new(ConfigStore::open(&url, SealingKey::new(b"rung4").unwrap()).unwrap());
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
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
                repository: ployz_store::RepositoryName::parse(format!("acme/{name}")).unwrap(),
                repository_id: ployz_store::RepositoryId::parse(repository_id).unwrap(),
                access: ServiceGitAccess::Public,
                default_branch: ployz_store::BranchName::parse("main").unwrap(),
                branches: Vec::new(),
            })
            .collect(),
        ..Trusted::default()
    };
    let mut checkouts = BTreeMap::new();
    let sources = tempfile::tempdir().unwrap();
    for (n, name) in names.iter().enumerate() {
        store
            .write_trusted(
                &who,
                &CreateGitService {
                    id: ServiceLineageId::parse(format!("00000000-0000-4000-8000-00000000001{n}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(*name).unwrap(),
                    repository: ployz_store::RepositoryName::parse(format!("acme/{name}")).unwrap(),
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
        .write(
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
                .write_trusted(
                    &who,
                    &Admit::Deploy(Deploy {
                        id: id.clone(),
                        environment: EnvironmentRef::default(),
                        services: Vec::new(),
                        version: None,
                        upload: None,
                        accept_volume_loss: Vec::new(),
                        message: None,
                    }),
                    &ployz_store::Trusted::default(),
                )
                .unwrap();
            let pins = names
                .iter()
                .map(|name| {
                    (
                        ServiceName::parse(*name).unwrap(),
                        ployz_store::CommitSha::parse(commit.as_str()).unwrap(),
                    )
                })
                .collect();
            store.pin(&id, &pins).unwrap();
            ployz::sdk::run_deployment(
                Arc::clone(&store),
                id.clone(),
                RunnerId::parse(format!("cloud-{n}")).unwrap(),
                vec![Connection::tcp(address)],
                Ok(ployz::sdk::Sources {
                    checkouts,
                    upload: None,
                }),
            )
            .await
            .unwrap();
            store
                .read(&who, &ployz_store::DeploymentQuery { id: id.clone() })
                .unwrap()
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
        .read(
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

/// Cloud's runner builds an uploaded Service on a Server, never GitHub. Once Cloud no
/// longer holds the upload, a later Deployment reuses the image while its build inputs
/// hold, and otherwise records that it needs a new upload.
#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image with Buildx"]
async fn cloud_s_runner_builds_an_upload_then_reuses_it_or_asks_for_a_new_one() {
    use ployz_core::ServiceName;
    use ployz_store::{
        Actor, Admit, BuildStatus, Change, ConfigStore, CreateProject, CreateService, Deploy,
        DeploymentId, DeploymentStatus, Edit, EnvironmentId, EnvironmentRef, OrganizationId,
        Outcome, ProjectId, ProjectName, RunnerId, SealingKey, ServiceLineageId, SettingPath,
        UploadedSource,
    };

    let plan =
        ClusterPlan::new(&format!("l3-store-cloud-upload-{}", std::process::id()), 1).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.initialize_entry().await.unwrap();
    let address = cluster.api_socket_address(0).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}", dir.path().join("store.db").display());
    let store = Arc::new(ConfigStore::open(&url, SealingKey::new(b"rung4").unwrap()).unwrap());
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000010").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("app").unwrap(),
                image: None,
            },
        )
        .unwrap();
    let set = |setting: &str, value: Value| {
        store
            .write(
                &who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: vec![Change::Set {
                        path: SettingPath::parse(setting).unwrap(),
                        value,
                    }],
                },
            )
            .unwrap();
    };
    set("app.buildMethod", json!("dockerfile"));
    let source = tempfile::tempdir().unwrap();
    std::fs::write(
        source.path().join("Dockerfile"),
        "FROM alpine:3.23.3\nCOPY payload /payload\nCMD [\"sleep\", \"600\"]\n",
    )
    .unwrap();
    std::fs::write(source.path().join("payload"), "uploaded").unwrap();
    let digest = ployz::build::content_digest(source.path()).unwrap();

    let deploy = |n: u8, upload: bool| {
        let store = Arc::clone(&store);
        let who = who.clone();
        let dir = upload.then(|| source.path().to_owned());
        let digest = digest.clone();
        async move {
            let id =
                DeploymentId::parse(format!("00000000-0000-4000-8000-0000000002{n:02}")).unwrap();
            store
                .write_trusted(
                    &who,
                    &Admit::Deploy(Deploy {
                        id: id.clone(),
                        environment: EnvironmentRef::default(),
                        services: Vec::new(),
                        version: None,
                        upload: upload.then_some(UploadedSource {
                            digest,
                            base: None,
                            uploader: None,
                        }),
                        accept_volume_loss: Vec::new(),
                        message: None,
                    }),
                    &ployz_store::Trusted::default(),
                )
                .unwrap();
            ployz::sdk::run_deployment(
                Arc::clone(&store),
                id.clone(),
                RunnerId::parse(format!("cloud-{n}")).unwrap(),
                vec![Connection::tcp(address)],
                Ok(ployz::sdk::Sources {
                    checkouts: std::collections::BTreeMap::new(),
                    upload: dir,
                }),
            )
            .await
            .unwrap();
            store
                .read(&who, &ployz_store::DeploymentQuery { id: id.clone() })
                .unwrap()
        }
    };
    let built = deploy(1, true).await;
    assert_eq!(
        built.deployment.status,
        DeploymentStatus::Applied,
        "{built:?}"
    );
    assert_eq!(built.builds.len(), 1);
    assert_eq!(built.builds[0].commit, None);
    assert_eq!(built.builds[0].status, BuildStatus::Built);
    let log = store
        .read(
            &who,
            &ployz_store::BuildLogQuery {
                deployment: built.deployment.id.clone(),
                service: ServiceName::parse("app").unwrap(),
            },
        )
        .unwrap();
    assert!(
        log.log.starts_with("GitHub can't build uploaded source"),
        "{}",
        log.log
    );
    let payload = cluster
        .machine_shell(
            0,
            "docker exec $(docker ps -q --filter label=ployz.namespace=shop-production | head -1) \
             cat /payload",
        )
        .unwrap();
    assert_eq!(payload.trim(), "uploaded");

    // The upload is gone once its Deployment ended; the image still serves.
    set("app.replicas", json!(2));
    let reused = deploy(2, false).await;
    assert_eq!(
        reused.deployment.status,
        DeploymentStatus::Applied,
        "{reused:?}"
    );
    assert_eq!(reused.builds[0].status, BuildStatus::Reused);
    assert_eq!(reused.deployment.upload, built.deployment.upload);

    // A changed build input rejects the old image: only a new upload builds it.
    set("app.env.MESSAGE", json!("changed"));
    let refused = deploy(3, false).await;
    assert_eq!(
        refused.deployment.status,
        DeploymentStatus::Failed,
        "{refused:?}"
    );
    let Some(Outcome::NotExecuted { needs_upload, .. }) = refused.outcome else {
        panic!("nothing executed: {refused:?}");
    };
    assert_eq!(needs_upload, vec![ServiceName::parse("app").unwrap()]);
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
