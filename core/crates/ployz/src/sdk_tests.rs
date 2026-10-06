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
