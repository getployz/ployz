//! Machine-local Docker Volume observation and Ensure tests.

use std::{collections::BTreeMap, sync::atomic::Ordering};

use axum::http::Method;

use super::*;
use crate::docker::test_support::*;

#[tokio::test]
async fn duplicate_compatible_aliases_ensure_one_volume() {
    let (runtime, fake) = fake_runtime().await;
    let name = DockerVolumeName::parse("aliases").unwrap();
    let managed = spec_with_sources(vec![
        ployz_core::RawVolumeSource::Ordinary {
            name: name.clone(),
            driver: ployz_core::VolumeDriver::parse("local", BTreeMap::new()).unwrap(),
            labels: BTreeMap::new(),
        }
        .admit()
        .expect("valid volume declaration"),
        ployz_core::RawVolumeSource::Ordinary {
            name: name.clone(),
            driver: ployz_core::VolumeDriver::parse("local", BTreeMap::new()).unwrap(),
            labels: BTreeMap::new(),
        }
        .admit()
        .expect("valid volume declaration"),
    ]);

    runtime
        .ensure_mounted_volumes(&MachineId::random(), &managed)
        .await
        .unwrap();
    assert_eq!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, path)| method == Method::POST && path.ends_with("/volumes/create"))
            .count(),
        1
    );

    fake.volumes.lock().unwrap().insert(
        name.to_string(),
        serde_json::json!({"Name":name,"Driver":"anything","Mountpoint":""}),
    );
    let external = spec_with_sources(vec![
        ployz_core::RawVolumeSource::External { name: name.clone() }
            .admit()
            .expect("valid volume declaration"),
        ployz_core::RawVolumeSource::External { name }
            .admit()
            .unwrap(),
    ]);
    runtime
        .ensure_mounted_volumes(&MachineId::random(), &external)
        .await
        .unwrap();
}

#[tokio::test]
async fn storage_admission_rejects_unknown_and_stateless_before_mutation() {
    let (runtime, fake) = fake_runtime().await;
    let spec = spec_with_sources(vec![provisioned_source("guarded", 1_073_741_824)]);
    let project = ployz_core::ProjectName::parse("app").unwrap();

    assert!(matches!(
        runtime
            .create_with_admission(
                &machine(),
                container_request(
                    ployz_core::ContainerKind::ServiceContainer,
                    &project,
                    &spec,
                    std::future::ready(None),
                ),
            )
            .await,
        Err(Error::StorageUnobservable)
    ));
    assert!(matches!(
        runtime
            .create_with_admission(
                &machine(),
                container_request(
                    ployz_core::ContainerKind::ServiceContainer,
                    &project,
                    &spec,
                    std::future::ready(Some(ployz_core::MachineStorageObservation::Stateless,)),
                ),
            )
            .await,
        Err(Error::ProvisionedStorageUnsupported)
    ));
    let requests = fake.requests.lock().unwrap();
    assert!(requests.iter().all(|(method, path)| {
        !path.contains("/images/")
            && !path.contains("/volumes/")
            && !(method == Method::POST && path.contains("/containers/"))
    }));
}

#[tokio::test]
async fn inventory_reads_provisioned_details_and_keeps_healthy_siblings() {
    let (runtime, fake) = fake_runtime().await;
    let machine_id = MachineId::random();

    let inventory = runtime.list_volumes(&machine_id).await.unwrap();

    assert_eq!(
        inventory
            .volumes
            .iter()
            .map(|volume| volume.id.name.as_str())
            .collect::<Vec<_>>(),
        ["plain", "healthy"]
    );
    assert_eq!(
        inventory
            .failures
            .iter()
            .map(|failure| failure.id.name.as_str())
            .collect::<Vec<_>>(),
        ["malformed", "unavailable", "mismatched"]
    );
    let requests = fake.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|(method, path)| method == Method::GET && path.ends_with("/volumes/healthy"))
    );
    assert!(
        !requests
            .iter()
            .any(|(method, path)| method == Method::GET && path.ends_with("/volumes/plain"))
    );
}

