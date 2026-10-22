use super::*;
use crate::sdk::preparation::same_again;

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

fn plan_refusal(error: crate::deploy::PlanError) -> RpcError {
    preparation_error(
        crate::sdk::prepare::PreparationError::Plan(crate::deploy::DeployError::Plan(error)),
        false,
    )
}

#[test]
fn a_hostname_another_service_publishes_fails_the_same_way_again() {
    use crate::deploy::{HostnameRefusal, PlanError};
    use ployz_core::{IngressHost, Namespace, QualifiedService, ServiceName};
    let conflict = || PlanError::HostnameConflict {
        hostname: IngressHost::parse("shop.example.com").unwrap(),
        owner: QualifiedService::new(
            Namespace::parse("blog").unwrap(),
            ServiceName::parse("web").unwrap(),
        ),
        refusal: HostnameRefusal::StillPublished,
    };
    let error = plan_refusal(conflict());
    assert_eq!(error.code, RpcErrorCode::Internal);
    assert!(
        error
            .message
            .contains("shop.example.com is already published by blog/web"),
        "{}",
        error.message
    );
    assert_eq!(
        error.details.pointer("/preparation/kind").unwrap(),
        "failed"
    );
    assert!(same_again(&error));
    // An image-only Deploy plans without preparing: its refusal says the same.
    let planned = RpcError::from(crate::deploy::DeployError::Plan(conflict()));
    assert_eq!(planned.code, RpcErrorCode::InvalidArgument);
    assert!(same_again(&planned));
    let in_service = PlanError::Service {
        service: "shop".into(),
        source: Box::new(PlanError::DependencyCycle {
            service: "web".into(),
        }),
    };
    assert!(same_again(&plan_refusal(in_service)));
}

#[test]
fn a_refusal_a_retry_may_get_past_does_not_claim_to_fail_the_same_way_again() {
    use crate::deploy::PlanError;
    use ployz_core::ServiceName;
    for refusal in [
        PlanError::CapacityUnknown,
        PlanError::InsufficientCapacity,
        PlanError::NoEligibleMachines {
            constraints: crate::deploy::EliminatingConstraints::new(Vec::new()),
        },
        PlanError::HostPortConflict {
            service: ServiceName::parse("web").unwrap(),
        },
    ] {
        let message = refusal.to_string();
        let planned = refusal.clone().into_rpc_error();
        assert!(!same_again(&planned), "{message}");
        assert_eq!(planned.details, serde_json::Value::Null, "{message}");
        let error = plan_refusal(refusal);
        assert!(!same_again(&error), "{message}");
        assert_eq!(error.details.get("same_again"), None, "{message}");
    }
    let delivery = preparation_error(
        crate::sdk::prepare::PreparationError::Delivery("the image never arrived".into()),
        false,
    );
    assert!(!same_again(&delivery));
    let unknown = RpcError {
        code: RpcErrorCode::Unknown("in_rotation".into()),
        message: "Machine is in rotation".into(),
        details: serde_json::json!({"preparation": {"kind": "failed"}}),
        cause: Vec::new(),
    };
    assert!(!same_again(&unknown));
    let internal = RpcError {
        code: RpcErrorCode::Internal,
        message: "runner lost".into(),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    };
    assert!(!same_again(&internal));
}
