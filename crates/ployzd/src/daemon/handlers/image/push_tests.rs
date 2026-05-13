use super::*;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use ployz_api::{
    DaemonPayload, ImageDistributeValidationFailure, ImageTransferFailureStage,
    ImageTransferTargetStatus,
};
use ployz_model::{
    ImageArtifact, ImageArtifactProvenance, ImageAvailabilityRecord, ImageDigest,
    ImageOperationKind, ImagePresence, ImageRef, MachineId, MachineMembership, NetworkLifecycle,
    NetworkName, OperationStatus, OverlayIp, PublicKey,
};
use ployz_orchestrator::{Mesh, WireguardDriver};
use ployz_runtime_api::{
    Identity, ImageArchiveReader, RuntimeImage, RuntimeImageError, RuntimeImageImportResult,
};
use ployz_store_api::{ImageAvailabilityStore, MachineMembershipStore, StoreDriver};
use ployz_store_memory::StoreDriverMemoryExt as _;
use ployz_time::now_unix_secs;
use sha2::{Digest as _, Sha256};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt as _;
use tower::ServiceExt as _;

use crate::daemon::{ActiveMesh, RetainedSubnet};
use crate::features::image::registry::{
    REGISTRY_OPERATION_HEADER, REGISTRY_SESSION_HEADER, REGISTRY_SOURCE_MACHINE_HEADER,
};
use crate::mesh_state::network::NetworkConfig;

#[tokio::test]
async fn image_push_rejects_zero_targets_before_operation_side_effects() {
    let state = make_state();
    let response = state
        .handle_image_push(&ImagePushRequest {
            source_image: "example/app:latest".into(),
            target_machines: Vec::new(),
            platform: None,
            expected_digest: None,
        })
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_PUSH_TARGET_REQUIRED");
    assert!(
        state
            .image_operation_store()
            .list()
            .expect("list operations")
            .is_empty()
    );
}

#[tokio::test]
async fn image_push_self_target_uploads_imports_and_records_availability() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let listener = crate::features::image::registry::serve(
        "127.0.0.1:0".parse().expect("bind addr"),
        state.image_registry.clone(),
    )
    .await
    .expect("serve registry");
    state
        .active
        .as_mut()
        .expect("active mesh")
        .image_receiver_bind_addr = Some(listener.bind_addr());
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

    let response = state
        .handle_image_push_with_backend(
            &ImagePushRequest {
                source_image: "example/app:latest".into(),
                target_machines: vec![MachineId::new("founder")],
                platform: None,
                expected_digest: Some(digest.clone()),
            },
            &backend,
        )
        .await;

    assert!(response.is_ok(), "{response:?}");
    let Some(DaemonPayload::ImagePush(payload)) = response.payload() else {
        panic!("expected push payload");
    };
    assert_eq!(payload.artifact.digest(), &digest);
    assert_eq!(payload.targets.len(), 1);
    assert_eq!(
        payload.targets[0].status(),
        ImageTransferTargetStatus::Present
    );
    assert!(payload.targets[0].record().is_some());
    assert!(*backend.imported.lock().expect("imported lock"));
    let stored = state
        .active
        .as_ref()
        .expect("active mesh")
        .mesh
        .store
        .get_image_availability(&MachineId::new("founder"), &digest)
        .await
        .expect("get availability")
        .expect("availability");
    assert!(matches!(stored.presence, ImagePresence::Present { .. }));
    let operations = state
        .image_operation_store()
        .list()
        .expect("list operations");
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].kind, ImageOperationKind::Push);
    assert_eq!(operations[0].status(), OperationStatus::Succeeded);
    assert!(
        !state
            .data_dir
            .join("image-push")
            .join(&payload.operation_id)
            .exists()
    );
    listener.shutdown().await;
}

#[tokio::test]
async fn image_push_source_verify_failure_marks_operation_failed() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let actual = digest('a');
    let expected = digest('b');
    let backend = FakeImageBackend::new(actual, docker_archive_bytes());

    let response = state
        .handle_image_push_with_backend(
            &ImagePushRequest {
                source_image: "example/app:latest".into(),
                target_machines: vec![MachineId::new("founder")],
                platform: None,
                expected_digest: Some(expected.clone()),
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_PUSH_FAILED");
    assert_no_availability(&state, &expected).await;
    let operations = state
        .image_operation_store()
        .list()
        .expect("list operations");
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].kind, ImageOperationKind::Push);
    assert_eq!(operations[0].status(), OperationStatus::Failed);
    assert_eq!(operations[0].targets[0].status(), OperationStatus::Failed);
}

