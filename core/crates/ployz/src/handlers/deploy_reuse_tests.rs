//! Rung 2: image reuse through the real SDK and owned Machine transport.
use super::*;

#[tokio::test]
async fn sdk_reuses_unchanged_git_image_when_another_service_changes() {
    use serde_json::json;
    use std::collections::BTreeMap;
    let (root, service, builds) = fixture();
    let (address, server) = listening(service).await;
    let session = crate::sdk::connect_connections(
        vec![crate::context::Connection::tcp(address)],
        Arc::new(crate::connect::SystemConnector::default()),
    )
    .await
    .unwrap();
    let mut deployment = json!({
        "projectName": "example", "snapshots": [
            {"config": {"version": 2, "privateDns": "one", "source": {
                "version": 2, "type": "git", "repository": "acme/one", "repositoryId": 42,
                "access": {"type": "public"}, "rootDir": "/", "branch": {"type": "connected", "name": "main"}
            }, "build": {"builder": "dockerfile", "dockerfilePath": "Dockerfile", "command": null},
            "healthcheck": {"type": "none"}, "restartPolicy": "on-failure"}},
            {"config": {"version": 2, "privateDns": "other", "source": {
                "version": 1, "type": "image", "image": "redis:7", "credentials": {"type": "none"}
            }, "healthcheck": {"type": "none"}, "restartPolicy": "on-failure"}}
        ]
    });
    let name: ployz_core::ServiceName = "one".parse().unwrap();
    let input = |deployment, receipts, commit| crate::sdk::PreparationInput {
        deployment,
        sources: BTreeMap::from([(name.clone(), root.clone())]),
        source_commits: BTreeMap::from([(name.clone(), commit)]),
        build_receipts: receipts,
    };
    let first = session
        .prepare(input(deployment.clone(), BTreeMap::new(), "a".repeat(40)))
        .unwrap()
        .finished()
        .await
        .unwrap();
    let receipts = first.build_receipts().clone();
    assert_eq!(receipts.len(), 1);
    let image = &receipts.get(&name).unwrap().image.reference;
    first.close();
    assert_eq!(builds.definitions.lock().unwrap().len(), 1);
    // The original builder may lose the image; a runtime peer can still serve it.
    builds
        .stores
        .lock()
        .unwrap()
        .remove(&machine('a', "builder").machine.id);
    *deployment
        .pointer_mut("/snapshots/1/config/source/image")
        .unwrap() = json!("redis:8");
    let second = session
        .prepare(input(deployment.clone(), receipts.clone(), "a".repeat(40)))
        .unwrap()
        .finished()
        .await
        .unwrap();
    assert_eq!(
        builds.definitions.lock().unwrap().len(),
        1,
        "unchanged Git source must not start another Build"
    );
    assert_eq!(
        &second.build_receipts().get(&name).unwrap().image.reference,
        image
    );
    assert!(second.preview().operations.iter().any(|row| {
        row.operation
            .spec()
            .is_some_and(|spec| spec.name.as_str() == "other" && spec.container.image == "redis:8")
    }));
    assert_eq!(
        second.build_receipts().get(&name).unwrap().machine_id,
        machine('b', "application").machine.id
    );
    second.close();

    // A missing image must fall back to a Build, even with matching inputs.
    builds.stores.lock().unwrap().clear();
    let missing = session
        .prepare(input(deployment.clone(), receipts.clone(), "a".repeat(40)))
        .unwrap()
        .finished()
        .await
        .unwrap();
    assert_eq!(builds.definitions.lock().unwrap().len(), 2);
    missing.close();
    let changed = session
        .prepare(input(deployment.clone(), receipts.clone(), "b".repeat(40)))
        .unwrap()
        .finished()
        .await
        .unwrap();
    assert_eq!(builds.definitions.lock().unwrap().len(), 3);
    changed.close();
    deployment
        .pointer_mut("/snapshots/0")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("resolvedEnv".into(), json!({"BUILD_VALUE": "changed"}));
    let changed = session
        .prepare(input(deployment.clone(), receipts, "a".repeat(40)))
        .unwrap()
        .finished()
        .await
        .unwrap();
    assert_eq!(builds.definitions.lock().unwrap().len(), 4);
    let mut uncovered = changed.build_receipts().clone();
    uncovered.get_mut(&name).unwrap().image.platforms = vec!["linux/arm64".into()];
    changed.close();
    let uncovered = session
        .prepare(input(deployment.clone(), uncovered, "a".repeat(40)))
        .unwrap()
        .finished()
        .await
        .unwrap();
    assert_eq!(
        builds.definitions.lock().unwrap().len(),
        5,
        "the reused image must cover runtime placement even without an explicit Dockerfile platform"
    );

    // A mixed preparation must build only the new Git Service and preserve the reused one.
    let mut two = deployment.pointer("/snapshots/0").unwrap().clone();
    *two.pointer_mut("/config/privateDns").unwrap() = json!("two");
    deployment
        .get_mut("snapshots")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(two);
    let mut mixed = input(
        deployment,
        uncovered.build_receipts().clone(),
        "a".repeat(40),
    );
    uncovered.close();
    mixed.sources.insert("two".parse().unwrap(), root.clone());
    mixed
        .source_commits
        .insert("two".parse().unwrap(), "a".repeat(40));
    let mixed = session.prepare(mixed).unwrap().finished().await.unwrap();
    assert_eq!(mixed.build_receipts().len(), 2);
    {
        let definitions = builds.definitions.lock().unwrap();
        assert_eq!(definitions.len(), 6);
        assert_eq!(
            definitions.last().unwrap().targets.first().unwrap().name,
            "two"
        );
    }
    mixed.close();
    session.close().await;
    server.abort();
    fs::remove_dir_all(root).unwrap();
}
