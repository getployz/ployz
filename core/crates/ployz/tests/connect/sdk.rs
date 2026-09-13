//! Façade tests for Cloud session connect / about / preview / run / close.

use std::time::Duration;

use ployz::deploy::{DeployIntent, PlanOptions};
use ployz_core::{
    CapabilityName, ContractDescription, DESCRIBE_CONTRACT_CAPABILITY, DeployOperation,
    DeployOutcome, ExecutionError, FailedOperation, MachineAction, MachineId, PROTOCOL_MAJOR,
    ProjectName, RequestedServiceSpec, RpcError, RpcErrorCode,
};
use tokio::time::timeout;

use super::support::DiscoveryService;
use super::unix_session::{self, UnixSession};

#[tokio::test]
async fn connect_about_returns_contract_and_branches_on_capability_names() {
    let description = advertised_description();
    let session = UnixSession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;

    let client = timeout(
        Duration::from_secs(5),
        unix_session::connect(&session.directory, description.machine_id.as_str()),
    )
    .await
    .expect("connect must not hang")
    .unwrap();

    let about = client.about().await.unwrap();
    assert_eq!(about.machine_id, description.machine_id);
    assert!(
        about.supports(DESCRIBE_CONTRACT_CAPABILITY),
        "callers branch on capability names, not daemon_version"
    );
    assert_eq!(about.daemon_version, description.daemon_version);
}

#[tokio::test]
async fn close_drops_the_session_and_repeated_lifecycle_works() {
    let description = advertised_description();
    let session = UnixSession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    let machine_id = description.machine_id.as_str();

    for _ in 0..3 {
        let client = unix_session::connect(&session.directory, machine_id)
            .await
            .unwrap();
        assert!(
            client
                .about()
                .await
                .unwrap()
                .supports(DESCRIBE_CONTRACT_CAPABILITY)
        );
        client.close().await;
        let closed = match client.about().await {
            Ok(_) => panic!("about() after close must fail"),
            Err(error) => error,
        };
        assert_eq!(closed.code, RpcErrorCode::Unavailable);
        client.close().await;
    }
}

#[tokio::test]
async fn deploy_returns_success_for_a_completed_run() {
    let description = advertised_description();
    let session = UnixSession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    let client = unix_session::connect(&session.directory, description.machine_id.as_str())
        .await
        .unwrap();

    let outcome = client
        .run(
            DeployIntent::apply_one(
                ProjectName::parse("app").unwrap(),
                spec("web"),
                skip_health(),
            ),
            None,
        )
        .await
        .unwrap();

    let DeployOutcome::Success { completed } = outcome else {
        panic!("expected success: {outcome:?}");
    };
    assert_eq!(completed.len(), 1);
    assert!(matches!(
        completed.first(),
        Some(DeployOperation::RunContainer { spec, skip_health_monitor: true, .. })
            if spec.name.as_str() == "web"
    ));
    assert!(
        client
            .about()
            .await
            .unwrap()
            .supports(DESCRIBE_CONTRACT_CAPABILITY)
    );
}

#[tokio::test]
async fn deploy_reports_volume_ensure_as_the_container_operation_failure() {
    let description = advertised_description();
    let session = UnixSession::start().await;
    let mut service = DiscoveryService::new(description.clone());
    service.create_container_error = Some(RpcError {
        code: RpcErrorCode::Unavailable,
        message: "Volume Ensure failed".into(),
        details: serde_json::Value::Null,
    });
    let _machine = session.spawn_machine(description.machine_id, service).await;
    let client = unix_session::connect(&session.directory, description.machine_id.as_str())
        .await
        .unwrap();
    let outcome = client
        .run(
            DeployIntent::apply_one(
                ProjectName::parse("app").unwrap(),
                spec_with_volume("web", "scratch"),
                skip_health(),
            ),
            None,
        )
        .await
        .unwrap();

    let DeployOutcome::Failed {
        completed,
        failed,
        unexecuted,
    } = outcome
    else {
        panic!("expected partial failure: {outcome:?}");
    };
    assert!(completed.is_empty());
    assert!(matches!(
        failed,
        FailedOperation::Operation {
            operation: DeployOperation::RunContainer { spec, .. },
            error: ExecutionError::Machine {
                action: MachineAction::CreateContainer,
                ..
            },
        } if spec.name.as_str() == "web"
    ));
    assert!(unexecuted.is_empty());
    assert!(
        client
            .about()
            .await
            .unwrap()
            .supports(DESCRIBE_CONTRACT_CAPABILITY)
    );
}

