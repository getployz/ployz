use super::*;

#[test]
fn build_failures_show_readable_messages_and_keep_structured_evidence() {
    for (outcome, kind) in [
        (
            crate::build::RemoteBuildFailure::Failed {
                stage: ployz_build::Stage::Building,
                message: "the build command exited with code 23".into(),
                work: ployz_build::WorkEvidence::default(),
            },
            "failed",
        ),
        (
            crate::build::RemoteBuildFailure::Unknown {
                stage: ployz_build::Stage::Building,
                message: "lost the build stream".into(),
                work: ployz_build::WorkEvidence::default(),
            },
            "unknown",
        ),
    ] {
        let error = preparation_error(
            crate::sdk::prepare::PreparationError::Build(crate::build::Error::RemoteBuild {
                outcome: Box::new(outcome),
            }),
            false,
        );
        assert!(error.message.to_lowercase().contains(kind));
        let diagnosis = error
            .details
            .pointer("/preparation/message")
            .unwrap()
            .as_str()
            .unwrap();
        assert!(error.message.contains(diagnosis));
        assert!(!error.message.contains("WorkEvidence"));
        assert!(!error.message.contains("stage:"));
        assert_eq!(error.details.pointer("/preparation/kind").unwrap(), kind);
        assert_eq!(
            error.details.pointer("/preparation/stage").unwrap(),
            "Building"
        );
        assert!(
            error
                .details
                .pointer("/preparation/work")
                .unwrap()
                .is_object()
        );
    }
}
#[test]
fn a_failed_build_says_build_failed_once_before_its_diagnosis() {
    let error = preparation_error(
        crate::sdk::prepare::PreparationError::Build(crate::build::Error::RemoteBuild {
            outcome: Box::new(crate::build::RemoteBuildFailure::Failed {
                stage: ployz_build::Stage::Building,
                message: r#"npm run build: process "npm run build" did not complete successfully: exit code: 3"#.into(),
                work: ployz_build::WorkEvidence::default(),
            }),
        }),
        false,
    );
    assert_eq!(
        error.message,
        r#"The Build failed, so no Service, hook, or volume change was attempted: npm run build: process "npm run build" did not complete successfully: exit code: 3"#
    );
}

#[test]
fn a_failed_build_call_reports_only_the_diagnosis() {
    let error = crate::sdk::build::build_error(
        crate::sdk::prepare::PreparationError::Build(crate::build::Error::RemoteBuild {
            outcome: Box::new(crate::build::RemoteBuildFailure::Failed {
                stage: ployz_build::Stage::Building,
                message: r#"npm run build: process "npm run build" did not complete successfully: exit code: 3"#.into(),
                work: ployz_build::WorkEvidence::default(),
            }),
        }),
        false,
    );
    assert_eq!(
        error.message,
        r#"npm run build: process "npm run build" did not complete successfully: exit code: 3"#
    );
    assert_eq!(
        error.details.pointer("/preparation/kind").unwrap(),
        "failed"
    );
}

#[test]
fn selection_failure_is_known_and_does_not_expose_provider_details() {
    let error = preparation_error(
        crate::sdk::prepare::PreparationError::Selection(ConnectError::Remote(
            RpcError::unsupported("provider token=secret")
                .with_details(serde_json::json!({"rejections":{"builds disabled":2}})),
        )),
        false,
    );
    assert_eq!(
        error.details.pointer("/preparation/kind").unwrap(),
        "failed"
    );
    assert_eq!(
        error
            .details
            .pointer("/preparation/rejections/builds disabled")
            .unwrap(),
        2
    );
    assert!(!error.message.contains("secret"));
    assert!(!error.details.to_string().contains("secret"));
    assert!(error.cause.iter().all(|cause| !cause.contains("secret")));
}

#[test]
fn requested_cancellation_preserves_unknown_stage_and_evidence() {
    let error = preparation_error(
        crate::sdk::prepare::PreparationError::Build(crate::build::Error::RemoteBuild {
            outcome: Box::new(crate::build::RemoteBuildFailure::Unknown {
                stage: ployz_build::Stage::Building,
                message: "lost stream".into(),
                work: ployz_build::WorkEvidence::default(),
            }),
        }),
        true,
    );
    assert_eq!(
        error.details.pointer("/preparation/kind").unwrap(),
        "unknown"
    );
    assert_eq!(
        error.details.pointer("/preparation/stage").unwrap(),
        "Building"
    );
    assert!(
        error
            .details
            .pointer("/preparation/work")
            .unwrap()
            .is_object()
    );
}

