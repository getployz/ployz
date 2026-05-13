use super::*;

#[tokio::test]
async fn preview_rejects_duplicate_hostname_in_final_plan() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![
        http_route_service_spec("api", "api.example.com"),
        http_route_service_spec("web", "API.EXAMPLE.COM."),
    ]);

    let error = preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect_err("duplicate hostname should fail preview");

    assert_eq!(
        error,
        Error::Deploy(DeployError::HostnameDeclaredByMultipleServices {
            hostname: "api.example.com".into(),
            first_namespace: "test".into(),
            first_service: "api".into(),
            second_namespace: "test".into(),
            second_service: "web".into()
        })
    );
}

#[tokio::test]
async fn preview_rejects_hostname_owned_by_another_namespace() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    seed_committed_http_release(&store, "prod", "api", "api.example.com").await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![http_route_service_spec("web", "api.example.com")]);

    let error = preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect_err("cross-namespace hostname conflict should fail preview");

    assert_eq!(
        error,
        Error::Deploy(DeployError::HostnameAlreadyOwned {
            hostname: "api.example.com".into(),
            owner_namespace: "prod".into(),
            owner_service: "api".into(),
            request_namespace: "test".into(),
            request_service: "web".into()
        })
    );
}

#[tokio::test]
async fn apply_rejects_hostname_owned_by_another_namespace_before_commit() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    seed_committed_http_release(&store, "prod", "api", "api.example.com").await;
    backend.reset_counts();
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![http_route_service_spec("web", "api.example.com")]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController::default());

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("cross-namespace hostname conflict should fail apply");

    assert_eq!(
        error,
        Error::Deploy(DeployError::HostnameAlreadyOwned {
            hostname: "api.example.com".into(),
            owner_namespace: "prod".into(),
            owner_service: "api".into(),
            request_namespace: "test".into(),
            request_service: "web".into()
        })
    );
    assert_eq!(backend.commit_count(), 0);
}

#[tokio::test]
async fn preview_allows_hostname_reuse_within_same_namespace() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    seed_committed_http_release(&store, "test", "api", "api.example.com").await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![http_route_service_spec("api", "api.example.com")]);

    preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect("same-namespace replacement should be valid");
}

#[tokio::test]
async fn preview_allows_hostname_move_within_same_namespace() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    seed_committed_http_release(&store, "test", "api", "api.example.com").await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![http_route_service_spec("web", "api.example.com")]);

    preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect("same-namespace ownership move should be valid");
}