#[tokio::test]
async fn deploy_planning_error_is_a_typed_rpc_error() {
    let description = advertised_description();
    let session = UnixSession::start().await;
    let _machine = session
        .spawn_machine(description.machine_id, {
            let mut service = DiscoveryService::new(description.clone());
            service.machines.clear();
            service
        })
        .await;
    let client = unix_session::connect(&session.directory, description.machine_id.as_str())
        .await
        .unwrap();

    let error = client
        .run(
            DeployIntent::apply_one(
                ProjectName::parse("app").unwrap(),
                spec("web"),
                skip_health(),
            ),
            None,
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert!(
        client
            .about()
            .await
            .unwrap()
            .supports(DESCRIBE_CONTRACT_CAPABILITY)
    );
}

#[tokio::test]
async fn preview_planning_error_is_a_typed_rpc_error() {
    let description = advertised_description();
    let session = UnixSession::start().await;
    let _machine = session
        .spawn_machine(description.machine_id, {
            let mut service = DiscoveryService::new(description.clone());
            service.machines.clear();
            service
        })
        .await;
    let client = unix_session::connect(&session.directory, description.machine_id.as_str())
        .await
        .unwrap();

    let error = client
        .preview(DeployIntent::apply_one(
            ProjectName::parse("app").unwrap(),
            spec("web"),
            skip_health(),
        ))
        .await
        .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert!(
        client
            .about()
            .await
            .unwrap()
            .supports(DESCRIBE_CONTRACT_CAPABILITY)
    );
}

#[tokio::test]
async fn preview_project_removal_reserved_is_a_typed_rpc_error() {
    let description = advertised_description();
    let session = UnixSession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    let client = unix_session::connect(&session.directory, description.machine_id.as_str())
        .await
        .unwrap();

    let error = client
        .preview_project_removal(ProjectName::system(), ployz::deploy::VolumeFate::Preserve)
        .await
        .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        error.message,
        "Project 'ployz-system' is reserved for Ployz infrastructure"
    );
}

#[tokio::test]
async fn preview_then_confirm_executes_the_shown_plan() {
    let description = advertised_description();
    let session = UnixSession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    let client = unix_session::connect(&session.directory, description.machine_id.as_str())
        .await
        .unwrap();
    let intent = DeployIntent::apply_one(
        ProjectName::parse("app").unwrap(),
        spec("web"),
        skip_health(),
    );

    let preview = client.preview(intent).await.unwrap();
    assert_eq!(preview.operations.len(), 1);
    assert!(matches!(
        preview.operations.first().map(|row| &row.operation),
        Some(DeployOperation::RunContainer { spec, skip_health_monitor: true, .. })
            if spec.name.as_str() == "web"
    ));

    let running = preview.confirm().unwrap();
    let outcome = running.finished().await.unwrap();
    let DeployOutcome::Success { completed } = outcome else {
        panic!("expected success: {outcome:?}");
    };
    assert_eq!(completed.len(), 1);
    assert!(
        client
            .about()
            .await
            .unwrap()
            .supports(DESCRIBE_CONTRACT_CAPABILITY)
    );
    assert!(preview.confirm().is_err(), "second confirm is illegal");
}

#[tokio::test]
async fn confirm_after_close_fails_closed() {
    let description = advertised_description();
    let session = UnixSession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    let client = unix_session::connect(&session.directory, description.machine_id.as_str())
        .await
        .unwrap();
    let preview = client
        .preview(DeployIntent::apply_one(
            ProjectName::parse("app").unwrap(),
            spec("web"),
            skip_health(),
        ))
        .await
        .unwrap();
    client.close().await;
    let Err(error) = preview.confirm() else {
        panic!("confirm after close must fail closed");
    };
    assert_eq!(error.code, RpcErrorCode::Unavailable);
}