#[tokio::test]
async fn direct_lookup_does_not_enumerate_unrelated_volumes() {
    let (runtime, fake) = fake_runtime().await;
    let machine_id = MachineId::random();

    let volume = runtime
        .inspect_volume(&machine_id, &DockerVolumeName::parse("healthy").unwrap())
        .await
        .unwrap();

    assert_eq!(volume.id.name.as_str(), "healthy");
    let requests = fake.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests.first().is_some_and(
            |(method, path)| method == Method::GET && path.ends_with("/volumes/healthy")
        )
    );
}

#[tokio::test]
async fn existence_accepts_integer_status_for_multiple_volumes() {
    let (runtime, _) = fake_runtime().await;

    ensure_volume_exists(&runtime.docker.client, "data")
        .await
        .unwrap();
    ensure_volume_exists(&runtime.docker.client, "cache")
        .await
        .unwrap();
}

#[tokio::test]
async fn existence_rejects_a_missing_volume() {
    let (runtime, _) = fake_runtime().await;

    let error = ensure_volume_exists(&runtime.docker.client, "missing")
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        Error::Docker(bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        })
    ));
}

#[tokio::test]
async fn external_volume_is_checked_for_existence_only() {
    let (runtime, fake) = fake_runtime().await;
    let source = ployz_core::RawVolumeSource::External {
        name: DockerVolumeName::parse("malformed").unwrap(),
    }
    .admit()
    .expect("valid volume declaration");

    runtime
        .ensure_volume_source(&MachineId::random(), &source)
        .await
        .unwrap();

    assert!(
        fake.requests.lock().unwrap().iter().all(|(method, path)| {
            method == Method::GET && path.ends_with("/volumes/malformed")
        })
    );
}

#[tokio::test]
async fn missing_ordinary_volume_uses_exact_declared_shape() {
    let (runtime, fake) = fake_runtime().await;
    let mut source = ployz_core::RawVolumeSource::Ordinary {
        name: DockerVolumeName::parse("ordinary").unwrap(),
        driver: ployz_core::VolumeDriver::parse(
            "example-driver",
            BTreeMap::from([("mode".into(), "safe".into())]),
        )
        .unwrap(),
        labels: BTreeMap::from([("backup".into(), "daily".into())]),
    }
    .admit()
    .expect("valid volume declaration");
    source.scope_to_project(&ployz_core::ProjectName::parse("app").unwrap());

    runtime
        .ensure_volume_source(&MachineId::random(), &source)
        .await
        .unwrap();

    let bodies = fake.request_bodies.lock().unwrap();
    let (_, request) = bodies
        .iter()
        .find(|(path, _)| path.ends_with("/volumes/create"))
        .unwrap();
    assert_eq!(
        request,
        &serde_json::json!({
            "Name":"app_ordinary",
            "Driver":"example-driver",
            "DriverOpts":{"mode":"safe"},
            "Labels":{"backup":"daily","ployz.managed":"","ployz.project.name":"app"}
        })
    );
}

#[tokio::test]
async fn normalized_local_driver_uses_no_options() {
    let (runtime, fake) = fake_runtime().await;
    let mut source = ployz_core::RawVolumeSource::Ordinary {
        name: DockerVolumeName::parse("default-driver").unwrap(),
        driver: ployz_core::VolumeDriver::parse("local", BTreeMap::new()).unwrap(),
        labels: BTreeMap::new(),
    }
    .admit()
    .expect("valid volume declaration");
    source.scope_to_project(&ployz_core::ProjectName::parse("app").unwrap());

    runtime
        .ensure_volume_source(&MachineId::random(), &source)
        .await
        .unwrap();

    let bodies = fake.request_bodies.lock().unwrap();
    let (_, request) = bodies
        .iter()
        .find(|(path, _)| path.ends_with("/volumes/create"))
        .unwrap();
    assert_eq!(request.get("Driver").unwrap(), "local");
    assert!(request.get("DriverOpts").is_none());
}

#[tokio::test]
async fn missing_provisioned_volume_uses_ployz_driver_bound_and_labels() {
    let (runtime, fake) = fake_runtime().await;
    let source = provisioned_source("bounded", 2_147_483_648);

    runtime
        .ensure_volume_source(&MachineId::random(), &source)
        .await
        .unwrap();

    let bodies = fake.request_bodies.lock().unwrap();
    let (_, request) = bodies
        .iter()
        .find(|(path, _)| path.ends_with("/volumes/create"))
        .unwrap();
    assert_eq!(
        request,
        &serde_json::json!({
            "Name":"app_bounded",
            "Driver":"ployz",
            "DriverOpts":{"size":"2147483648b"},
            "Labels":{"backup":"daily","ployz.managed":"","ployz.project.name":"app"}
        })
    );
}

