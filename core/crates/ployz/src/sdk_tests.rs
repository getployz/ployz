use super::*;
#[tokio::test]
async fn slow_preparation_consumer_loses_nothing_and_cannot_block_completion() {
    let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
    let join = tokio::spawn(async move {
        for n in 0..1000 {
            events.send((1, serde_json::json!({"n":n}))).unwrap();
        }
        Err(invalid_argument("fixture failure".into()))
    });
    let running = RunningPreparation {
        cancel: CancellationToken::new(),
        events: Mutex::new(receiver),
        buffered: Arc::new(AtomicUsize::new(1000)),
        join: Mutex::new(Some(join)),
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), running.finished())
        .await
        .unwrap();
    assert_eq!(result.unwrap_err().message, "fixture failure");
    let mut count = 0;
    while running.next().await.is_some() {
        count += 1;
    }
    assert_eq!(count, 1000);
    assert_eq!(running.buffered.load(Ordering::Relaxed), 0);
}
#[test]
fn output_beyond_the_budget_becomes_one_marker_until_the_consumer_catches_up() {
    use crate::sdk::prepare::Progress;
    use ployz_build::Progress as Build;
    let buffered = AtomicUsize::new(OUTPUT_BUDGET);
    let mut budget = OutputBudget::default();
    let output = |text: &str| {
        Progress::Build(Build::StepOutput {
            step: "sha256:a".into(),
            stderr: false,
            text: text.into(),
        })
    };
    let (size, marker) = budget.frame(output("cargo output\n"), &buffered).unwrap();
    assert!(marker.to_string().contains("output dropped"), "{marker}");
    assert!(size > 0);
    assert!(budget.frame(output("more\n"), &buffered).is_none());
    // Structured state always passes.
    let (_, step) = budget
        .frame(
            Progress::Build(Build::Step(ployz_build::BuildStep::default())),
            &buffered,
        )
        .unwrap();
    assert!(step.get("Build").is_some());
    // Still dropping above half the budget, even though this frame would fit.
    buffered.store(OUTPUT_BUDGET / 2 + 1, Ordering::Relaxed);
    assert!(budget.frame(output("between\n"), &buffered).is_none());
    buffered.store(0, Ordering::Relaxed);
    let (size, value) = budget.frame(output("after\n"), &buffered).unwrap();
    assert_eq!(size, "after\n".len());
    assert!(value.to_string().contains("after"), "{value}");
}
#[test]
fn selection_failure_is_known_and_does_not_expose_provider_details() {
    let error = preparation_error(
        crate::sdk::prepare::PreparationError::Selection(ConnectError::Remote(RpcError {
            code: RpcErrorCode::Unsupported,
            message: "provider token=secret".into(),
            details: serde_json::json!({"rejections":{"builds disabled":2}}),
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
