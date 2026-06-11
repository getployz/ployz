use ployz_core::cert::{
    AcmeChallengeStateKey, AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue,
    AcmeHttp01Challenge, ActiveCertState, CertBundleRef, CertStateKey, CertValidAt,
    CertValidityWindow,
};
use ployz_core::ops::{
    CertOperationFailure, CertOperationState, CertRunningStage, FailureMessage, OperationEvent,
    OperationEventProjection, OperationStatus, OperationSubjectRef, RouteHostname,
    StatusProjectionError, project_operation_event,
};
use ployz_test_support::ids::{cert_id, event_sequence, operation_id};
use ployzd::controllers::cert::{
    AcmeChallengeWrite, AcmeFinishResult, ActiveCertWrite, CertChallengeCommand, CertChallengePlan,
    CertFinishCommand, CertFinishPlan, CertRenewalPlanError, plan_cert_challenge, plan_cert_finish,
};

#[test]
fn cert_challenge_phase_writes_ttl_challenge_state() {
    let cert_id = cert_id("cert_api");
    let challenge = challenge("api.example.com");

    assert_eq!(
        plan_cert_challenge(CertChallengeCommand {
            cert_id: cert_id.clone(),
            challenge: challenge.clone(),
            current: None,
        }),
        Ok(CertChallengePlan {
            lock_key: ployz_core::cert::AcmeLockKey::from_cert_id(&cert_id),
            challenge_write: AcmeChallengeWrite {
                key: AcmeChallengeStateKey::from_cert_id(&cert_id),
                challenge,
            },
        })
    );
}

#[test]
fn cert_finish_phase_writes_active_cert_after_acme_success() {
    let cert_id = cert_id("cert_api");

    assert_eq!(
        plan_cert_finish(CertFinishCommand {
            cert_id: cert_id.clone(),
            hostname: hostname("api.example.com"),
            current: None,
            acme_result: AcmeFinishResult::Issued {
                bundle_ref: bundle_ref("obj://PLZ_CERTS/cert_api/rev_2"),
                validity: validity(),
            },
        }),
        Ok(CertFinishPlan::Completed {
            active_cert_write: ActiveCertWrite {
                key: CertStateKey::from_cert_id(&cert_id),
                state: ActiveCertState {
                    cert_id: cert_id.clone(),
                    hostname: hostname("api.example.com"),
                    bundle_ref: bundle_ref("obj://PLZ_CERTS/cert_api/rev_2"),
                    validity: validity(),
                },
            },
        })
    );
}

#[test]
fn cert_finish_failure_keeps_prior_active_cert_and_has_no_active_write() {
    let cert_id = cert_id("cert_api");
    let current = active_cert(
        "cert_api",
        "api.example.com",
        "obj://PLZ_CERTS/cert_api/rev_1",
    );
    let message = FailureMessage::try_new("ACME server rejected challenge").expect("valid failure");

    assert_eq!(
        plan_cert_finish(CertFinishCommand {
            cert_id: cert_id.clone(),
            hostname: hostname("api.example.com"),
            current: Some(current.clone()),
            acme_result: AcmeFinishResult::Failed {
                message: message.clone(),
            },
        }),
        Ok(CertFinishPlan::Failed {
            failure: CertOperationFailure::AcmeValidationFailed {
                cert_id,
                message,
                retained_active_cert: Some(current),
            },
        })
    );
}

#[test]
fn cert_schedule_targets_same_job_subject_as_fallback_worker() {
    let cert_id = cert_id("cert_api");

    assert_eq!(
        ployz_core::subjects::cert_renewal_schedule(&cert_id),
        "plz.v1.sched.cert.renew.cert_api"
    );
    assert_eq!(
        ployz_core::subjects::cert_renewal_job(&cert_id),
        "plz.v1.job.cert.renew.cert_api"
    );
}

#[test]
fn cert_operation_events_project_visible_progress() {
    let operation_id = operation_id("op_cert");
    let cert = cert_id("cert_api");
    let accepted = OperationStatus::cert_accepted(operation_id.clone(), cert, event_sequence(1));

    assert_eq!(
        project_operation_event(
            &accepted,
            OperationEvent::CertChallengePublished {
                operation_id: operation_id.clone(),
                cert_id: cert_id("cert_api"),
                challenge: challenge("api.example.com"),
            },
            event_sequence(2),
        ),
        Ok(OperationEventProjection::StatusChanged {
            status: Box::new(OperationStatus::Cert {
                id: operation_id,
                cert_id: cert_id("cert_api"),
                state: CertOperationState::Running {
                    stage: CertRunningStage::ChallengePublished,
                },
                last_event_sequence: event_sequence(2),
            }),
        })
    );
}

#[test]
fn cert_completion_rejects_active_cert_for_another_cert() {
    let operation_id = operation_id("op_cert");
    let current = OperationStatus::Cert {
        id: operation_id.clone(),
        cert_id: cert_id("cert_api"),
        state: CertOperationState::Running {
            stage: CertRunningStage::ValidationStarted,
        },
        last_event_sequence: event_sequence(3),
    };

    assert_eq!(
        project_operation_event(
            &current,
            OperationEvent::CertCompleted {
                operation_id: operation_id.clone(),
                active_cert: active_cert(
                    "cert_other",
                    "api.example.com",
                    "obj://PLZ_CERTS/cert_other/rev_2"
                ),
            },
            event_sequence(4),
        ),
        Err(StatusProjectionError::OperationSubjectMismatch {
            operation_id,
            expected: OperationSubjectRef::Cert(cert_id("cert_api")),
            actual: OperationSubjectRef::Cert(cert_id("cert_other")),
        })
    );
}

