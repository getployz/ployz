//! A user Service goes from authoring to running, and from a staged removal to gone,
//! on a real Cluster through the hidden SQLite Config Store, with the CLI as the
//! Deployment's runner.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed JSON results use indexing; missing entries must fail the test."
)]

#[path = "support/big_stack.rs"]
mod big_stack;

use ployz::connect::Call;
use ployz_store::testing::backend::PinAs;
use std::{path::Path, process::Command, sync::Arc, time::Duration};

use ployz::{
    connect::{SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections},
};
use ployz_testkit::{Cluster, ClusterPlan, SERVICE_CONTAINER_IMAGE, poll_until};
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
    // A local Store holds no Cluster key on this computer, so it takes no secret
    // (config_cli's a_local_store_refuses_a_plain_text_secret); a plain variable
    // still reaches the containers.
    ployz(&["set", "web.env.GREETING=hi"]);
    let deployed = ployz(&["deploy", "--events", events.to_str().unwrap()]);
    assert_eq!(deployed["status"], json!("applied"), "{deployed}");
    assert_eq!(deployed["nodes"][0]["outcome"], json!("deployed"));
    assert!(deployed["preview"].is_object());
    let progress = std::fs::read_to_string(&events).unwrap();
    assert!(progress.lines().count() > 0);
    for line in progress.lines() {
        serde_json::from_str::<Value>(line).unwrap();
    }
    assert_eq!(ployz(&["diff"])["changes"], json!([]));

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
            == Some(&"hi".into())
    }));

    // A Volume mounted into web is created by its Deploy; removing it deletes its
    // Docker Volume only once the loss is accepted by name. A Volume takes one writer.
    ployz(&["set", "web.replicas=1"]);
    let added = ployz(&["volume", "add", "data", "--mount", "web:/data", "--docker"]);
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
    let (code, refused) = attempt(address, &store, &["deploy", "--message", "drop data"]);
    assert_eq!(code, Some(2), "{refused}");
    let refused = &refused["error"];
    assert_eq!(refused["code"], json!("confirmation_required"), "{refused}");
    assert_eq!(refused["details"]["accept"], json!(["data"]));
    let version = refused["details"]["version"].as_str().unwrap();
    // The retry keeps what was typed besides the acceptance, with free text left to fill in.
    assert_eq!(
        refused["details"]["retry"],
        json!(format!(
            "ployz --connect tcp://{address} --json --why '<REASON>' deploy --message '<TEXT>' --accept-volume-loss data --expect-version {version}"
        ))
    );
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
    assert_eq!(web_containers(&live), 1);
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
    let cache = staging(&[
        "volume",
        "add",
        "cache",
        "--mount",
        "api:/cache",
        "--docker",
    ]);
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
    assert_eq!(code, Some(2), "{refused}");
    // The typed name confirms it: the retry accepts the lost Volume, no version.
    assert_eq!(
        refused["error"]["details"]["retry"],
        json!(format!(
            "ployz --connect tcp://{address} --json --why '<REASON>' env rm staging --confirm shop/staging --accept-volume-loss cache"
        ))
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
    assert_eq!(removed["removal"], json!("removed"), "{removed}");
    assert_eq!(
        removed["deployments"][0]["remove"],
        json!(true),
        "{removed}"
    );
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
    // The typed name alone removes deployed Services.
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
        .call::<ployz_core::op::ListMachines>(ployz_core::ListMachinesRequest {}, Call::read())
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
    poll_until(
        Duration::from_secs(30),
        &format!("{count} web Containers"),
        async || {
            client
                .live_services(ployz_core::EnvironmentValues::Included)
                .await
                .is_ok_and(|live| web_containers(&live) == count)
        },
    )
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
    std::fs::copy(
        source.path().join("Dockerfile"),
        source.path().join("Dockerfile.alt"),
    )
    .unwrap();
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
    // A variable changes at runtime only: the image still serves.
    ployz(&["set", "app.env.MESSAGE=changed"]);
    assert_eq!(ployz(&["deploy"])["status"], json!("applied"));
    // A changed build input needs a new upload.
    ployz(&["set", "app.dockerfilePath=Dockerfile.alt"]);
    let (code, refused) = attempt(address, &store, &["deploy"]);
    assert_eq!(code, Some(1), "{refused}");
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
#[test]
#[ignore = "informing: requires the privileged Ployz testkit image with Buildx"]
fn cloud_s_runner_builds_git_services_and_a_retry_rebuilds_only_what_failed() {
    big_stack::run(builds_git_services_then_retries);
}

async fn builds_git_services_then_retries() {
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
                region: None,
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
                        prune: false,
                        no_prune: false,
                        versions: Vec::new(),
                        copies: Vec::new(),
                        recreate: Vec::new(),
                        rule: None,
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
            store
                .pin_as(&id, &pins, &Trusted::default(), &format!("cloud-{n}"))
                .unwrap();
            ployz::sdk::run_deployment(
                ployz_core::Operator::Cloud,
                Arc::clone(&store),
                id.clone(),
                RunnerId::parse(format!("cloud-{n}")).unwrap(),
                vec![Connection::tcp(address)],
                Ok(ployz::sdk::Sources {
                    checkouts,
                    upload: None,
                    reader: None,
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
            (
                ployz_core::ServiceName::parse("api").unwrap(),
                BuildStatus::Failed
            ),
            (
                ployz_core::ServiceName::parse("web").unwrap(),
                BuildStatus::Built
            ),
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
            (
                ployz_core::ServiceName::parse("api").unwrap(),
                BuildStatus::Built
            ),
            (
                ployz_core::ServiceName::parse("web").unwrap(),
                BuildStatus::Reused
            ),
        ])
    );
}

/// Five Services of one repository, as a monorepo's web, workers and cron are: four build
/// one Dockerfile into one image, one digest under four repositories, and the fifth builds
/// a Dockerfile of its own. Cloud's runner builds each once, a redeploy of the same commit
/// reuses every image, and only a changed build variable or commit builds again.
#[test]
#[ignore = "informing: requires the privileged Ployz testkit image with Buildx"]
fn a_same_commit_redeploy_of_services_sharing_a_dockerfile_reuses_every_build() {
    big_stack::run(|| redeploys_sharing_a_dockerfile(false));
}

/// The same, each Service reaching a Volume, so each builds a Full Build: its image loads
/// under its own name, not under no name as a free Build's does.
#[test]
#[ignore = "informing: requires the privileged Ployz testkit image with Buildx"]
fn a_same_commit_redeploy_of_data_services_sharing_a_dockerfile_reuses_every_build() {
    big_stack::run(|| redeploys_sharing_a_dockerfile(true));
}

async fn redeploys_sharing_a_dockerfile(reaches_data: bool) {
    use std::collections::BTreeMap;

    use ployz_core::ServiceName;
    use ployz_core::config::ServiceGitAccess;
    use ployz_store::{
        Actor, Admit, AuthorizedRepository, BuildStatus, Change, ConfigStore, CreateGitService,
        CreateProject, Deploy, DeploymentId, DeploymentStatus, Edit, EnvironmentId, EnvironmentRef,
        OrganizationId, ProjectId, ProjectName, RunnerId, SealingKey, ServiceLineageId,
        SettingPath, Trusted,
    };

    let plan = ClusterPlan::new(
        &format!("l3-store-reuse-{}-{reaches_data}", std::process::id()),
        1,
    )
    .unwrap();
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
                region: None,
            },
        )
        .unwrap();
    let names = ["queue", "web", "reverb", "scheduler", "ai"];
    let repository = ployz_store::RepositoryName::parse("acme/shop").unwrap();
    let evidence = Trusted {
        repositories: vec![AuthorizedRepository {
            repository: repository.clone(),
            repository_id: ployz_store::RepositoryId::parse(11).unwrap(),
            access: ServiceGitAccess::Public,
            default_branch: ployz_store::BranchName::parse("main").unwrap(),
            branches: Vec::new(),
        }],
        ..Trusted::default()
    };
    let checkout = tempfile::tempdir().unwrap();
    // No step writes a layer, so the four builds give one image whatever their timing, as
    // the Services sharing a Dockerfile in production ran one.
    std::fs::write(
        checkout.path().join("Dockerfile"),
        "FROM alpine:3.23.3\nCMD [\"sleep\", \"600\"]\n",
    )
    .unwrap();
    std::fs::write(
        checkout.path().join("ai.Dockerfile"),
        "FROM alpine:3.23.3\nRUN sleep 2 && echo ai > /role\nCMD [\"sleep\", \"600\"]\n",
    )
    .unwrap();
    for (n, name) in names.iter().enumerate() {
        store
            .write_trusted(
                &who,
                &CreateGitService {
                    id: ServiceLineageId::parse(format!("00000000-0000-4000-8000-00000000001{n}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(*name).unwrap(),
                    repository: repository.clone(),
                    branch: None,
                },
                &evidence,
            )
            .unwrap();
    }
    if reaches_data {
        for (n, name) in names.iter().enumerate() {
            store
                .write(
                    &who,
                    &ployz_store::CreateVolume {
                        restored_from: None,
                        shared_writes: false,
                        storage_locked: false,
                        storage: ployz_core::config::VolumeKind::Docker {},
                        id: ployz_store::VolumeId::parse(format!(
                            "00000000-0000-4000-8000-00000000003{n}"
                        ))
                        .unwrap(),
                        environment: EnvironmentRef::default(),
                        name: ployz_store::VolumeName::parse(format!("{name}-data")).unwrap(),
                        mounts: vec![ployz_store::Mount {
                            service: ServiceName::parse(*name).unwrap(),
                            path: "/data".into(),
                        }],
                    },
                )
                .unwrap();
        }
    }
    let edit = |changes: Vec<(String, String)>| {
        store
            .write(
                &who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: changes
                        .into_iter()
                        .map(|(path, value)| Change::Set {
                            path: SettingPath::parse(&path).unwrap(),
                            value: json!(value),
                        })
                        .collect(),
                },
            )
            .unwrap();
    };
    edit(
        names
            .iter()
            .flat_map(|name| {
                [
                    (format!("{name}.buildMethod"), "dockerfile".to_owned()),
                    (format!("{name}.env.ROLE"), (*name).to_owned()),
                ]
            })
            .chain([("ai.dockerfilePath".to_owned(), "ai.Dockerfile".to_owned())])
            .collect(),
    );
    let checkouts: BTreeMap<_, _> = names
        .iter()
        .map(|name| {
            (
                ServiceName::parse(*name).unwrap(),
                checkout.path().to_owned(),
            )
        })
        .collect();

    let (proxy, rpcs) = counting_builds(address).await;
    let deploy = |n: u8, commit: String| {
        let store = Arc::clone(&store);
        let (who, checkouts) = (who.clone(), checkouts.clone());
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
                        upload: None,
                        accept_volume_loss: Vec::new(),
                        message: None,
                        prune: false,
                        no_prune: false,
                        versions: Vec::new(),
                        copies: Vec::new(),
                        recreate: Vec::new(),
                        rule: None,
                    }),
                    &Trusted::default(),
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
            store
                .pin_as(&id, &pins, &Trusted::default(), &format!("cloud-{n}"))
                .unwrap();
            let started = std::time::Instant::now();
            ployz::sdk::run_deployment(
                ployz_core::Operator::Cloud,
                Arc::clone(&store),
                id.clone(),
                RunnerId::parse(format!("cloud-{n}")).unwrap(),
                vec![Connection::tcp(proxy)],
                Ok(ployz::sdk::Sources {
                    checkouts,
                    upload: None,
                    reader: None,
                }),
            )
            .await
            .unwrap();
            let view = store
                .read(&who, &ployz_store::DeploymentQuery { id: id.clone() })
                .unwrap();
            let build_phase = view
                .builds
                .iter()
                .filter_map(|build| build.finished_at)
                .max()
                .zip(
                    view.builds
                        .iter()
                        .filter_map(|build| build.started_at)
                        .min(),
                )
                .map(|(finished, started)| finished - started);
            eprintln!(
                "deployment {n}: build phase {build_phase:?}s, run {:.1}s\n  builds {:?}\n  nodes {:?}",
                started.elapsed().as_secs_f64(),
                view.builds
                    .iter()
                    .map(|build| (build.service.to_string(), build.status))
                    .collect::<Vec<_>>(),
                view.nodes
                    .iter()
                    .map(|node| (&node.node, node.outcome))
                    .collect::<Vec<_>>(),
            );
            assert_eq!(
                view.deployment.status,
                DeploymentStatus::Applied,
                "{view:?}"
            );
            view
        }
    };
    // Every Build and BuildFree the Server receives, while the Deployment builds and while
    // it prepares: a rebuild that lands on the old digest still counts, though the Store
    // labels it Reused.
    let sent = || rpcs.swap(0, std::sync::atomic::Ordering::SeqCst);

    // The image each of the fixture's Services runs on the Machine, read from the containers
    // Ployz manages in the Environment's Namespace and found by their Service label.
    let running = || {
        use ployz_core::labels::{MANAGED, NAMESPACE, SERVICE_NAME};
        let listed = cluster
            .machine_shell(
                0,
                &format!(
                    "set -e; ids=$(docker ps -q --filter label={NAMESPACE}=shop-production --filter label={MANAGED}); \
                     [ -n \"$ids\" ]; \
                     docker inspect --format '{{{{index .Config.Labels \"{SERVICE_NAME}\"}}}} {{{{.Image}}}}' $ids"
                ),
            )
            .unwrap();
        service_images(&listed, &names)
    };
    let reused = || {
        names
            .iter()
            .map(|name| ((*name).to_owned(), BuildStatus::Reused))
            .collect::<BTreeMap<_, _>>()
    };
    let statuses = |view: &ployz_store::DeploymentView| {
        view.builds
            .iter()
            .map(|build| (build.service.to_string(), build.status))
            .collect::<BTreeMap<_, _>>()
    };

    let first = deploy(1, "c".repeat(40)).await;
    let images = running();
    eprintln!("images after the rollout: {images:?}");
    assert!(
        ["web", "reverb", "scheduler"]
            .iter()
            .all(|name| images[*name] == images["queue"])
            && images["ai"] != images["queue"],
        "the four Services sharing a Dockerfile run one image, and ai its own: {images:?}"
    );
    // A container named like a Service's, but not one Ployz runs for it, counts for none.
    cluster
        .machine_shell(
            0,
            &format!(
                "docker run -d --network none --name queue-abcdefabcdef {SERVICE_CONTAINER_IMAGE} sleep 600"
            ),
        )
        .unwrap();
    assert_eq!(
        running(),
        images,
        "a lookalike container counts for no Service"
    );
    cluster
        .machine_shell(0, "docker rm -f queue-abcdefabcdef")
        .unwrap();
    let rollout = sent();
    assert!(
        rollout >= 5,
        "the first Deployment builds every Service: {rollout} {first:?}"
    );
    for n in [2, 3] {
        let again = deploy(n, "c".repeat(40)).await;
        assert_eq!(
            sent(),
            0,
            "a same-commit redeploy must send no Build to a Server: {again:?}"
        );
        assert_eq!(statuses(&again), reused(), "{again:?}");
        assert_eq!(
            running(),
            images,
            "a same-commit redeploy runs the same images"
        );
    }
    edit(vec![("web.env.ROLE".into(), "changed".into())]);
    let changed = deploy(4, "c".repeat(40)).await;
    let rebuilt = sent();
    assert!(
        0 < rebuilt && rebuilt < rollout,
        "{rebuilt} of {rollout}: {changed:?}"
    );
    assert_eq!(
        changed
            .builds
            .iter()
            .filter(|build| build.status != BuildStatus::Reused)
            .map(|build| build.service.to_string())
            .collect::<Vec<_>>(),
        ["web"],
        "{changed:?}"
    );
    let moved = deploy(5, "d".repeat(40)).await;
    assert!(
        sent() >= 5,
        "a new commit builds every Service again: {moved:?}"
    );
}

