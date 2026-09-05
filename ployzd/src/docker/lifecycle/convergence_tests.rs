//! Service admission and Global convergence tests at the fake-Docker boundary.

use axum::http::Method;

use super::*;
use crate::docker::test_support::*;

#[test]
fn non_ensured_global_convergence_converts_to_admission_errors() {
    assert!(matches!(
        GlobalSlotConvergence::Ineligible(
            ServicePlacementIneligibleReason::ProvisionedStorageUnsupported
        )
        .into_container(),
        Err(Error::ProvisionedStorageUnsupported)
    ));
    assert!(matches!(
        GlobalSlotConvergence::Unknown(ServicePlacementUnknownReason::MissingStorageEvidence)
            .into_container(),
        Err(Error::StorageUnobservable)
    ));
}

#[tokio::test]
async fn rejected_admission_does_not_poll_deferred_network() {
    let (runtime, fake) = fake_runtime().await;
    let machine = machine();
    let project = ProjectName::parse("app").unwrap();
    let mut ineligible = spec_with_sources(Vec::new());
    ineligible.placement = ployz_core::Placement {
        machines: vec![ployz_core::MachineTarget::parse("other").unwrap()],
    };

    let ordinary = runtime
        .create_with_network(
            &machine,
            ContainerRequest {
                kind: ContainerKind::ServiceContainer,
                project_name: &project,
                spec: &ineligible,
                network: async { Err(Error::EndpointCapacity) },
                storage: std::future::ready(None),
            },
        )
        .await;
    assert!(matches!(ordinary, Err(Error::ServicePlacementMismatch)));

    let unknown = spec_with_sources(vec![provisioned_source("bounded", 1_073_741_824)]);
    let global = runtime
        .converge_global_slot(
            &machine,
            GlobalSlotRequest {
                project_name: &project,
                spec: &unknown,
                network: async { Err(Error::EndpointCapacity) },
                storage: std::future::ready(None),
            },
        )
        .await;
    assert!(matches!(
        global,
        Ok(GlobalSlotConvergence::Unknown(
            ServicePlacementUnknownReason::MissingStorageEvidence
        ))
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
async fn run_replacement_hook_and_missing_global_reach_the_same_volume_ensure() {
    let (runtime, fake) = fake_runtime().await;
    fake.volumes.lock().unwrap().insert(
        "unsafe".into(),
        serde_json::json!({
            "Name":"unsafe","Driver":"local","Mountpoint":"/volumes/unsafe"
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
                .create_with_network(
                    &machine,
                    container_request(kind, &project, &spec, std::future::ready(None)),
                )
                .await,
            Err(Error::VolumeShapeMismatch { .. })
        ));
    }
    assert!(matches!(
        runtime
            .converge_global_slot(
                &machine,
                global_slot_request(&project, &spec, std::future::ready(None)),
            )
            .await,
        Err(Error::VolumeShapeMismatch { .. })
    ));

    assert!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .all(|(method, path)| { !(method == Method::POST && path.contains("/containers/")) })
    );
}

#[tokio::test]
async fn existing_global_slot_is_verified_before_early_return_or_restart() {
    for state in ["running", "exited"] {
        let (runtime, fake) = fake_runtime().await;
        fake.volumes.lock().unwrap().insert(
            "bounded".into(),
            serde_json::json!({
                "Name":"bounded","Driver":"ployz","Mountpoint":"/volumes/bounded",
                "Options":{"size":"1073741824b"},"Labels":{"backup":"daily"},
                "Status":{"bound_bytes":1073741824,"used_bytes":0}
            }),
        );
        let spec = spec_with_sources(vec![provisioned_source("bounded", 1_073_741_824)]);
        let machine = machine();
        let project = ProjectName::parse("app").unwrap();
        install_existing_global_slot(&runtime, &fake, &spec, state).await;

        assert!(matches!(
            runtime
                .converge_global_slot(
                    &machine,
                    global_slot_request(&project, &spec, std::future::ready(None)),
                )
                .await,
            Ok(GlobalSlotConvergence::Unknown(
                ServicePlacementUnknownReason::MissingStorageEvidence
            ))
        ));
        assert!(fake.requests.lock().unwrap().iter().all(|(method, path)| {
            !(method == Method::POST && path.contains("/containers/"))
                && !path.ends_with("/start")
                && method != Method::DELETE
        }));
    }
}

#[tokio::test]
async fn observer_eligible_target_ineligible_retires_the_existing_global_slot() {
    let (runtime, fake) = fake_runtime().await;
    let spec = spec_with_sources(vec![provisioned_source("bounded", 1_073_741_824)]);
    let machine = machine();
    let project = ProjectName::parse("app").unwrap();
    install_existing_global_slot(&runtime, &fake, &spec, "running").await;
    assert!(matches!(
        spec.placement_eligibility(&machine, Some(&MachineStorageObservation::Ready)),
        ServicePlacementEligibility::Eligible
    ));

    let outcome = runtime
        .converge_global_slot(
            &machine,
            global_slot_request(
                &project,
                &spec,
                std::future::ready(Some(MachineStorageObservation::Stateless)),
            ),
        )
        .await;

    assert!(matches!(
        outcome,
        Ok(GlobalSlotConvergence::Ineligible(
            ServicePlacementIneligibleReason::ProvisionedStorageUnsupported
        ))
    ));
    assert!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .any(|(method, path)| method == Method::DELETE && path.contains("/containers/"))
    );
}

#[tokio::test]
async fn target_ineligible_retires_an_older_global_generation() {
    let (runtime, fake) = fake_runtime().await;
    let spec = spec_with_sources(vec![provisioned_source("bounded", 1_073_741_824)]);
    let mut older_spec = spec.clone();
    older_spec.service_id = ployz_core::ServiceId::random();
    assert_ne!(older_spec.service_id, spec.service_id);
    let machine = machine();
    let project = ProjectName::parse("app").unwrap();
    install_existing_global_slot(&runtime, &fake, &older_spec, "running").await;

    let outcome = runtime
        .converge_global_slot(
            &machine,
            global_slot_request(
                &project,
                &spec,
                std::future::ready(Some(MachineStorageObservation::Stateless)),
            ),
        )
        .await;

    assert!(matches!(
        outcome,
        Ok(GlobalSlotConvergence::Ineligible(
            ServicePlacementIneligibleReason::ProvisionedStorageUnsupported
        ))
    ));
    assert!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .any(|(method, path)| method == Method::DELETE && path.contains("/containers/"))
    );
}