#[test]
fn cert_completion_is_allowed_after_validation_started() {
    let operation_id = operation_id("op_cert");
    let current = OperationStatus::Cert {
        id: operation_id.clone(),
        cert_id: cert_id("cert_api"),
        state: CertOperationState::Running {
            stage: CertRunningStage::ValidationStarted,
        },
        last_event_sequence: event_sequence(3),
    };

    assert_eq!(
        project_operation_event(
            &current,
            OperationEvent::CertCompleted {
                operation_id: operation_id.clone(),
                active_cert: active_cert(
                    "cert_api",
                    "api.example.com",
                    "obj://PLZ_CERTS/cert_api/rev_2"
                ),
            },
            event_sequence(4),
        ),
        Ok(OperationEventProjection::StatusChanged {
            status: Box::new(OperationStatus::Cert {
                id: operation_id,
                cert_id: cert_id("cert_api"),
                state: CertOperationState::Completed,
                last_event_sequence: event_sequence(4),
            }),
        })
    );
}

#[test]
fn cert_renewal_rejects_active_state_for_another_cert() {
    assert_eq!(
        plan_cert_finish(CertFinishCommand {
            cert_id: cert_id("cert_api"),
            hostname: hostname("api.example.com"),
            current: Some(active_cert(
                "cert_other",
                "api.example.com",
                "obj://PLZ_CERTS/cert_other/rev_1"
            )),
            acme_result: AcmeFinishResult::Issued {
                bundle_ref: bundle_ref("obj://PLZ_CERTS/cert_api/rev_2"),
                validity: validity(),
            },
        }),
        Err(CertRenewalPlanError::CurrentCertMismatch {
            expected: cert_id("cert_api"),
            actual: cert_id("cert_other"),
        })
    );
}

#[test]
fn cert_renewal_rejects_active_state_for_another_hostname() {
    assert_eq!(
        plan_cert_finish(CertFinishCommand {
            cert_id: cert_id("cert_api"),
            hostname: hostname("api.example.com"),
            current: Some(active_cert(
                "cert_api",
                "www.example.com",
                "obj://PLZ_CERTS/cert_api/rev_1"
            )),
            acme_result: AcmeFinishResult::Issued {
                bundle_ref: bundle_ref("obj://PLZ_CERTS/cert_api/rev_2"),
                validity: validity(),
            },
        }),
        Err(CertRenewalPlanError::CurrentHostnameMismatch {
            expected: hostname("api.example.com"),
            actual: hostname("www.example.com"),
        })
    );
}

#[test]
fn cert_text_models_reject_invalid_shapes() {
    assert!(AcmeChallengeToken::try_new("").is_err());
    assert!(AcmeChallengeToken::try_new("token/with/slash").is_err());
    assert!(AcmeChallengeValue::try_new("value with spaces").is_err());
    assert!(AcmeChallengeValue::try_new("proof_without_thumbprint").is_err());
    assert!(AcmeChallengeValue::try_new("proof_123.th\u{fc}mprint").is_err());
    assert!(
        AcmeHttp01Challenge::try_new(
            hostname("api.example.com"),
            AcmeChallengeToken::try_new("token_123").expect("valid challenge token"),
            AcmeChallengeValue::try_new("proof_123.thumbprint_456").expect("valid challenge value"),
            AcmeChallengeTtlSeconds::try_new(60).expect("valid challenge ttl"),
        )
        .is_err()
    );
    assert!(CertBundleRef::try_new("file://PLZ_CERTS/cert_api/rev_2").is_err());
    assert!(CertBundleRef::try_new("obj://PLZ_CERTS").is_err());
    assert!(CertBundleRef::try_new("obj://PLZ_CERTS//rev_2").is_err());
    assert!(CertBundleRef::try_new("obj://PLZ_CERTS/cert_api/\u{7}").is_err());
    assert!(AcmeChallengeTtlSeconds::try_new(0).is_err());
    assert!(CertValidityWindow::try_new(valid_at(20), valid_at(10)).is_err());
}

fn active_cert(
    cert_id_value: &str,
    hostname_value: &str,
    bundle_ref_value: &str,
) -> ActiveCertState {
    ActiveCertState {
        cert_id: cert_id(cert_id_value),
        hostname: hostname(hostname_value),
        bundle_ref: bundle_ref(bundle_ref_value),
        validity: validity(),
    }
}

fn challenge(hostname_value: &str) -> AcmeHttp01Challenge {
    AcmeHttp01Challenge::try_new(
        hostname(hostname_value),
        AcmeChallengeToken::try_new("token_123").expect("valid challenge token"),
        AcmeChallengeValue::try_new("token_123.thumbprint_456").expect("valid challenge value"),
        AcmeChallengeTtlSeconds::try_new(60).expect("valid challenge ttl"),
    )
    .expect("valid challenge")
}

fn validity() -> CertValidityWindow {
    CertValidityWindow::try_new(valid_at(1_700_000_000), valid_at(1_707_776_000))
        .expect("valid cert window")
}

fn valid_at(value: u64) -> CertValidAt {
    CertValidAt::try_new(value).expect("valid cert timestamp")
}

fn hostname(value: &str) -> RouteHostname {
    RouteHostname::try_new(value).expect("valid route hostname")
}

fn bundle_ref(value: &str) -> CertBundleRef {
    CertBundleRef::try_new(value).expect("valid cert bundle reference")
}