fn planned(error: crate::deploy::PlanError) -> Unprepared {
    Unprepared::from(crate::deploy::DeployError::Plan(error))
}

fn prepared(error: crate::deploy::PlanError) -> Unprepared {
    unprepared(
        crate::sdk::prepare::PreparationError::Plan(crate::deploy::DeployError::Plan(error)),
        false,
    )
}

#[test]
fn a_hostname_two_requested_services_claim_fails_the_same_way_again() {
    use crate::deploy::{HostnameRefusal, PlanError};
    use ployz_core::{IngressHost, Namespace, QualifiedService, ServiceName};
    let conflict = || PlanError::HostnameConflict {
        hostname: IngressHost::parse("shop.example.com").unwrap(),
        owner: QualifiedService::new(
            Namespace::parse("shop").unwrap(),
            ServiceName::parse("web").unwrap(),
        ),
        refusal: HostnameRefusal::RequestedTwice {
            to: ServiceName::parse("api").unwrap(),
        },
    };
    let Unprepared {
        error, same_again, ..
    } = prepared(conflict());
    assert!(same_again);
    assert_eq!(error.code, RpcErrorCode::Internal);
    assert!(
        error
            .message
            .contains("shop.example.com is requested by both shop/web and api"),
        "{}",
        error.message
    );
    assert_eq!(
        error.details.pointer("/preparation/kind").unwrap(),
        "failed"
    );
    assert_eq!(error.details.get("same_again"), Some(&Value::Bool(true)));
    // An image-only Deploy plans without preparing: its refusal says the same.
    let Unprepared {
        error, same_again, ..
    } = planned(conflict());
    assert!(same_again);
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert_eq!(error.details.get("same_again"), Some(&Value::Bool(true)));
    let in_service = PlanError::Service {
        service: "shop".into(),
        source: Box::new(PlanError::DependencyCycle {
            service: "web".into(),
        }),
    };
    assert!(prepared(in_service).same_again);
}

#[test]
fn a_remote_errors_own_same_again_key_suppresses_nothing() {
    let remote = || {
        RpcError::unavailable("Machine is draining")
            .with_details(serde_json::json!({"same_again": true}))
    };
    let passed_through = |unprepared: Unprepared| {
        assert!(!unprepared.same_again);
        assert_eq!(
            unprepared.error.details.get("same_again"),
            Some(&Value::Bool(true))
        );
    };
    passed_through(Unprepared::from(crate::deploy::DeployError::Connect(
        crate::connect::ConnectError::Remote(remote()),
    )));
    passed_through(Unprepared::from(remote()));
}

#[test]
fn a_refusal_a_retry_may_get_past_does_not_claim_to_fail_the_same_way_again() {
    use crate::deploy::{HostnameRefusal, PlanError};
    use ployz_core::{DockerVolumeName, IngressHost, QualifiedService, ServiceName};
    for refusal in [
        PlanError::CapacityUnknown,
        PlanError::InsufficientCapacity,
        PlanError::NoEligibleMachines {
            constraints: crate::deploy::EliminatingConstraints::new(Vec::new()),
        },
        PlanError::HostPortConflict {
            service: ServiceName::parse("web").unwrap(),
        },
        PlanError::HostnameConflict {
            hostname: IngressHost::parse("shop.example.com").unwrap(),
            owner: QualifiedService::parse("blog/web").unwrap(),
            refusal: HostnameRefusal::StillPublished,
        },
        PlanError::ConflictingDockerVolumeDefinitions {
            name: DockerVolumeName::parse("app_data").unwrap(),
        },
    ] {
        let message = refusal.to_string();
        let Unprepared {
            error, same_again, ..
        } = planned(refusal.clone());
        assert!(!same_again, "{message}");
        assert_eq!(error.details, Value::Null, "{message}");
        let Unprepared {
            error, same_again, ..
        } = prepared(refusal);
        assert!(!same_again, "{message}");
        assert_eq!(error.details.get("same_again"), None, "{message}");
    }
    let delivery = unprepared(
        crate::sdk::prepare::PreparationError::Delivery("the image never arrived".into()),
        false,
    );
    assert!(!delivery.same_again);
    assert_eq!(delivery.error.details.get("same_again"), None);
}