/// The one image each of `services` runs, from `{service label} {image}` lines for the
/// managed containers of one Namespace. Every container belongs to one of `services`, each
/// Service runs exactly one, and each runs an image ID.
fn service_images(listed: &str, services: &[&str]) -> std::collections::BTreeMap<String, String> {
    let mut running = std::collections::BTreeMap::<String, Vec<String>>::new();
    for line in listed.lines() {
        let (service, image) = line.split_once(' ').unwrap_or((line, ""));
        assert!(
            services.contains(&service),
            "a container in the Namespace runs no Service of the fixture: {listed}"
        );
        let digest = image.strip_prefix("sha256:").unwrap_or_default();
        assert!(
            digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit()),
            "{service} runs no image ID: {listed}"
        );
        running
            .entry(service.to_owned())
            .or_default()
            .push(image.to_owned());
    }
    let mut expected = services.to_vec();
    expected.sort_unstable();
    assert_eq!(
        running.keys().map(String::as_str).collect::<Vec<_>>(),
        expected,
        "{listed}"
    );
    running
        .into_iter()
        .map(|(service, images)| match images.as_slice() {
            [image] => (service, image.clone()),
            _ => panic!("{service} runs one container, not {images:?}"),
        })
        .collect()
}

/// Listens for the runner's connection and forwards each of its requests to the Server at
/// `to`, counting the Build and BuildFree calls among them.
async fn counting_builds(
    to: std::net::SocketAddr,
) -> (std::net::SocketAddr, Arc<std::sync::atomic::AtomicUsize>) {
    use hyper_util::rt::{TokioExecutor, TokioIo};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    tokio::spawn(async move {
        loop {
            let (inbound, _) = listener.accept().await.unwrap();
            let calls = Arc::clone(&counted);
            tokio::spawn(async move {
                let outbound = tokio::net::TcpStream::connect(to).await.unwrap();
                let (server, connection) = hyper::client::conn::http2::handshake(
                    TokioExecutor::new(),
                    TokioIo::new(outbound),
                )
                .await
                .unwrap();
                tokio::spawn(connection);
                let forward = hyper::service::service_fn(
                    move |request: hyper::Request<hyper::body::Incoming>| {
                        if matches!(
                            request.uri().path().rsplit('/').next(),
                            Some("Build" | "BuildFree")
                        ) {
                            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                        let mut server = server.clone();
                        async move { server.send_request(request).await }
                    },
                );
                let _closed = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(inbound), forward)
                    .await;
            });
        }
    });
    (address, calls)
}