#[tokio::test]
async fn existing_managed_volume_allows_extra_labels() {
    let (runtime, fake) = fake_runtime().await;
    fake.volumes.lock().unwrap().insert(
        "app_ordinary".into(),
        serde_json::json!({
            "Name":"app_ordinary",
            "Driver":"example-driver",
            "Mountpoint":"/volumes/app_ordinary",
            "Options":{"mode":"safe"},
            "Labels":{"backup":"daily","unrelated":"kept","ployz.managed":"","ployz.project.name":"app"}
        }),
    );

    runtime
        .ensure_volume_source(&MachineId::random(), &ordinary_source("ordinary"))
        .await
        .unwrap();

    assert!(fake.requests.lock().unwrap().iter().all(|(method, path)| {
        method == Method::GET && path.ends_with("/volumes/app_ordinary")
    }));
}

#[tokio::test]
async fn existing_managed_volume_refuses_every_unsafe_shape_mismatch() {
    let (runtime, fake) = fake_runtime().await;
    let cases = [
        (
            ordinary_source("wrong-driver"),
            serde_json::json!({
                "Name":"app_wrong-driver","Driver":"local","Mountpoint":"/volumes/app_wrong-driver",
                "Options":{"mode":"safe"},"Labels":{"backup":"daily","ployz.managed":"","ployz.project.name":"app"}
            }),
        ),
        (
            ordinary_source("wrong-options"),
            serde_json::json!({
                "Name":"app_wrong-options","Driver":"example-driver","Mountpoint":"/volumes/app_wrong-options",
                "Options":{"mode":"unsafe"},"Labels":{"backup":"daily","ployz.managed":"","ployz.project.name":"app"}
            }),
        ),
        (
            ordinary_source("wrong-label"),
            serde_json::json!({
                "Name":"app_wrong-label","Driver":"example-driver","Mountpoint":"/volumes/app_wrong-label",
                "Options":{"mode":"safe"},"Labels":{"backup":"never","ployz.managed":"","ployz.project.name":"app"}
            }),
        ),
        (
            provisioned_source("wrong-kind", 2_147_483_648),
            serde_json::json!({
                "Name":"app_wrong-kind","Driver":"local","Mountpoint":"/volumes/app_wrong-kind",
                "Options":{"size":"2147483648b"},"Labels":{"backup":"daily","ployz.managed":"","ployz.project.name":"app"}
            }),
        ),
        (
            provisioned_source("wrong-maximum", 2_147_483_648),
            serde_json::json!({
                "Name":"app_wrong-maximum","Driver":"ployz","Mountpoint":"/volumes/app_wrong-maximum",
                "Options":{"size":"2147483648b"},"Labels":{"backup":"daily","ployz.managed":"","ployz.project.name":"app"},
                "Status":{"bound_bytes":1073741824,"used_bytes":0}
            }),
        ),
    ];
    for (source, observed) in cases {
        let name = source.docker_volume_name().unwrap().to_string();
        fake.volumes.lock().unwrap().insert(name, observed);
        assert!(matches!(
            runtime
                .ensure_volume_source(&MachineId::random(), &source)
                .await,
            Err(Error::VolumeShapeMismatch { .. })
        ));
    }
    assert!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .all(|(method, _)| { method != Method::POST && method != Method::DELETE })
    );
}

#[tokio::test]
async fn missing_external_volume_fails_without_create() {
    let (runtime, fake) = fake_runtime().await;
    let source = ployz_core::RawVolumeSource::External {
        name: DockerVolumeName::parse("external-missing").unwrap(),
    }
    .admit()
    .expect("valid volume declaration");

    assert!(matches!(
        runtime.ensure_volume_source(&MachineId::random(), &source).await,
        Err(Error::ExternalVolumeNotFound(name)) if name.as_str() == "external-missing"
    ));
    assert!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .all(|(method, _)| { method != Method::POST && method != Method::DELETE })
    );
}

