use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use ployz::{
    build::{BuildSpec, CapturedBuild, Recipe},
    sdk::{PreparationInput, PreparedDeploy, Session},
};
use ployz_core::{DeployOutcome, RpcError};
use ployz_testkit::{Cluster, ClusterPlan};
use serde_json::{Value, json};

#[path = "build_layer3/remote.rs"]
mod remote;

#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image with Buildx"]
async fn railpack_preparation_preserves_variables_cache_and_failure_boundaries() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    fs::write(root.join("package.json"), r#"{"name":"railpack-check","version":"1.0.0","engines":{"node":"22.14.0"},"scripts":{"build":"node build.js","start":"node index.js"}}"#).unwrap();
    fs::write(root.join("build.js"), "require('fs').writeFileSync('built.json', JSON.stringify({message:process.env.MESSAGE,stamp:Date.now()}));").unwrap();
    fs::write(
        root.join("index.js"),
        "const message = require('./built.json').message; require('http').createServer((req,res) => res.end(message)).listen(3000, '0.0.0.0');",
    )
    .unwrap();
    let cluster = Cluster::create(
        ClusterPlan::new(&format!("l3-railpack-{}", std::process::id()), 1).unwrap(),
    )
    .unwrap();
    cluster.wait_ready(Duration::from_secs(120)).await.unwrap();
    cluster.initialize_first().await.unwrap();
    let session = session(&cluster).await;
    let built_content = |prepared: &PreparedDeploy| {
        let receipt = prepared.build_receipts().values().next().unwrap();
        cluster
            .machine_shell(
                0,
                &format!(
                    "docker run --rm --entrypoint cat {} /app/built.json",
                    receipt.image.reference
                ),
            )
            .unwrap()
    };
    let mut previous: Option<String> = None;
    let mut deployable = None;
    for value in ["first", "first", "changed"] {
        let prepared = prepare(&session, git_deployment("railpack", value), root)
            .await
            .unwrap();
        let receipt = prepared.build_receipts().values().next().unwrap();
        assert!(
            receipt
                .image
                .platforms
                .iter()
                .all(|p| p.starts_with("linux/"))
        );
        let content = built_content(&prepared);
        assert!(
            content.contains(&format!("\"message\":\"{value}\"")),
            "{content}"
        );
        match &previous {
            Some(previous) if value == "first" => assert_eq!(
                &content, previous,
                "unchanged inputs should reuse the build layer"
            ),
            Some(previous) => assert_ne!(&content, previous),
            None => {}
        }
        previous = Some(content);
        if let Some(stale) = deployable.replace(prepared) {
            stale.close();
        }
    }
    let prepared = deployable.unwrap();
    let outcome = prepared.confirm().unwrap().finished().await.unwrap();
    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let containers = || {
        cluster
            .machine_shell(
                0,
                "docker ps -aq --no-trunc --filter label=ployz.service.name=app",
            )
            .unwrap()
    };
    let before = containers();
    assert_eq!(before.lines().count(), 1);
    let id = before.trim();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if cluster.docker(0, &["exec", id, "node", "-e", "fetch('http://127.0.0.1:3000').then(r=>r.text()).then(t=>{if(t!=='changed')process.exit(1)})"]).is_ok() { break; }
        assert!(
            Instant::now() < deadline,
            "deployed application did not serve its built variable"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Neither a compiler failure nor a detection failure may touch the live Service.
    fs::write(
        root.join("build.js"),
        "throw new Error('expected compilation failure');",
    )
    .unwrap();
    let failed = prepare(&session, git_deployment("railpack", "changed"), root)
        .await
        .err()
        .unwrap();
    assert!(failed.message.contains("Build"), "{failed:?}");
    assert_eq!(containers(), before);
    for file in ["package.json", "index.js", "build.js"] {
        fs::remove_file(root.join(file)).unwrap();
    }
    fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
    let failed = prepare(&session, git_deployment("railpack", "changed"), root)
        .await
        .err()
        .unwrap();
    assert!(failed.message.contains("Railpack"), "{failed:?}");
    assert_eq!(containers(), before);
    session.close().await;
}

/// A Cloud session through the entry Machine's plain TCP endpoint.
async fn session(cluster: &Cluster) -> Session {
    ployz::sdk::connect_connections(
        vec![ployz::context::Connection::tcp(
            cluster.api_socket_address(0).unwrap(),
        )],
        Arc::new(ployz::connect::SystemConnector::default()),
    )
    .await
    .unwrap()
}

/// One Git Service `app` in Project `build`, as Cloud freezes it. `MESSAGE`
/// reaches the Build as a variable.
fn git_deployment(build_method: &str, message: &str) -> Value {
    json!({"projectName": "build", "snapshots": [{
        "config": {"version": 2, "privateDns": "app", "source": {
            "version": 2, "type": "git", "repository": "acme/app", "repositoryId": 42,
            "access": {"type": "public"}, "rootDir": "/", "branch": {"type": "connected", "name": "main"}
        }, "build": {"buildMethod": build_method, "dockerfilePath": "Dockerfile", "command": null},
        "healthcheck": {"type": "none"}, "restartPolicy": "on-failure"},
        "resolvedEnv": {"MESSAGE": message}
    }]})
}

/// Cloud's preparation of `deployment` with `root` as the checkout of `app`.
async fn prepare(
    session: &Session,
    deployment: Value,
    root: &Path,
) -> Result<PreparedDeploy, RpcError> {
    let name: ployz_core::ServiceName = "app".parse().unwrap();
    session
        .prepare(PreparationInput {
            deployment,
            sources: BTreeMap::from([(name.clone(), root.to_owned())]),
            source_commits: BTreeMap::from([(name, "a".repeat(40))]),
            build_receipts: BTreeMap::new(),
        })
        .unwrap()
        .finished()
        .await
}

/// A build-only capture of `root` for Service `app` tagged `image`; its
/// variables become build variables.
fn capture(root: &Path, image: &str, variables: Value, recipe: Recipe) -> CapturedBuild {
    let intent = ployz_core::config::lower_deployment(
        serde_json::from_value(json!({"projectName": "build", "snapshots": [{
            "config": {"version": 2, "privateDns": "app", "source": {
                "type": "image", "version": 1, "image": image, "credentials": {"type": "none"}
            }, "healthcheck": {"type": "none"}, "restartPolicy": "on-failure"},
            "resolvedEnv": variables
        }]}))
        .unwrap(),
    )
    .unwrap();
    ployz::build::capture(
        &intent,
        BTreeMap::from([(
            "app".parse().unwrap(),
            BuildSpec {
                context: root.to_owned(),
                recipe,
            },
        )]),
    )
    .unwrap()
}

#[path = "build_layer3/policy.rs"]
mod policy;