#[tokio::test]
async fn image_push_import_failure_does_not_record_availability() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let listener = crate::features::image::registry::serve(
        "127.0.0.1:0".parse().expect("bind addr"),
        state.image_registry.clone(),
    )
    .await
    .expect("serve registry");
    state
        .active
        .as_mut()
        .expect("active mesh")
        .image_receiver_bind_addr = Some(listener.bind_addr());
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes()).with_import_error();

    let response = state
        .handle_image_push_with_backend(
            &ImagePushRequest {
                source_image: "example/app:latest".into(),
                target_machines: vec![MachineId::new("founder")],
                platform: None,
                expected_digest: Some(digest.clone()),
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_PUSH_FAILED");
    assert_no_availability(&state, &digest).await;
    listener.shutdown().await;
}

#[tokio::test]
async fn image_push_without_expected_digest_uses_image_id_when_repo_digest_is_absent() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let listener = crate::features::image::registry::serve(
        "127.0.0.1:0".parse().expect("bind addr"),
        state.image_registry.clone(),
    )
    .await
    .expect("serve registry");
    state
        .active
        .as_mut()
        .expect("active mesh")
        .image_receiver_bind_addr = Some(listener.bind_addr());
    let digest = digest('a');
    let backend =
        FakeImageBackend::new(digest.clone(), docker_archive_bytes()).without_repo_digests();

    let response = state
        .handle_image_push_with_backend(
            &ImagePushRequest {
                source_image: "example/app:latest".into(),
                target_machines: vec![MachineId::new("founder")],
                platform: None,
                expected_digest: None,
            },
            &backend,
        )
        .await;

    assert!(response.is_ok(), "{response:?}");
    let Some(DaemonPayload::ImagePush(payload)) = response.payload() else {
        panic!("expected push payload");
    };
    assert_eq!(payload.artifact.digest(), &digest);
    let operations = state
        .image_operation_store()
        .list()
        .expect("list operations");
    assert_eq!(operations[0].digest.as_ref(), Some(&digest));
    listener.shutdown().await;
}

#[tokio::test]
async fn image_push_expected_repo_digest_uses_image_id_for_import_identity() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let listener = crate::features::image::registry::serve(
        "127.0.0.1:0".parse().expect("bind addr"),
        state.image_registry.clone(),
    )
    .await
    .expect("serve registry");
    state
        .active
        .as_mut()
        .expect("active mesh")
        .image_receiver_bind_addr = Some(listener.bind_addr());
    let repo_digest = digest('a');
    let image_id = digest('b');
    let backend = FakeImageBackend::new(repo_digest.clone(), docker_archive_bytes())
        .with_image_id(image_id.clone());

    let response = state
        .handle_image_push_with_backend(
            &ImagePushRequest {
                source_image: "example/app:latest".into(),
                target_machines: vec![MachineId::new("founder")],
                platform: None,
                expected_digest: Some(repo_digest.clone()),
            },
            &backend,
        )
        .await;

    assert!(response.is_ok(), "{response:?}");
    let Some(DaemonPayload::ImagePush(payload)) = response.payload() else {
        panic!("expected push payload");
    };
    assert_eq!(payload.artifact.digest(), &image_id);
    assert_no_availability(&state, &repo_digest).await;
    let stored = state
        .active
        .as_ref()
        .expect("active mesh")
        .mesh
        .store
        .get_image_availability(&MachineId::new("founder"), &image_id)
        .await
        .expect("get availability")
        .expect("image id availability");
    assert!(matches!(stored.presence, ImagePresence::Present { .. }));
    listener.shutdown().await;
}

#[tokio::test]
async fn image_push_expected_repo_digest_without_image_id_fails_before_transfer() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes()).without_image_id();

    let response = state
        .handle_image_push_with_backend(
            &ImagePushRequest {
                source_image: "example/app:latest".into(),
                target_machines: vec![MachineId::new("founder")],
                platform: None,
                expected_digest: Some(digest.clone()),
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_PUSH_FAILED");
    assert!(!*backend.imported.lock().expect("imported lock"));
    assert_no_availability(&state, &digest).await;
}

#[tokio::test]
async fn image_push_without_expected_digest_fails_when_runtime_has_no_digest_identity() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest, docker_archive_bytes())
        .without_repo_digests()
        .without_image_id();

    let response = state
        .handle_image_push_with_backend(
            &ImagePushRequest {
                source_image: "example/app:latest".into(),
                target_machines: vec![MachineId::new("founder")],
                platform: None,
                expected_digest: None,
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_PUSH_FAILED");
    let operations = state
        .image_operation_store()
        .list()
        .expect("list operations");
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].status(), OperationStatus::Failed);
}

#[tokio::test]
async fn image_push_receive_session_failure_marks_operation_failed() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    state
        .active
        .as_mut()
        .expect("active mesh")
        .image_receiver_bind_addr = None;
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

    let response = state
        .handle_image_push_with_backend(
            &ImagePushRequest {
                source_image: "example/app:latest".into(),
                target_machines: vec![MachineId::new("founder")],
                platform: None,
                expected_digest: Some(digest.clone()),
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_PUSH_FAILED");
    assert_no_availability(&state, &digest).await;
    let operations = state
        .image_operation_store()
        .list()
        .expect("list operations");
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].status(), OperationStatus::Failed);
    assert_eq!(operations[0].targets[0].status(), OperationStatus::Failed);
}