/// Each arm's wire `details`, byte for byte: the keys are frozen for every reader of an
/// old or new daemon's answer.
fn details_text(error: &RpcError) -> String {
    serde_json::to_string(&error.details).unwrap()
}

fn remote_build(
    outcome: crate::build::RemoteBuildFailure,
    cancellation_requested: bool,
) -> RpcError {
    preparation_error(
        crate::sdk::prepare::PreparationError::Build(crate::build::Error::RemoteBuild {
            outcome: Box::new(outcome),
        }),
        cancellation_requested,
    )
}

fn work() -> ployz_build::WorkEvidence {
    ployz_build::WorkEvidence(
        [
            ("api".to_owned(), ployz_build::TargetEvidence::Unattempted),
            ("web".to_owned(), ployz_build::TargetEvidence::Validated),
        ]
        .into(),
    )
}

#[test]
fn golden_selection_details() {
    let remote = |details| {
        crate::sdk::prepare::PreparationError::Selection(ConnectError::Remote(
            RpcError::unsupported("provider token=secret").with_details(details),
        ))
    };
    let with_reasons = preparation_error(
        remote(serde_json::json!({
            "rejections": {"builds disabled": 2},
            "reasons": ["no Build Machine", "all drained"],
        })),
        false,
    );
    assert_eq!(with_reasons.code, RpcErrorCode::Unavailable);
    assert_eq!(
        with_reasons.message,
        "No eligible Build Machine was selected; no build was started."
    );
    assert_eq!(with_reasons.cause, ["no Build Machine; all drained"]);
    assert_eq!(
        details_text(&with_reasons),
        r#"{"preparation":{"kind":"failed","message":"No eligible Build Machine was selected; no build was started.","rejections":{"builds disabled":2},"stage":"Selection"}}"#
    );
    let bare = preparation_error(remote(Value::Null), false);
    assert!(bare.cause.is_empty());
    assert_eq!(
        details_text(&bare),
        r#"{"preparation":{"kind":"failed","message":"No eligible Build Machine was selected; no build was started.","rejections":null,"stage":"Selection"}}"#
    );
    let local = preparation_error(
        crate::sdk::prepare::PreparationError::Selection(ConnectError::EntryNotReady),
        true,
    );
    assert_eq!(details_text(&local), details_text(&bare));
}

#[test]
fn golden_observation_details() {
    let error = preparation_error(
        crate::sdk::prepare::PreparationError::Connect(ConnectError::EntryNotReady),
        false,
    );
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(
        error.message,
        "Could not read Machine observations during preparation."
    );
    assert_eq!(
        details_text(&error),
        r#"{"preparation":{"kind":"failed","message":"Could not read Machine observations during preparation.","stage":"Observation"}}"#
    );
}

#[test]
fn golden_remote_build_details() {
    let failed = || crate::build::RemoteBuildFailure::Failed {
        stage: ployz_build::Stage::Push,
        message: "denied".into(),
        work: work(),
    };
    let unknown = || crate::build::RemoteBuildFailure::Unknown {
        stage: ployz_build::Stage::Upload,
        message: "lost the stream".into(),
        work: work(),
    };
    assert_eq!(
        details_text(&remote_build(failed(), false)),
        r#"{"preparation":{"kind":"failed","message":"denied","stage":"Push","work":{"api":"Unattempted","web":"Validated"}}}"#
    );
    assert_eq!(
        details_text(&remote_build(unknown(), false)),
        r#"{"preparation":{"kind":"unknown","message":"lost the stream","stage":"Upload","work":{"api":"Unattempted","web":"Validated"}}}"#
    );
    // Only a confirmed failure becomes `cancelled`; an unknown outcome stays unknown.
    assert_eq!(
        details_text(&remote_build(failed(), true)),
        r#"{"preparation":{"kind":"cancelled","message":"denied","stage":"Push","work":{"api":"Unattempted","web":"Validated"}}}"#
    );
    assert_eq!(
        details_text(&remote_build(unknown(), true)),
        r#"{"preparation":{"kind":"unknown","message":"lost the stream","stage":"Upload","work":{"api":"Unattempted","web":"Validated"}}}"#
    );
    assert_eq!(remote_build(failed(), true).code, RpcErrorCode::Internal);
}

