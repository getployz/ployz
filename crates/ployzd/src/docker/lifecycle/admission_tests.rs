//! Service admission tests at the fake-Docker boundary.

use axum::http::Method;

use super::*;
use crate::docker::test_support::*;

#[tokio::test]
async fn rejected_admission_does_not_poll_deferred_local_admission() {
    let (runtime, fake) = fake_runtime().await;
    let machine = machine();
    let project = ProjectName::parse("app").unwrap();
    let mut ineligible = spec_with_sources(Vec::new());
    ineligible.placement = ployz_core::Placement {
        constraints: ["node.labels.target==other".parse().unwrap()].into(),
    };

    let ordinary = runtime
        .create_with_admission(
            &machine,
            ContainerRequest {
                creation_key: None,
                kind: ContainerKind::ServiceContainer,
                project_name: &project,
                spec: &ineligible,
                admission: async { Err(Error::EndpointCapacity) },
                storage: std::future::ready(None),
            },
        )
        .await;
    assert!(matches!(ordinary, Err(Error::ServicePlacementMismatch)));

    let unknown = spec_with_sources(vec![provisioned_source("bounded", 1_073_741_824)]);
    let mut request = container_request(
        ContainerKind::ServiceContainer,
        &project,
        &unknown,
        std::future::ready(None),
    );
    request.creation_key = Some("unknown-storage");
    assert!(matches!(
        runtime.create_with_admission(&machine, request).await,
        Err(Error::StorageUnobservable)
    ));
    assert!(fake.requests.lock().unwrap().iter().all(|(method, path)| {
        !path.contains("/images/")
            && !path.contains("/volumes/")
            && !(method == Method::POST && path.contains("/containers/"))
            && !path.ends_with("/start")
            && method != Method::DELETE
    }));
}

#[tokio::test]
async fn service_and_hook_creation_reach_the_same_volume_ensure() {
    let (runtime, fake) = fake_runtime().await;
    fake.volumes.lock().unwrap().insert(
        "app_unsafe".into(),
        serde_json::json!({
            "Name":"app_unsafe","Driver":"local","Mountpoint":"/volumes/app_unsafe"
        }),
    );
    let spec = spec_with_sources(vec![ordinary_source("unsafe")]);
    let machine = machine();
    let project = ProjectName::parse("app").unwrap();

    for kind in [
        ContainerKind::ServiceContainer,
        ContainerKind::PreDeployHook,
    ] {
        assert!(matches!(
            runtime
                .create_with_admission(
                    &machine,
                    container_request(kind, &project, &spec, std::future::ready(None)),
                )
                .await,
            Err(Error::VolumeShapeMismatch { .. })
        ));
    }
    assert!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .all(|(method, path)| { !(method == Method::POST && path.contains("/containers/")) })
    );
}