#[tokio::test]
async fn observer_ineligible_target_eligible_ensures_and_starts_the_existing_global_slot() {
    let (runtime, fake) = fake_runtime().await;
    fake.volumes.lock().unwrap().insert(
        "bounded".into(),
        serde_json::json!({
            "Name":"bounded","Driver":"ployz","Mountpoint":"/volumes/bounded",
            "Options":{"size":"1073741824b"},"Labels":{"backup":"daily"},
            "Status":{"bound_bytes":1073741824,"used_bytes":0}
        }),
    );
    let spec = spec_with_sources(vec![provisioned_source("bounded", 1_073_741_824)]);
    let machine = machine();
    let project = ProjectName::parse("app").unwrap();
    install_existing_global_slot(&runtime, &fake, &spec, "exited").await;
    assert!(matches!(
        spec.placement_eligibility(&machine, Some(&MachineStorageObservation::Stateless)),
        ServicePlacementEligibility::Ineligible(_)
    ));

    let outcome = runtime
        .converge_global_slot(
            &machine,
            global_slot_request(
                &project,
                &spec,
                std::future::ready(Some(MachineStorageObservation::Ready)),
            ),
        )
        .await;

    assert!(matches!(outcome, Ok(GlobalSlotConvergence::Ensured(_))));
    let requests = fake.requests.lock().unwrap();
    assert!(requests.iter().any(|(method, path)| {
        method == Method::POST && path.contains("/containers/") && path.ends_with("/start")
    }));
    assert!(requests.iter().all(|(method, _)| method != Method::DELETE));
}

#[tokio::test]
async fn inspection_rejects_labels_from_a_different_service_spec() {
    let (runtime, fake) = fake_runtime().await;
    let spec = spec_with_sources(Vec::new());
    let container_id = ContainerId::parse("a".repeat(64)).unwrap();
    let machine = machine();
    install_existing_global_slot(&runtime, &fake, &spec, "future-state").await;
    let observed = runtime
        .inspect_managed(&container_id, &machine.id)
        .await
        .unwrap();
    assert_eq!(observed.identity().to_string(), "app/api");
    assert!(matches!(
        observed.runtime,
        ployz_core::ContainerRuntimeObservation::Unknown { .. }
    ));
    for (label, value) in [
        ("ployz.service.name", "web".to_owned()),
        ("ployz.service.id", "b".repeat(32)),
    ] {
        install_existing_global_slot(&runtime, &fake, &spec, "running").await;
        *fake
            .existing_container
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .pointer_mut(&format!("/Config/Labels/{label}"))
            .unwrap() = value.into();
        assert!(matches!(
            runtime.inspect_managed(&container_id, &machine.id).await,
            Err(Error::Observation(error)) if error.label == label
        ));
    }
}

async fn install_existing_global_slot(
    runtime: &ContainerRuntime,
    fake: &FakeDocker,
    spec: &ResolvedServiceSpec,
    state: &str,
) {
    let container_id = ContainerId::parse("a".repeat(64)).unwrap();
    runtime
        .specs
        .config_operation()
        .await
        .put(&container_id, spec)
        .await
        .unwrap();
    fake.existing_container
        .lock()
        .unwrap()
        .replace(serde_json::json!({
            "Id":container_id,
            "Name":"/api-existing",
            "Created":"2026-01-01T00:00:00Z",
            "Config":{"Labels":{
                "ployz.managed":"",
                "ployz.project.name":"app",
                "ployz.service.id":spec.service_id,
                "ployz.service.name":"api"
            }},
            "State":{"Status":state,"ExitCode":0}
        }));
}