#[tokio::test]
async fn image_push_later_target_failure_preserves_first_target_success() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let listener = crate::features::image::registry::serve(
        "127.0.0.1:0".parse().expect("bind addr"),
        state.image_registry.clone(),
    )
    .await
    .expect("serve registry");
    state
        .active
        .as_mut()
        .expect("active mesh")
        .image_receiver_bind_addr = Some(listener.bind_addr());
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

    let response = state
        .handle_image_push_with_backend(
            &ImagePushRequest {
                source_image: "example/app:latest".into(),
                target_machines: vec![MachineId::new("founder"), MachineId::new("machine-a")],
                platform: None,
                expected_digest: Some(digest.clone()),
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_PUSH_PARTIAL_FAILED");
    let Some(DaemonPayload::ImagePush(payload)) = response.payload() else {
        panic!("expected push payload");
    };
    assert_eq!(payload.targets.len(), 2);
    assert_eq!(payload.targets[0].machine_id(), &MachineId::new("founder"));
    assert_eq!(
        payload.targets[0].status(),
        ImageTransferTargetStatus::Present
    );
    assert_eq!(
        payload.targets[1].machine_id(),
        &MachineId::new("machine-a")
    );
    assert_eq!(
        payload.targets[1].status(),
        ImageTransferTargetStatus::Failed
    );
    let failure = payload.targets[1].failure().expect("target failure");
    assert_eq!(failure.code, "IMAGE_DISTRIBUTE_RECEIVE_SESSION_FAILED");
    assert_eq!(failure.stage, ImageTransferFailureStage::ReceiveSession);
    let stored = state
        .active
        .as_ref()
        .expect("active mesh")
        .mesh
        .store
        .get_image_availability(&MachineId::new("founder"), &digest)
        .await
        .expect("get availability")
        .expect("first target availability");
    assert!(matches!(stored.presence, ImagePresence::Present { .. }));
    let operations = state
        .image_operation_store()
        .list()
        .expect("list operations");
    let push = operations
        .iter()
        .find(|operation| operation.kind == ImageOperationKind::Push)
        .expect("push operation");
    assert_eq!(push.status(), OperationStatus::Failed);
    assert!(
        push.targets
            .iter()
            .any(|target| target.machine_id() == &MachineId::new("founder")
                && target.status() == OperationStatus::Succeeded)
    );
    assert!(
        push.targets
            .iter()
            .any(|target| target.machine_id() == &MachineId::new("machine-a")
                && target.status() == OperationStatus::Failed)
    );
    listener.shutdown().await;
}

#[tokio::test]
async fn image_distribute_rejects_non_local_source_before_operation_side_effects() {
    let state = make_state();
    let response = state
        .handle_image_distribute(&ImageDistributeRequest {
            digest: ImageDigest::try_new(format!("sha256:{}", "a".repeat(64)))
                .expect("valid digest"),
            source_machine: MachineId::new("machine-a"),
            target_machines: vec![MachineId::new("machine-b")],
            platform: None,
        })
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_DISTRIBUTE_SOURCE_NOT_LOCAL");
    let Some(DaemonPayload::ImageDistributeValidation(payload)) = response.payload() else {
        panic!("expected validation payload");
    };
    assert_eq!(
        payload.failure,
        ImageDistributeValidationFailure::SourceNotLocal {
            source_machine: MachineId::new("machine-a"),
            local_machine: MachineId::new("founder"),
        }
    );
    assert!(
        state
            .image_operation_store()
            .list()
            .expect("list operations")
            .is_empty()
    );
}

#[tokio::test]
async fn image_distribute_validates_before_runtime_backend_lookup() {
    let state = make_state();
    let response = state
        .handle_image_distribute(&ImageDistributeRequest {
            digest: digest('a'),
            source_machine: MachineId::new("founder"),
            target_machines: Vec::new(),
            platform: None,
        })
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_DISTRIBUTE_TARGET_REQUIRED");
    assert!(
        state
            .image_operation_store()
            .list()
            .expect("list operations")
            .is_empty()
    );
}

#[tokio::test]
async fn image_distribute_source_target_verifies_and_records_local_availability() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

    let response = state
        .handle_image_distribute_with_backend(
            &ImageDistributeRequest {
                digest: digest.clone(),
                source_machine: MachineId::new("founder"),
                target_machines: vec![MachineId::new("founder")],
                platform: None,
            },
            &backend,
        )
        .await;

    assert!(response.is_ok(), "{response:?}");
    let Some(DaemonPayload::ImageDistribute(payload)) = response.payload() else {
        panic!("expected distribute payload");
    };
    assert_eq!(payload.digest, digest);
    assert_eq!(payload.targets.len(), 1);
    assert_eq!(
        payload.targets[0].status(),
        ImageTransferTargetStatus::Present
    );
    assert!(payload.targets[0].record().is_some());
    assert!(!*backend.imported.lock().expect("imported lock"));
    assert_eq!(backend.export_count(), 0);
    let stored = state
        .active
        .as_ref()
        .expect("active mesh")
        .mesh
        .store
        .get_image_availability(&MachineId::new("founder"), &payload.digest)
        .await
        .expect("get availability")
        .expect("availability");
    assert!(matches!(stored.presence, ImagePresence::Present { .. }));
    let operations = state
        .image_operation_store()
        .list()
        .expect("list operations");
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].status(), OperationStatus::Succeeded);
    assert!(
        !state
            .data_dir
            .join("image-transfer")
            .join(&payload.operation_id)
            .exists()
    );
    assert!(
        !state
            .data_dir
            .join("image-import")
            .join(&payload.operation_id)
            .exists()
    );
}