#[tokio::test]
async fn created_but_unverified_error_keeps_identity_and_retry_inspects_without_recreate() {
    let (runtime, fake) = fake_runtime().await;
    fake.fail_after_create
        .lock()
        .unwrap()
        .insert("app_retry-volume".into());
    let machine_id = MachineId::random();
    let source = ordinary_source("retry-volume");

    let error = runtime
        .ensure_volume_source(&machine_id, &source)
        .await
        .unwrap_err();
    let Error::VolumeCreatedButUnverified { id, .. } = &error else {
        panic!("expected created-but-unverified error, got {error}")
    };
    assert_eq!(id.machine_id, machine_id);
    assert_eq!(id.name.as_str(), "app_retry-volume");
    let rpc = ployz_core::RpcError::from(&error);
    assert_eq!(rpc.code, ployz_core::RpcErrorCode::Unavailable);
    assert_eq!(
        rpc.details
            .get("created_volume")
            .and_then(|created| created.get("name"))
            .unwrap(),
        "app_retry-volume"
    );

    runtime
        .ensure_volume_source(&machine_id, &source)
        .await
        .unwrap();
    assert_eq!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, path)| {
                method == Method::POST && path.ends_with("/volumes/create")
            })
            .count(),
        1
    );
}

#[tokio::test]
async fn a_later_volume_failure_does_not_roll_back_an_earlier_create() {
    let (runtime, fake) = fake_runtime().await;
    fake.volumes.lock().unwrap().insert(
        "app_z-mismatch".into(),
        serde_json::json!({
            "Name":"app_z-mismatch","Driver":"local","Mountpoint":"/volumes/app_z-mismatch"
        }),
    );
    let spec = spec_with_sources(vec![
        ployz_core::RawVolumeSource::Ordinary {
            name: DockerVolumeName::parse("a-created").unwrap(),
            driver: ployz_core::VolumeDriver::parse("local", BTreeMap::new()).unwrap(),
            labels: BTreeMap::new(),
        }
        .admit()
        .expect("valid volume declaration"),
        ordinary_source("z-mismatch"),
    ]);

    assert!(matches!(
        runtime
            .ensure_mounted_volumes(&MachineId::random(), &spec)
            .await,
        Err(Error::VolumeShapeMismatch { .. })
    ));
    assert!(fake.volumes.lock().unwrap().contains_key("app_a-created"));
    assert!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .all(|(method, _)| { method != Method::DELETE })
    );
}

#[tokio::test]
async fn a_volume_created_before_container_creation_failure_is_left_for_retry() {
    let (runtime, fake) = fake_runtime().await;
    let spec = spec_with_sources(vec![
        ployz_core::RawVolumeSource::Ordinary {
            name: DockerVolumeName::parse("created-before-container-failure").unwrap(),
            driver: ployz_core::VolumeDriver::parse("local", BTreeMap::new()).unwrap(),
            labels: BTreeMap::new(),
        }
        .admit()
        .expect("valid volume declaration"),
    ]);
    let project = ployz_core::ProjectName::parse("app").unwrap();

    assert!(
        runtime
            .create_with_admission(
                &machine(),
                container_request(
                    ployz_core::ContainerKind::ServiceContainer,
                    &project,
                    &spec,
                    std::future::ready(None),
                ),
            )
            .await
            .is_err()
    );
    assert!(
        fake.volumes
            .lock()
            .unwrap()
            .contains_key("app_created-before-container-failure")
    );
    assert!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .all(|(method, _)| { method != Method::DELETE })
    );
    let requests = fake.requests.lock().unwrap();
    let image = requests
        .iter()
        .position(|(method, path)| method == Method::GET && path.contains("/images/"))
        .unwrap();
    let volume = requests
        .iter()
        .position(|(method, path)| method == Method::POST && path.ends_with("/volumes/create"))
        .unwrap();
    let container = requests
        .iter()
        .position(|(method, path)| method == Method::POST && path.contains("/containers/"))
        .unwrap();
    assert!(volume < image && image < container);
}

#[tokio::test]
async fn inspect_rejects_a_mismatched_volume_identity() {
    let (runtime, _) = fake_runtime().await;
    let machine_id = MachineId::random();

    let error = runtime
        .inspect_volume(&machine_id, &DockerVolumeName::parse("mismatched").unwrap())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        Error::UnexpectedVolumeName { requested, actual }
            if requested.as_str() == "mismatched" && actual == "other"
    ));
}

