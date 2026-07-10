use ployz_core::cert::{
    ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow, CertificateProvisionFailure,
};
use ployz_core::ids::{CertId, MachineId, OperationId};
use ployz_core::ops::{
    CertOperationFailure, CertOperationFailureError, CertOperationState, EventSequence,
    FailureMessage, OperationEvent, OperationProjection, OperationStatus, RouteHostname,
    project_operation_event,
};

#[test]
fn valid_reused_authorization_can_complete_without_a_new_challenge() {
    let active_cert = active_cert();
    let accepted = OperationStatus::cert_accepted(operation_id(), cert_id(), sequence(1));

    assert_eq!(
        project_operation_event(
            &accepted,
            OperationEvent::CertCompleted {
                operation_id: operation_id(),
                active_cert,
            },
            sequence(2),
        ),
        Ok(OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::Cert {
                id: operation_id(),
                cert_id: cert_id(),
                state: CertOperationState::Completed,
                last_event_sequence: sequence(2),
            }),
        })
    );
}

#[test]
fn every_certificate_failure_roundtrips_inside_operation_evidence() {
    for failure in provision_failures() {
        let operation_failure =
            CertOperationFailure::try_new(cert_id(), failure, Some(active_cert()))
                .expect("matching certificate identities");
        let event = OperationEvent::CertFailed {
            operation_id: operation_id(),
            failure: operation_failure.clone(),
        };

        assert_eq!(
            serde_json::from_value::<OperationEvent>(
                serde_json::to_value(&event).expect("serialize event")
            )
            .expect("deserialize event"),
            event
        );
    }
}

#[test]
fn operation_failure_rejects_a_retained_certificate_for_another_identity() {
    let mut retained = active_cert();
    retained.cert_id = CertId::try_new("cert_other").expect("cert id");

    assert_eq!(
        CertOperationFailure::try_new(
            cert_id(),
            CertificateProvisionFailure::AcmeValidation {
                message: FailureMessage::try_new("validation failed").expect("message"),
            },
            Some(retained),
        ),
        Err(CertOperationFailureError::RetainedCertIdMismatch {
            cert_id: cert_id(),
            retained_cert_id: CertId::try_new("cert_other").expect("cert id"),
        })
    );
}

#[test]
fn operation_failure_serde_rejects_mismatched_attempted_commit_evidence() {
    let failure = CertOperationFailure::try_new(
        cert_id(),
        CertificateProvisionFailure::ActiveCertCommit {
            attempted_active_cert: active_cert(),
            message: FailureMessage::try_new("commit failed").expect("message"),
        },
        None,
    )
    .expect("matching certificate identities");
    let mut encoded = serde_json::to_value(failure).expect("serialize failure");
    encoded["failure"]["attempted_active_cert"]["cert_id"] = serde_json::json!("cert_other");

    assert!(serde_json::from_value::<CertOperationFailure>(encoded).is_err());
}

fn provision_failures() -> Vec<CertificateProvisionFailure> {
    let message = || FailureMessage::try_new("certificate failure").expect("message");
    vec![
        CertificateProvisionFailure::OperationEvidenceWrite { message: message() },
        CertificateProvisionFailure::DnsPreflight { message: message() },
        CertificateProvisionFailure::ChallengePublish { message: message() },
        CertificateProvisionFailure::ChallengeReadiness {
            missing_machine_ids: vec![machine_id()],
        },
        CertificateProvisionFailure::AcmeValidation { message: message() },
        CertificateProvisionFailure::GatewayArtifactPush {
            machine_id: machine_id(),
            message: message(),
        },
        CertificateProvisionFailure::ActiveCertCommit {
            attempted_active_cert: active_cert(),
            message: message(),
        },
    ]
}

fn active_cert() -> ActiveCertState {
    ActiveCertState {
        cert_id: cert_id(),
        hostname: RouteHostname::try_new("api.example.com").expect("hostname"),
        bundle_ref: CertBundleRef::try_new(format!(
            "sha256:{}:/var/lib/ployz/certs/cert_api.pem",
            "a".repeat(64)
        ))
        .expect("bundle ref"),
        validity: CertValidityWindow::try_new(
            CertValidAt::try_new(1_700_000_000).expect("not before"),
            CertValidAt::try_new(1_707_776_000).expect("not after"),
        )
        .expect("validity"),
    }
}

fn operation_id() -> OperationId {
    OperationId::try_new("op_cert").expect("operation id")
}

fn cert_id() -> CertId {
    CertId::try_new("cert_api").expect("cert id")
}

fn machine_id() -> MachineId {
    MachineId::try_new("machine_gateway").expect("machine id")
}

fn sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("event sequence")
}