#[tokio::test]
async fn node_smoke_covers_connect_about_preview_run_and_close() {
    let description = advertised_description();
    let session = UnixSession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    session
        .assert_sdk_script(
            "node_smoke.js",
            description.machine_id,
            &[("PLOYZ_UNKNOWN_MACHINE_ID", MachineId::random().as_str())],
        )
        .await;
}

pub(super) fn advertised_description() -> ContractDescription {
    ContractDescription {
        machine_id: MachineId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "do-not-branch-on-me".into(),
        capabilities: [CapabilityName::parse(DESCRIBE_CONTRACT_CAPABILITY)
            .expect("catalogued capability names are valid")]
        .into(),
    }
}
fn spec(name: &str) -> RequestedServiceSpec {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "always" }
    }))
    .unwrap()
}

fn spec_with_volume(name: &str, volume: &str) -> RequestedServiceSpec {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "always" },
        "volumes": [{
            "reference": volume,
            "source": {
                "kind": "ordinary",
                "name": volume,
                "driver": {"name": "local", "options": {}}
            }
        }],
        "mounts": [{ "volume": volume, "target": format!("/{volume}") }]
    }))
    .unwrap()
}

fn skip_health() -> PlanOptions {
    PlanOptions {
        skip_health_monitor: true,
        ..PlanOptions::default()
    }
}

#[tokio::test]
async fn sdk_storage_shortage_preserves_numbers_and_actions_without_mutating() {
    let mut description = advertised_description();
    description
        .capabilities
        .insert(CapabilityName::parse(ployz_core::MACHINE_STORAGE_OBSERVATION_CAPABILITY).unwrap());
    let session = UnixSession::start().await;
    let mut service = DiscoveryService::new(description.clone());
    service.storage_capacity = Some(ployz_core::StorageCapacity {
        backing: ployz_core::StorageBacking::Unallocated {
            host_total_bytes: 100 * ployz_core::STORAGE_GIB,
            host_available_bytes: 90 * ployz_core::STORAGE_GIB,
        },
        unmanaged_used_bytes: 0,
        volumes: Default::default(),
    });
    let created = service.created_volumes.clone();
    let target_id = service.machines.first().unwrap().machine.id;
    let _machine = session.spawn_machine(description.machine_id, service).await;
    let client = unix_session::connect(&session.directory, description.machine_id.as_str())
        .await
        .unwrap();
    let services = ["data", "server"].map(|name| {
        let mut value = serde_json::to_value(spec_with_volume(name, name)).unwrap();
        *value.pointer_mut("/volumes/0/source").unwrap() = serde_json::json!({ "kind":"provisioned", "name":name, "maximum_bytes":30 * ployz_core::STORAGE_GIB });
        serde_json::from_value::<RequestedServiceSpec>(value).unwrap()
    });
    let error = client
        .preview(DeployIntent::apply_all(
            ProjectName::parse("app").unwrap(),
            services.iter(),
            skip_health(),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.details.get("code").unwrap(), "insufficient_storage");
    assert_eq!(error.details.get("machine_id").unwrap(), target_id.as_str());
    assert_eq!(
        error.details.get("shortfall_bytes").unwrap(),
        2 * ployz_core::STORAGE_GIB
    );
    assert!(error.message.contains("2.00 GiB"), "{error}");
    assert!(
        error
            .details
            .get("suggestions")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|suggestion| suggestion == "Expand the disk")
    );
    assert!(created.lock().unwrap().is_empty());
}