#[tokio::test]
async fn list_reports_a_mismatched_detail_under_the_requested_name() {
    let (runtime, _) = fake_runtime().await;
    let machine_id = MachineId::random();

    let inventory = runtime.list_volumes(&machine_id).await.unwrap();

    let failure = inventory
        .failures
        .iter()
        .find(|failure| failure.id.name.as_str() == "mismatched")
        .expect("mismatched detail is retained as a named failure");
    assert_eq!(failure.id.machine_id, machine_id);
    assert!(failure.error.message.contains("returned Volume 'other'"));
}

#[tokio::test]
async fn list_returns_a_top_level_collection_error() {
    let (runtime, fake) = fake_runtime().await;
    fake.reject_list.store(true, Ordering::Relaxed);

    let error = runtime
        .list_volumes(&MachineId::random())
        .await
        .unwrap_err();

    assert!(error.to_string().contains("collection unavailable"));
}

#[tokio::test]
async fn create_reports_mutation_success_separately_from_failed_verification() {
    let (runtime, fake) = fake_runtime().await;
    let machine_id = MachineId::random();

    let report = runtime
        .create_volume(
            &machine_id,
            CreateVolumeRequest {
                name: DockerVolumeName::parse("unavailable").unwrap(),
                driver: "ployz".into(),
                options: Default::default(),
                labels: Default::default(),
            },
        )
        .await
        .unwrap();

    let CreateVolumeReport::Unverified { id, error } = report else {
        panic!("expected created-but-unverified report")
    };
    assert_eq!(id.machine_id, machine_id);
    assert_eq!(id.name.as_str(), "unavailable");
    assert!(error.message.contains("detail unavailable"), "{error}");
    let requests = fake.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|(method, path)| method == Method::POST && path.ends_with("/volumes/create"))
    );
    assert!(
        requests
            .iter()
            .any(|(method, path)| method == Method::GET && path.ends_with("/volumes/unavailable"))
    );
}

#[tokio::test]
async fn create_returns_docker_rejection_as_an_error() {
    let (runtime, _) = fake_runtime().await;

    let error = runtime
        .create_volume(
            &MachineId::random(),
            CreateVolumeRequest {
                name: DockerVolumeName::parse("rejected").unwrap(),
                driver: "ployz".into(),
                options: Default::default(),
                labels: Default::default(),
            },
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("create rejected"));
}

#[test]
fn docker_volume_preserves_provisioned_usage_at_alert_threshold() {
    let contents = serde_json::json!({"Volumes":[{
        "Name":"data",
        "Driver":"ployz",
        "Mountpoint":"/var/lib/ployz-volumes/data",
        "Status":{"bound_bytes":1073741824,"used_bytes":966367642},
        "Options":{"size":"1g"}
    }]})
    .to_string();
    let mut volumes = decode_volume_list(Err(bollard::errors::Error::JsonDataError {
        message: "generated Volume status cannot represent numeric values".into(),
        contents,
        column: 0,
    }))
    .unwrap();
    let observed = docker_volume(&MachineId::random(), volumes.remove(0)).unwrap();

    assert_eq!(
        observed.storage,
        DockerVolumeStorageObservation::Provisioned {
            mountpoint: ployz_core::MachinePath::parse("/var/lib/ployz-volumes/data").unwrap(),
            bound_bytes: std::num::NonZeroU64::new(1_073_741_824).unwrap(),
            used_bytes: 966_367_642,
        }
    );
}

#[test]
fn provisioned_volume_without_complete_plugin_evidence_is_an_error() {
    let error = docker_volume(
        &MachineId::random(),
        serde_json::from_value(serde_json::json!({
            "Name":"data",
            "Driver":"ployz",
            "Mountpoint":"/var/lib/ployz-volumes/data"
        }))
        .unwrap(),
    )
    .unwrap_err();

    assert!(error.to_string().contains("Provisioned Volume status"));
}

#[test]
fn provisioned_volume_rejects_a_relative_mountpoint() {
    let error = docker_volume(
        &MachineId::random(),
        serde_json::from_value(serde_json::json!({
            "Name":"data",
            "Driver":"ployz",
            "Mountpoint":"relative/data",
            "Status":{"bound_bytes":1073741824,"used_bytes":0}
        }))
        .unwrap(),
    )
    .unwrap_err();

    assert!(error.to_string().contains("invalid mountpoint"));
}