#[test]
fn golden_cancelled_details() {
    let error = preparation_error(crate::sdk::prepare::PreparationError::Cancelled, true);
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(
        error.message,
        "preparation cancelled; no application changes attempted"
    );
    assert_eq!(
        details_text(&error),
        r#"{"preparation":{"kind":"cancelled"}}"#
    );
}

#[test]
fn golden_upload_needed_details() {
    let services = vec![
        ployz_core::ServiceName::parse("one").unwrap(),
        ployz_core::ServiceName::parse("two").unwrap(),
    ];
    let error = preparation_error(
        crate::sdk::prepare::PreparationError::UploadNeeded(services.clone()),
        false,
    );
    assert_eq!(error.code, RpcErrorCode::NotFound);
    assert_eq!(
        details_text(&error),
        r#"{"preparation":{"kind":"upload_needed","services":["one","two"]}}"#
    );
    assert_eq!(
        crate::sdk::preparation::needs_upload(&error),
        Some(services)
    );
}

#[test]
fn golden_generic_failure_and_same_again_details() {
    use crate::deploy::{HostnameRefusal, PlanError};
    use ployz_core::{IngressHost, Namespace, QualifiedService, ServiceName};
    let conflict = PlanError::HostnameConflict {
        hostname: IngressHost::parse("shop.example.com").unwrap(),
        owner: QualifiedService::new(
            Namespace::parse("shop").unwrap(),
            ServiceName::parse("web").unwrap(),
        ),
        refusal: HostnameRefusal::RequestedTwice {
            to: ServiceName::parse("api").unwrap(),
        },
    };
    assert_eq!(
        details_text(&prepared(conflict.clone()).error),
        r#"{"preparation":{"kind":"failed","message":"hostname shop.example.com is requested by both shop/web and api: keep it on one of them"},"same_again":true}"#
    );
    assert_eq!(
        details_text(&planned(conflict).error),
        r#"{"same_again":true}"#
    );
    assert_eq!(
        details_text(&prepared(PlanError::CapacityUnknown).error),
        r#"{"preparation":{"kind":"failed","message":"capacity unknown: an eligible Machine did not return fresh bridge telemetry"}}"#
    );
    assert_eq!(
        details_text(&planned(PlanError::CapacityUnknown).error),
        r#"null"#
    );
    let delivery = unprepared(
        crate::sdk::prepare::PreparationError::Delivery("the image never arrived".into()),
        false,
    );
    assert_eq!(
        details_text(&delivery.error),
        r#"{"preparation":{"kind":"failed","message":"the image never arrived"}}"#
    );
    let invalid = unprepared(
        crate::sdk::prepare::PreparationError::Build(crate::build::Error::Invalid(
            "no Dockerfile".into(),
        )),
        false,
    );
    assert_eq!(
        details_text(&invalid.error),
        r#"{"preparation":{"kind":"failed","message":"The Build failed, so no Service, hook, or volume change was attempted: invalid Build: no Dockerfile"}}"#
    );
}

#[test]
fn a_role_wait_that_gave_up_is_unavailable_and_a_connection_failure_keeps_its_own_code() {
    use crate::cluster::{RoleSetting, RoleWaitError};
    for error in [
        RoleWaitError::NotObserved(RoleSetting::IngressOn),
        RoleWaitError::ChangeNotObserved,
        RoleWaitError::Cancelled,
    ] {
        let text = error.to_string();
        let refused = RpcError::from(error);
        assert_eq!(refused.code, RpcErrorCode::Unavailable);
        assert_eq!(refused.message, text);
    }
    assert_eq!(
        RpcError::from(RoleWaitError::ChangeNotObserved).message,
        "this entry Server has not yet listed the change"
    );
    assert_eq!(
        RpcError::from(RoleWaitError::Cancelled).message,
        "cancelled while waiting for the new role"
    );
    assert_eq!(
        RpcError::from(RoleWaitError::Connect(ConnectError::EntryNotReady)).code,
        RpcError::from(ConnectError::EntryNotReady).code
    );
}