#[tokio::test]
async fn image_distribute_rejects_zero_and_duplicate_targets_before_operation_side_effects() {
    let state = make_state();
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

    let response = state
        .handle_image_distribute_with_backend(
            &ImageDistributeRequest {
                digest: digest.clone(),
                source_machine: MachineId::new("founder"),
                target_machines: Vec::new(),
                platform: None,
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_DISTRIBUTE_TARGET_REQUIRED");
    let Some(DaemonPayload::ImageDistributeValidation(payload)) = response.payload() else {
        panic!("expected validation payload");
    };
    assert_eq!(
        payload.failure,
        ImageDistributeValidationFailure::TargetRequired { target_count: 0 }
    );

    let response = state
        .handle_image_distribute_with_backend(
            &ImageDistributeRequest {
                digest,
                source_machine: MachineId::new("founder"),
                target_machines: vec![MachineId::new("founder"), MachineId::new("founder")],
                platform: None,
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_DISTRIBUTE_DUPLICATE_TARGET");
    let Some(DaemonPayload::ImageDistributeValidation(payload)) = response.payload() else {
        panic!("expected validation payload");
    };
    assert_eq!(
        payload.failure,
        ImageDistributeValidationFailure::DuplicateTarget {
            duplicate_target: MachineId::new("founder")
        }
    );
    assert!(
        state
            .image_operation_store()
            .list()
            .expect("list operations")
            .is_empty()
    );
}

#[tokio::test]
async fn image_distribute_skips_present_targets_without_exporting() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());
    let store = state
        .active
        .as_ref()
        .expect("active mesh")
        .mesh
        .store
        .clone();
    store
        .upsert_image_availability(&present_availability_record("founder", digest.clone()))
        .await
        .expect("seed founder availability");
    store
        .upsert_image_availability(&present_availability_record("machine-a", digest.clone()))
        .await
        .expect("seed machine-a availability");

    let response = state
        .handle_image_distribute_with_backend(
            &ImageDistributeRequest {
                digest: digest.clone(),
                source_machine: MachineId::new("founder"),
                target_machines: vec![MachineId::new("founder"), MachineId::new("machine-a")],
                platform: None,
            },
            &backend,
        )
        .await;

    assert!(response.is_ok(), "{response:?}");
    let Some(DaemonPayload::ImageDistribute(payload)) = response.payload() else {
        panic!("expected distribute payload");
    };
    assert_eq!(payload.targets.len(), 2);
    assert!(
        payload
            .targets
            .iter()
            .all(|target| target.status() == ImageTransferTargetStatus::SkippedPresent)
    );
    assert_eq!(backend.export_count(), 0);
    let operations = state
        .image_operation_store()
        .list()
        .expect("list operations");
    assert_eq!(operations[0].status(), OperationStatus::Succeeded);
    assert!(
        operations[0]
            .targets
            .iter()
            .all(|target| target.status() == OperationStatus::Succeeded)
    );
}

#[tokio::test]
async fn image_distribute_partial_failure_preserves_success_and_attempts_later_targets() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

    let response = state
        .handle_image_distribute_with_backend(
            &ImageDistributeRequest {
                digest: digest.clone(),
                source_machine: MachineId::new("founder"),
                target_machines: vec![MachineId::new("founder"), MachineId::new("machine-a")],
                platform: None,
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_DISTRIBUTE_PARTIAL_FAILED");
    let Some(DaemonPayload::ImageDistribute(payload)) = response.payload() else {
        panic!("expected distribute payload");
    };
    assert_eq!(payload.targets.len(), 2);
    assert_eq!(payload.targets[0].machine_id(), &MachineId::new("founder"));
    assert_eq!(
        payload.targets[0].status(),
        ImageTransferTargetStatus::Present
    );
    assert_eq!(
        payload.targets[1].machine_id(),
        &MachineId::new("machine-a")
    );
    assert_eq!(
        payload.targets[1].status(),
        ImageTransferTargetStatus::Failed
    );
    assert_eq!(
        payload.targets[1].failure().expect("target failure").stage,
        ImageTransferFailureStage::ReceiveSession
    );
    assert_eq!(backend.export_count(), 1);
    let stored = state
        .active
        .as_ref()
        .expect("active mesh")
        .mesh
        .store
        .get_image_availability(&MachineId::new("founder"), &digest)
        .await
        .expect("get availability")
        .expect("founder availability");
    assert!(matches!(stored.presence, ImagePresence::Present { .. }));
    let operations = state
        .image_operation_store()
        .list()
        .expect("list operations");
    assert_eq!(operations[0].status(), OperationStatus::Failed);
    assert_eq!(
        operations[0].targets[0].status(),
        OperationStatus::Succeeded
    );
    assert_eq!(operations[0].targets[1].status(), OperationStatus::Failed);
}

#[tokio::test]
async fn image_distribute_continues_after_failed_target() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

    let response = state
        .handle_image_distribute_with_backend(
            &ImageDistributeRequest {
                digest: digest.clone(),
                source_machine: MachineId::new("founder"),
                target_machines: vec![MachineId::new("machine-a"), MachineId::new("founder")],
                platform: None,
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_DISTRIBUTE_PARTIAL_FAILED");
    let Some(DaemonPayload::ImageDistribute(payload)) = response.payload() else {
        panic!("expected distribute payload");
    };
    assert_eq!(payload.targets.len(), 2);
    assert_eq!(
        payload.targets[0].machine_id(),
        &MachineId::new("machine-a")
    );
    assert_eq!(
        payload.targets[0].status(),
        ImageTransferTargetStatus::Failed
    );
    assert_eq!(payload.targets[1].machine_id(), &MachineId::new("founder"));
    assert_eq!(
        payload.targets[1].status(),
        ImageTransferTargetStatus::Present
    );
    let stored = state
        .active
        .as_ref()
        .expect("active mesh")
        .mesh
        .store
        .get_image_availability(&MachineId::new("founder"), &digest)
        .await
        .expect("get availability")
        .expect("founder availability");
    assert!(matches!(stored.presence, ImagePresence::Present { .. }));
}

#[tokio::test]
async fn image_distribute_attempts_multiple_transfer_targets_from_one_export() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

    let response = state
        .handle_image_distribute_with_backend(
            &ImageDistributeRequest {
                digest,
                source_machine: MachineId::new("founder"),
                target_machines: vec![MachineId::new("machine-a"), MachineId::new("machine-b")],
                platform: None,
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_DISTRIBUTE_FAILED");
    let Some(DaemonPayload::ImageDistribute(payload)) = response.payload() else {
        panic!("expected distribute payload");
    };
    assert_eq!(
        payload
            .targets
            .iter()
            .map(|target| target.machine_id().clone())
            .collect::<Vec<_>>(),
        vec![MachineId::new("machine-a"), MachineId::new("machine-b")]
    );
    assert!(
        payload
            .targets
            .iter()
            .all(|target| target.status() == ImageTransferTargetStatus::Failed)
    );
    assert_eq!(backend.export_count(), 1);
    let operations = state
        .image_operation_store()
        .list()
        .expect("list operations");
    assert_eq!(operations[0].targets.len(), 2);
    assert!(
        operations[0]
            .targets
            .iter()
            .all(|target| target.status() == OperationStatus::Failed)
    );
}

#[tokio::test]
async fn image_distribute_export_failure_preserves_skipped_targets() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes()).with_export_error();
    let store = state
        .active
        .as_ref()
        .expect("active mesh")
        .mesh
        .store
        .clone();
    store
        .upsert_image_availability(&present_availability_record("founder", digest.clone()))
        .await
        .expect("seed availability");

    let response = state
        .handle_image_distribute_with_backend(
            &ImageDistributeRequest {
                digest: digest.clone(),
                source_machine: MachineId::new("founder"),
                target_machines: vec![MachineId::new("founder"), MachineId::new("machine-a")],
                platform: None,
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_DISTRIBUTE_PARTIAL_FAILED");
    let Some(DaemonPayload::ImageDistribute(payload)) = response.payload() else {
        panic!("expected distribute payload");
    };
    assert_eq!(
        payload.targets[0].status(),
        ImageTransferTargetStatus::SkippedPresent
    );
    assert_eq!(
        payload.targets[1].status(),
        ImageTransferTargetStatus::Failed
    );
    let operations = state
        .image_operation_store()
        .list()
        .expect("list operations");
    assert_eq!(
        operations[0].targets[0].status(),
        OperationStatus::Succeeded
    );
    assert_eq!(operations[0].targets[1].status(), OperationStatus::Failed);
}

#[tokio::test]
async fn image_distribute_export_failure_preserves_local_source_target() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes()).with_export_error();

    let response = state
        .handle_image_distribute_with_backend(
            &ImageDistributeRequest {
                digest: digest.clone(),
                source_machine: MachineId::new("founder"),
                target_machines: vec![MachineId::new("founder"), MachineId::new("machine-a")],
                platform: None,
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_DISTRIBUTE_PARTIAL_FAILED");
    let Some(DaemonPayload::ImageDistribute(payload)) = response.payload() else {
        panic!("expected distribute payload");
    };
    assert_eq!(
        payload.targets[0].status(),
        ImageTransferTargetStatus::Present
    );
    assert_eq!(
        payload.targets[1].status(),
        ImageTransferTargetStatus::Failed
    );
    let stored = state
        .active
        .as_ref()
        .expect("active mesh")
        .mesh
        .store
        .get_image_availability(&MachineId::new("founder"), &digest)
        .await
        .expect("get availability")
        .expect("founder availability");
    assert!(matches!(stored.presence, ImagePresence::Present { .. }));
    let operations = state
        .image_operation_store()
        .list()
        .expect("list operations");
    assert_eq!(
        operations[0].targets[0].status(),
        OperationStatus::Succeeded
    );
    assert_eq!(operations[0].targets[1].status(), OperationStatus::Failed);
}

#[tokio::test]
async fn image_distribute_archive_parse_failure_marks_all_targets_failed_and_cleans_work_dir() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), b"not a tar archive".to_vec());

    let response = state
        .handle_image_distribute_with_backend(
            &ImageDistributeRequest {
                digest: digest.clone(),
                source_machine: MachineId::new("founder"),
                target_machines: vec![MachineId::new("machine-a")],
                platform: None,
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_DISTRIBUTE_FAILED");
    let Some(DaemonPayload::ImageDistribute(payload)) = response.payload() else {
        panic!("expected distribute payload");
    };
    assert_eq!(payload.targets.len(), 1);
    assert_eq!(
        payload.targets[0].status(),
        ImageTransferTargetStatus::Failed
    );
    assert!(
        !state
            .data_dir
            .join("image-transfer")
            .join(&payload.operation_id)
            .exists()
    );
    let operations = state
        .image_operation_store()
        .list()
        .expect("list operations");
    assert_eq!(operations[0].targets[0].status(), OperationStatus::Failed);
}

#[tokio::test]
async fn image_distribute_source_verify_failure_marks_all_targets_failed() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let actual = digest('a');
    let expected = digest('b');
    let backend = FakeImageBackend::new(actual, docker_archive_bytes());

    let response = state
        .handle_image_distribute_with_backend(
            &ImageDistributeRequest {
                digest: expected.clone(),
                source_machine: MachineId::new("founder"),
                target_machines: vec![MachineId::new("founder"), MachineId::new("machine-a")],
                platform: None,
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_DISTRIBUTE_FAILED");
    let Some(DaemonPayload::ImageDistribute(payload)) = response.payload() else {
        panic!("expected distribute payload");
    };
    assert_eq!(payload.targets.len(), 2);
    assert!(
        payload
            .targets
            .iter()
            .all(|target| target.status() == ImageTransferTargetStatus::Failed)
    );
    assert_eq!(backend.export_count(), 0);
    assert_no_availability(&state, &expected).await;
    let operations = state
        .image_operation_store()
        .list()
        .expect("list operations");
    assert_eq!(operations[0].status(), OperationStatus::Failed);
    assert!(
        operations[0]
            .targets
            .iter()
            .all(|target| target.status() == OperationStatus::Failed)
    );
}

#[tokio::test]
async fn image_distribute_receive_session_failure_marks_operation_failed() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    state
        .active
        .as_mut()
        .expect("active mesh")
        .image_receiver_bind_addr = None;
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

    let response = state
        .handle_image_distribute_with_backend(
            &ImageDistributeRequest {
                digest,
                source_machine: MachineId::new("founder"),
                target_machines: vec![MachineId::new("machine-a")],
                platform: None,
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_DISTRIBUTE_FAILED");
    let operations = state
        .image_operation_store()
        .list()
        .expect("list operations");
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].status(), OperationStatus::Failed);
    assert_eq!(operations[0].targets[0].status(), OperationStatus::Failed);
}

#[tokio::test]
async fn image_distribute_target_failure_does_not_record_availability() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

    let response = state
        .handle_image_distribute_with_backend(
            &ImageDistributeRequest {
                digest: digest.clone(),
                source_machine: MachineId::new("founder"),
                target_machines: vec![MachineId::new("machine-a")],
                platform: None,
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_DISTRIBUTE_FAILED");
    assert_no_availability_on(&state, "machine-a", &digest).await;
}

#[tokio::test]
async fn image_distribute_source_target_skips_existing_availability_without_rewriting() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());
    let store = state
        .active
        .as_ref()
        .expect("active mesh")
        .mesh
        .store
        .clone();
    let existing = present_availability_record("founder", digest.clone());
    store
        .upsert_image_availability(&existing)
        .await
        .expect("seed availability");

    let response = state
        .handle_image_distribute_with_backend(
            &ImageDistributeRequest {
                digest: digest.clone(),
                source_machine: MachineId::new("founder"),
                target_machines: vec![MachineId::new("founder")],
                platform: None,
            },
            &backend,
        )
        .await;

    assert!(response.is_ok(), "{response:?}");
    let Some(DaemonPayload::ImageDistribute(payload)) = response.payload() else {
        panic!("expected distribute payload");
    };
    assert_eq!(payload.targets.len(), 1);
    assert_eq!(
        payload.targets[0].status(),
        ImageTransferTargetStatus::SkippedPresent
    );
    assert_eq!(backend.export_count(), 0);
    let stored = store
        .get_image_availability(&MachineId::new("founder"), &digest)
        .await
        .expect("get availability")
        .expect("availability");
    assert_eq!(stored, existing);
}

#[tokio::test]
async fn image_received_import_missing_manifest_does_not_record_availability() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

    let response = state
        .handle_image_received_import_with_backend(
            &ImageReceivedImportRequest {
                operation_id: "op-1".into(),
                source_machine: MachineId::new("founder"),
                repository: "ployz/op-1".into(),
                reference: "op-1".into(),
                expected_digest: digest.clone(),
                platform: None,
                repo_tags: vec!["example/app:latest".into()],
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_RECEIVED_IMPORT_RECONSTRUCT_FAILED");
    assert!(
        state
            .active
            .as_ref()
            .expect("active mesh")
            .mesh
            .store
            .get_image_availability(&MachineId::new("founder"), &digest)
            .await
            .expect("get availability")
            .is_none()
    );
}

#[tokio::test]
async fn image_received_import_rejects_unsafe_operation_id_before_filesystem_work() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let digest = digest('a');
    let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

    let response = state
        .handle_image_received_import_with_backend(
            &ImageReceivedImportRequest {
                operation_id: "../outside".into(),
                source_machine: MachineId::new("founder"),
                repository: "ployz/op-1".into(),
                reference: "op-1".into(),
                expected_digest: digest,
                platform: None,
                repo_tags: vec!["example/app:latest".into()],
            },
            &backend,
        )
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_RECEIVED_IMPORT_INVALID_OPERATION");
    assert!(!state.data_dir.join("outside").exists());
}

#[tokio::test]
async fn image_receive_session_requires_active_mesh() {
    let state = make_state();
    let response = state
        .handle_image_receive_session(&ImageReceiveSessionRequest {
            operation_id: "image-push-1".into(),
            source_machine: MachineId::new("machine-a"),
            repository: Some("ployz/image-push-1".into()),
        })
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_RECEIVER_INACTIVE");
}

#[tokio::test]
async fn image_receive_session_returns_endpoint_token_and_headers_for_local_source() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;

    let response = state
        .handle_image_receive_session(&ImageReceiveSessionRequest {
            operation_id: "image-push-1".into(),
            source_machine: MachineId::new("founder"),
            repository: Some("ployz/image-push-1".into()),
        })
        .await;

    assert!(response.is_ok(), "{response:?}");
    let Some(DaemonPayload::ImageReceiveSession(payload)) = response.payload() else {
        panic!("expected image receive session payload");
    };
    assert_eq!(payload.target_machine, MachineId::new("founder"));
    assert_eq!(
        payload.endpoint,
        "http://127.0.0.1:4320/v2/ployz/image-push-1"
    );
    assert_eq!(
        payload
            .headers
            .get(REGISTRY_OPERATION_HEADER)
            .map(String::as_str),
        Some("image-push-1")
    );
    assert_eq!(
        payload
            .headers
            .get(REGISTRY_SOURCE_MACHINE_HEADER)
            .map(String::as_str),
        Some("founder")
    );
    assert_eq!(
        payload
            .headers
            .get(REGISTRY_SESSION_HEADER)
            .map(String::as_str),
        Some(payload.token.as_str())
    );
    assert!(payload.expires_at_unix_secs > 0);
}

#[tokio::test]
async fn image_receive_session_rejects_unknown_source_machine() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;

    let response = state
        .handle_image_receive_session(&ImageReceiveSessionRequest {
            operation_id: "image-push-1".into(),
            source_machine: MachineId::new("unknown"),
            repository: Some("ployz/image-push-1".into()),
        })
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_RECEIVER_SOURCE_UNKNOWN");
}

#[tokio::test]
async fn image_receive_session_rejects_remote_source_for_loopback_receiver() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;

    let response = state
        .handle_image_receive_session(&ImageReceiveSessionRequest {
            operation_id: "image-push-1".into(),
            source_machine: MachineId::new("machine-a"),
            repository: Some("ployz/image-push-1".into()),
        })
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IMAGE_RECEIVER_SOURCE_NOT_LOCAL");
}