#[tokio::test]
async fn sdk_preview_recovers_pool_before_observing_existing_docker_volume() {
    let mut description = advertised_description();
    description
        .capabilities
        .insert(CapabilityName::parse(ployz_core::MACHINE_STORAGE_OBSERVATION_CAPABILITY).unwrap());
    let session = UnixSession::start().await;
    let mut service = DiscoveryService::new(description.clone());
    let target = service.machines.first().unwrap().machine.id;
    let project = ProjectName::parse("app").unwrap();
    let mut value = serde_json::to_value(spec_with_volume("api", "data")).unwrap();
    *value.pointer_mut("/volumes/0/source").unwrap() = serde_json::json!({
        "kind":"provisioned", "name":"data", "maximum_bytes":ployz_core::STORAGE_GIB
    });
    let requested: RequestedServiceSpec = serde_json::from_value(value).unwrap();
    let mut source = requested
        .volume_graph()
        .volumes()
        .first()
        .unwrap()
        .source
        .clone();
    source.scope_to_project(&project);
    let volume = super::support::created_volume(target, source.to_create_volume_request().unwrap());
    service.storage_capacity = Some(ployz_core::StorageCapacity {
        backing: ployz_core::StorageBacking::Fixed {
            pool_size_bytes: 10 * ployz_core::STORAGE_GIB,
        },
        unmanaged_used_bytes: 0,
        volumes: [(
            volume.id.name.clone(),
            ployz_core::ProvisionedVolumeMaximumBytes::new(
                std::num::NonZeroU64::new(ployz_core::STORAGE_GIB).unwrap(),
            ),
        )]
        .into(),
    });
    service
        .listed_volumes
        .lock()
        .unwrap()
        .insert(target, Vec::new());
    service.volume_observation_failures.lock().unwrap().insert(
        target,
        vec![ployz_core::VolumeObservationFailure {
            id: volume.id.clone(),
            error: RpcError {
                code: RpcErrorCode::Unavailable,
                message: "Pool is not imported".into(),
                details: serde_json::Value::Null,
            },
        }],
    );
    service.recover_volume_on_storage_inspect = Some(volume);
    let created = service.created_volumes.clone();
    let _machine = session.spawn_machine(description.machine_id, service).await;
    let client = unix_session::connect(&session.directory, description.machine_id.as_str())
        .await
        .unwrap();
    let preview = client
        .preview(DeployIntent::apply_one(project, requested, skip_health()))
        .await
        .unwrap();
    assert_eq!(
        preview
            .storage
            .first()
            .unwrap()
            .budget
            .additional_commitment_bytes,
        0
    );
    assert!(preview.volumes_to_create.is_empty());
    assert!(created.lock().unwrap().is_empty());
}

#[tokio::test]
async fn sdk_close_interrupts_blocked_data_loss_read() {
    let description = advertised_description();
    let session = UnixSession::start().await;
    let received = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut service = DiscoveryService::new(description.clone());
    service.list_machines_blocked = Some(received.clone());
    let _machine = session.spawn_machine(description.machine_id, service).await;
    let client = unix_session::connect(&session.directory, description.machine_id.as_str())
        .await
        .unwrap();
    let pending = {
        let client = client.clone();
        tokio::spawn(async move { client.data_loss_if_cluster_destroyed().await })
    };
    timeout(Duration::from_secs(2), received.notified())
        .await
        .unwrap();
    client.close().await;
    let error = timeout(Duration::from_millis(500), pending)
        .await
        .expect("close must release blocked read")
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Unavailable);
}

#[tokio::test]
async fn sdk_close_interrupts_running_deploy_with_uncertain_error() {
    let description = advertised_description();
    let session = UnixSession::start().await;
    let received = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut service = DiscoveryService::new(description.clone());
    service.create_container_blocked = Some(received.clone());
    let _machine = session.spawn_machine(description.machine_id, service).await;
    let client = unix_session::connect(&session.directory, description.machine_id.as_str())
        .await
        .unwrap();
    let preview = client
        .preview(DeployIntent::apply_one(
            ProjectName::parse("app").unwrap(),
            spec("web"),
            skip_health(),
        ))
        .await
        .unwrap();
    let running = preview.confirm().unwrap();
    timeout(Duration::from_secs(2), received.notified())
        .await
        .unwrap();
    client.close().await;
    let error = timeout(Duration::from_millis(500), running.finished())
        .await
        .expect("close must release blocked mutation")
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert!(error.message.contains("uncertain"));
}
