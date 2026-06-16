use super::fixtures::*;
use ployz_core::ops::{
    CertOperationFailure, CertOperationState, CertRunningStage, OperationStatus,
    StatusProjectionError,
};
use ployz_nats::operations::RecordCertEventError;

#[tokio::test]
async fn operation_repository_records_cert_lifecycle_against_real_nats() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let accepted = repository
        .submit_cert(cert_submission("op_cert", "cert_api"))
        .await
        .expect("cert submit accepted");

    assert_eq!(accepted.operation_id, operation_id("op_cert"));
    assert_eq!(accepted.cert_id, cert_id("cert_api"));

    repository
        .record_cert_challenge_published(
            &operation_id("op_cert"),
            &cert_id("cert_api"),
            cert_challenge("api.example.com"),
        )
        .await
        .expect("challenge records");
    repository
        .record_cert_validation_started(&operation_id("op_cert"), &cert_id("cert_api"))
        .await
        .expect("validation records");
    repository
        .record_cert_completed(
            &operation_id("op_cert"),
            active_cert("cert_api", "api.example.com"),
        )
        .await
        .expect("completion records");

    assert_eq!(
        repository
            .records()
            .get(&operation_id("op_cert"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::Cert {
            id: operation_id("op_cert"),
            cert_id: cert_id("cert_api"),
            state: CertOperationState::Completed,
            last_event_sequence: event_sequence(4),
        })
    );
}

#[tokio::test]
async fn operation_repository_duplicate_cert_operation_id_recovers_submit() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    let first = repository
        .submit_cert(cert_submission("op_cert", "cert_api"))
        .await
        .expect("first submit accepted");
    let duplicate = repository
        .submit_cert(cert_submission("op_cert", "cert_api"))
        .await
        .expect("duplicate operation id recovers");

    assert_eq!(duplicate.operation_id, operation_id("op_cert"));
    assert_eq!(duplicate.start_sequence, first.start_sequence);
    assert_eq!(duplicate.cert_id, cert_id("cert_api"));
}

#[tokio::test]
async fn operation_repository_rejects_cert_event_for_another_cert() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    repository
        .submit_cert(cert_submission("op_cert", "cert_api"))
        .await
        .expect("cert submit accepted");

    let rejected = repository
        .record_cert_failed(
            &operation_id("op_cert"),
            CertOperationFailure::AcmeValidationFailed {
                cert_id: cert_id("cert_other"),
                message: failure_message("validation failed"),
                retained_active_cert: None,
            },
        )
        .await
        .expect_err("wrong cert failure is rejected");

    assert!(matches!(
        rejected,
        RecordCertEventError::ProjectStatus(StatusProjectionError::OperationSubjectMismatch { .. })
    ));
    assert_eq!(
        repository
            .records()
            .get(&operation_id("op_cert"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::Cert {
            id: operation_id("op_cert"),
            cert_id: cert_id("cert_api"),
            state: CertOperationState::Accepted,
            last_event_sequence: event_sequence(1),
        })
    );
}

#[tokio::test]
async fn operation_repository_rejects_cert_validation_for_another_cert() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    repository
        .submit_cert(cert_submission("op_cert", "cert_api"))
        .await
        .expect("cert submit accepted");
    repository
        .record_cert_challenge_published(
            &operation_id("op_cert"),
            &cert_id("cert_api"),
            cert_challenge("api.example.com"),
        )
        .await
        .expect("challenge records");

    let rejected = repository
        .record_cert_validation_started(&operation_id("op_cert"), &cert_id("cert_other"))
        .await
        .expect_err("wrong cert validation is rejected");

    assert!(matches!(
        rejected,
        RecordCertEventError::ProjectStatus(StatusProjectionError::OperationSubjectMismatch { .. })
    ));
    assert_eq!(
        repository
            .records()
            .get(&operation_id("op_cert"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::Cert {
            id: operation_id("op_cert"),
            cert_id: cert_id("cert_api"),
            state: CertOperationState::Running {
                stage: CertRunningStage::ChallengePublished,
            },
            last_event_sequence: event_sequence(2),
        })
    );
}