/// Cloud's runner builds an uploaded Service on a Server, never GitHub. Once Cloud no
/// longer holds the upload, a later Deployment reuses the image while its build inputs
/// hold, and otherwise records that it needs a new upload.
#[test]
#[ignore = "informing: requires the privileged Ployz testkit image with Buildx"]
fn cloud_s_runner_builds_an_upload_then_reuses_it_or_asks_for_a_new_one() {
    big_stack::run(builds_an_upload_then_reuses_it);
}

async fn builds_an_upload_then_reuses_it() {
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
                region: None,
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
                template: None,
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
    std::fs::copy(
        source.path().join("Dockerfile"),
        source.path().join("Dockerfile.alt"),
    )
    .unwrap();
    let digest =
        ployz_core::UploadDigest::parse(ployz::build::content_digest(source.path()).unwrap())
            .unwrap();

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
                        prune: false,
                        no_prune: false,
                        versions: Vec::new(),
                        copies: Vec::new(),
                        recreate: Vec::new(),
                        rule: None,
                    }),
                    &ployz_store::Trusted::default(),
                )
                .unwrap();
            ployz::sdk::run_deployment(
                ployz_core::Operator::Cloud,
                Arc::clone(&store),
                id.clone(),
                RunnerId::parse(format!("cloud-{n}")).unwrap(),
                vec![Connection::tcp(address)],
                Ok(ployz::sdk::Sources {
                    checkouts: std::collections::BTreeMap::new(),
                    upload: dir,
                    reader: None,
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

    // A variable changes at runtime only: the image still serves.
    set("app.env.MESSAGE", json!("changed"));
    let varied = deploy(3, false).await;
    assert_eq!(
        varied.deployment.status,
        DeploymentStatus::Applied,
        "{varied:?}"
    );
    assert_eq!(varied.builds[0].status, BuildStatus::Reused);
    // A changed build input rejects the old image: only a new upload builds it.
    set("app.dockerfilePath", json!("Dockerfile.alt"));
    let refused = deploy(4, false).await;
    assert_eq!(
        refused.deployment.status,
        DeploymentStatus::Failed,
        "{refused:?}"
    );
    let Some(Outcome::NotExecuted { needs_upload, .. }) = refused.deployment.outcome else {
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

/// Run `ployz --json ARGS` against `store` and the Cluster at `address`, with a Ployz
/// config beside `store`, so no device key of this computer's signs.
fn run(address: std::net::SocketAddr, store: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--why=test")
        .args(args)
        .args(["--json", "--connect", &format!("tcp://{address}")])
        .env("PLOYZ_CONFIG", store.with_file_name("config.yaml"))
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
