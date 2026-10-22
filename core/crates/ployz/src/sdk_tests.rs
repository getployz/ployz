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
        crate::sdk::prepare::PreparationError::Selection(ConnectError::Remote(RpcError {
            code: RpcErrorCode::Unsupported,
            message: "provider token=secret".into(),
            details: serde_json::json!({"rejections":{"builds disabled":2}}),
            cause: Vec::new(),
        })),
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
    let Unprepared { error, same_again } = prepared(conflict());
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
    let Unprepared { error, same_again } = planned(conflict());
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
    let remote = || RpcError {
        code: RpcErrorCode::Unavailable,
        message: "Machine is draining".into(),
        details: serde_json::json!({"same_again": true}),
        cause: Vec::new(),
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
        let Unprepared { error, same_again } = planned(refusal.clone());
        assert!(!same_again, "{message}");
        assert_eq!(error.details, Value::Null, "{message}");
        let Unprepared { error, same_again } = prepared(refusal);
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