#[tokio::test]
async fn image_receive_session_token_authorizes_registry_upload() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let response = state
        .handle_image_receive_session(&ImageReceiveSessionRequest {
            operation_id: "image-push-1".into(),
            source_machine: MachineId::new("founder"),
            repository: Some("ployz/image-push-1".into()),
        })
        .await;
    let Some(DaemonPayload::ImageReceiveSession(payload)) = response.payload() else {
        panic!("expected image receive session payload");
    };
    let digest = test_sha256_digest(b"hello");
    let router = state.image_registry.clone().router();
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/v2/ployz/image-push-1/blobs/uploads/?digest={digest}"
        ))
        .header(
            REGISTRY_OPERATION_HEADER,
            payload.headers[REGISTRY_OPERATION_HEADER].as_str(),
        )
        .header(
            REGISTRY_SOURCE_MACHINE_HEADER,
            payload.headers[REGISTRY_SOURCE_MACHINE_HEADER].as_str(),
        )
        .header(
            REGISTRY_SESSION_HEADER,
            payload.headers[REGISTRY_SESSION_HEADER].as_str(),
        )
        .body(Body::from("hello"))
        .expect("request");

    let response = router.oneshot(request).await.expect("registry response");

    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn image_receive_session_token_is_scoped_to_repository() {
    let mut state = make_state();
    install_active_mesh(&mut state).await;
    let response = state
        .handle_image_receive_session(&ImageReceiveSessionRequest {
            operation_id: "image-push-1".into(),
            source_machine: MachineId::new("founder"),
            repository: Some("ployz/image-push-1".into()),
        })
        .await;
    let Some(DaemonPayload::ImageReceiveSession(payload)) = response.payload() else {
        panic!("expected image receive session payload");
    };
    let digest = test_sha256_digest(b"hello");
    let router = state.image_registry.clone().router();
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("/v2/other/repo/blobs/uploads/?digest={digest}"))
        .header(
            REGISTRY_OPERATION_HEADER,
            payload.headers[REGISTRY_OPERATION_HEADER].as_str(),
        )
        .header(
            REGISTRY_SOURCE_MACHINE_HEADER,
            payload.headers[REGISTRY_SOURCE_MACHINE_HEADER].as_str(),
        )
        .header(
            REGISTRY_SESSION_HEADER,
            payload.headers[REGISTRY_SESSION_HEADER].as_str(),
        )
        .body(Body::from("hello"))
        .expect("request");

    let response = router.oneshot(request).await.expect("registry response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

struct FakeImageBackend {
    digest: ImageDigest,
    archive: Vec<u8>,
    repo_digests: Vec<ImageDigest>,
    image_id: Option<String>,
    imported: Arc<Mutex<bool>>,
    export_count: Arc<Mutex<u64>>,
    export_error: bool,
    import_error: bool,
    import_verify_error: bool,
}

impl FakeImageBackend {
    fn new(digest: ImageDigest, archive: Vec<u8>) -> Self {
        Self {
            repo_digests: vec![digest.clone()],
            image_id: Some(digest.as_str().into()),
            digest,
            archive,
            imported: Arc::new(Mutex::new(false)),
            export_count: Arc::new(Mutex::new(0)),
            export_error: false,
            import_error: false,
            import_verify_error: false,
        }
    }

    fn with_export_error(mut self) -> Self {
        self.export_error = true;
        self
    }

    fn with_import_error(mut self) -> Self {
        self.import_error = true;
        self
    }

    fn without_repo_digests(mut self) -> Self {
        self.repo_digests.clear();
        self
    }

    fn without_image_id(mut self) -> Self {
        self.image_id = None;
        self
    }

    fn with_image_id(mut self, digest: ImageDigest) -> Self {
        self.image_id = Some(digest.as_str().into());
        self
    }

    fn export_count(&self) -> u64 {
        *self.export_count.lock().expect("export count lock")
    }
}

#[async_trait::async_trait]
impl RuntimeImageBackend for FakeImageBackend {
    async fn inspect_image(
        &self,
        reference: &str,
    ) -> Result<Option<RuntimeImage>, RuntimeImageError> {
        let (repo_digests, id) = if self.import_verify_error && reference != self.digest.as_str() {
            (Vec::new(), None)
        } else {
            (self.repo_digests.clone(), self.image_id.clone())
        };
        Ok(Some(RuntimeImage {
            reference: reference.into(),
            id,
            repo_digests,
            platform: None,
            size_bytes: None,
        }))
    }

    async fn export_image_archive(
        &self,
        reference: &str,
    ) -> Result<ImageArchiveReader, RuntimeImageError> {
        let _ = reference;
        *self.export_count.lock().expect("export count lock") += 1;
        if self.export_error {
            return Err(RuntimeImageError::backend(
                "fake image export",
                "export failed",
            ));
        }
        Ok(Box::pin(std::io::Cursor::new(self.archive.clone())))
    }

    async fn import_image_archive(
        &self,
        mut archive: ImageArchiveReader,
    ) -> Result<RuntimeImageImportResult, RuntimeImageError> {
        if self.import_error {
            return Err(RuntimeImageError::backend(
                "fake image import",
                "import failed",
            ));
        }
        let mut body = Vec::new();
        archive
            .read_to_end(&mut body)
            .await
            .map_err(|error| RuntimeImageError::backend("fake image import", error.to_string()))?;
        if body.is_empty() {
            return Err(RuntimeImageError::backend(
                "fake image import",
                "archive was empty",
            ));
        }
        *self.imported.lock().expect("imported lock") = true;
        Ok(RuntimeImageImportResult {
            messages: vec!["imported".into()],
        })
    }
}

fn make_state() -> DaemonState {
    let data_dir =
        std::env::temp_dir().join(format!("ployz-image-push-handler-{}", uuid::Uuid::new_v4()));
    let identity = Identity::generate(MachineId::new("founder"), [31; 32]);
    DaemonState::new_for_tests(
        &data_dir,
        identity,
        "10.210.0.0/16".into(),
        24,
        4319,
        "127.0.0.1:0".into(),
        None,
        1,
    )
}

async fn install_active_mesh(state: &mut DaemonState) {
    let identity = Identity::generate(MachineId::new("founder"), [31; 32]);
    let mut config = NetworkConfig::new(
        NetworkName("alpha".into()),
        &identity.public_key,
        "10.210.0.0/16",
        "10.210.0.0/24".parse().expect("valid subnet"),
    );
    config.lifecycle = NetworkLifecycle::Running;
    let store = StoreDriver::memory();
    store
        .upsert_self_machine(&MachineMembership::seed(
            MachineId::new("founder"),
            PublicKey([31; 32]),
            OverlayIp("fd00::31".parse().expect("valid overlay")),
            None,
            Vec::new(),
        ))
        .await
        .expect("insert local machine");
    store
        .upsert_self_machine(&MachineMembership::seed(
            MachineId::new("machine-a"),
            PublicKey([12; 32]),
            OverlayIp("fd00::12".parse().expect("valid overlay")),
            None,
            Vec::new(),
        ))
        .await
        .expect("insert source machine");
    let mesh = Mesh::new(
        WireguardDriver::memory(),
        store,
        None,
        state.identity.machine_id.clone(),
        51820,
    );
    state.active = Some(ActiveMesh {
        retained_subnet: RetainedSubnet::from_running_config(config.subnet),
        config,
        mesh,
        nats_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        zfs_transfer: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        image_receiver: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        image_receiver_bind_addr: Some("127.0.0.1:4320".parse().expect("valid address")),
        gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        certificate_renewal: None,
        bootstrap_peer_seed: None,
    });
}

fn test_sha256_digest(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    format!("sha256:{:x}", hasher.finalize())
}

fn digest(hex: char) -> ImageDigest {
    ImageDigest::try_new(format!("sha256:{}", hex.to_string().repeat(64))).expect("valid digest")
}

async fn assert_no_availability(state: &DaemonState, digest: &ImageDigest) {
    assert_no_availability_on(state, "founder", digest).await;
}

async fn assert_no_availability_on(state: &DaemonState, machine_id: &str, digest: &ImageDigest) {
    assert!(
        state
            .active
            .as_ref()
            .expect("active mesh")
            .mesh
            .store
            .get_image_availability(&MachineId::new(machine_id), digest)
            .await
            .expect("get availability")
            .is_none()
    );
}

fn present_availability_record(machine_id: &str, digest: ImageDigest) -> ImageAvailabilityRecord {
    let now = now_unix_secs();
    ImageAvailabilityRecord {
        machine_id: MachineId::new(machine_id),
        digest: digest.clone(),
        presence: ImagePresence::Present {
            artifact: ImageArtifact {
                image: ImageRef::digest_only(digest),
                platform: None,
                provenance: ImageArtifactProvenance::External {
                    source: Some("test".into()),
                },
                created_at: now,
            },
            recorded_at: now,
            source_operation_id: Some("seed".into()),
        },
        updated_at: now,
    }
}

fn docker_archive_bytes() -> Vec<u8> {
    let mut output = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut output);
        append_tar_member(&mut builder, "config.json", br#"{"architecture":"amd64"}"#);
        append_tar_member(&mut builder, "layer-one/layer.tar", b"layer-one");
        append_tar_member(&mut builder, "layer-two/layer.tar", b"layer-two");
        append_tar_member(
                &mut builder,
                "manifest.json",
                br#"[{"Config":"config.json","RepoTags":["example/app:latest"],"Layers":["layer-one/layer.tar","layer-two/layer.tar"]}]"#,
            );
        builder.finish().expect("finish tar");
    }
    output
}

fn append_tar_member(builder: &mut tar::Builder<&mut Vec<u8>>, name: &str, body: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, name, body)
        .expect("append tar member");
}
