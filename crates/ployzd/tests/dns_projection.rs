use ployz_test_support::ids::route_hostname;
use ployzd::roles::dns::projection::{
    DnsAnswer, DnsProjection, DnsProjectionError, DnsProjectionInput, DnsProjectionState,
    DnsProjectionUpdate, DnsProjector, DnsRecordSet, DnsServingState, apply_dns_update,
    project_dns,
};

#[test]
fn dns_projection_sorts_records_and_deduplicates_answers() {
    let projection = project_dns(DnsProjectionInput {
        records: vec![
            record("www.example.com", ["203.0.113.20"]),
            record(
                "api.example.com",
                ["203.0.113.10", "203.0.113.10", "203.0.113.11"],
            ),
            record("API.example.com", ["2001:db8::10", "203.0.113.11"]),
        ],
    });

    assert_eq!(
        projection,
        DnsProjection {
            records: vec![
                record(
                    "api.example.com",
                    ["203.0.113.10", "203.0.113.11", "2001:db8::10"]
                ),
                record("www.example.com", ["203.0.113.20"]),
            ],
        }
    );
}

#[test]
fn dns_keeps_last_good_projection_when_source_is_unavailable() {
    let last_good = DnsProjection {
        records: vec![record("api.example.com", ["203.0.113.10"])],
    };

    assert_eq!(
        apply_dns_update(
            current_state(last_good),
            DnsProjectionUpdate::SourceUnavailable,
        ),
        failed_state(
            DnsProjection {
                records: vec![record("api.example.com", ["203.0.113.10"])],
            },
            DnsProjectionError::InvalidSource {
                message: "DNS source unavailable".to_owned(),
            },
        )
    );
    assert_eq!(
        apply_dns_update(
            DnsProjectionState::unavailable(),
            DnsProjectionUpdate::SourceUnavailable,
        ),
        DnsProjectionState::unavailable()
    );
}

#[test]
fn dns_retains_last_good_projection_when_source_is_invalid() {
    let last_good = DnsProjection {
        records: vec![record("api.example.com", ["203.0.113.10"])],
    };
    let error = DnsProjectionError::InvalidSource {
        message: "decode failed".to_owned(),
    };

    assert_eq!(
        apply_dns_update(
            current_state(last_good),
            DnsProjectionUpdate::SourceInvalid(error.clone()),
        ),
        failed_state(
            DnsProjection {
                records: vec![record("api.example.com", ["203.0.113.10"])],
            },
            error,
        )
    );
}

#[test]
fn dns_keeps_failure_evidence_when_invalid_source_then_disappears() {
    let last_good = DnsProjection {
        records: vec![record("api.example.com", ["203.0.113.10"])],
    };
    let error = DnsProjectionError::InvalidSource {
        message: "decode failed".to_owned(),
    };

    let failed = apply_dns_update(
        current_state(last_good),
        DnsProjectionUpdate::SourceInvalid(error.clone()),
    );

    assert_eq!(
        apply_dns_update(failed, DnsProjectionUpdate::SourceUnavailable),
        failed_state(
            DnsProjection {
                records: vec![record("api.example.com", ["203.0.113.10"])],
            },
            error,
        )
    );
}

#[test]
fn dns_runtime_keeps_serving_last_good_answers_after_source_disappears() {
    let mut runtime = DnsProjector::new();
    let hostname = route_hostname("api.example.com");
    let first_tick =
        runtime.apply_source_update(DnsProjectionUpdate::SourceAvailable(DnsProjectionInput {
            records: vec![record("api.example.com", ["203.0.113.10"])],
        }));

    assert_eq!(
        first_tick.serving,
        DnsServingState::Current { record_count: 1 }
    );
    assert_eq!(
        runtime.answers().answers_for(&hostname),
        &[DnsAnswer::try_new("203.0.113.10").expect("valid answer")]
    );

    let second_tick = runtime.apply_source_update(DnsProjectionUpdate::SourceUnavailable);

    assert!(matches!(
        second_tick.serving,
        DnsServingState::LastKnownGood {
            record_count: 1,
            ..
        }
    ));
    assert_eq!(
        runtime.answers().answers_for(&hostname),
        &[DnsAnswer::try_new("203.0.113.10").expect("valid answer")]
    );
}

#[test]
fn dns_answers_reject_empty_and_whitespace_values() {
    assert_eq!(
        DnsAnswer::try_new(""),
        Err(ployzd::roles::dns::projection::DnsAnswerError::Empty)
    );
    assert!(DnsAnswer::try_new("not-an-address").is_err());
    assert!(DnsAnswer::try_new("203.0.113.10 203.0.113.11").is_err());
    assert_eq!(
        DnsAnswer::try_new("2001:db8::10")
            .expect("valid ipv6 answer")
            .render(),
        "2001:db8::10"
    );
}

fn record<const N: usize>(hostname: &str, answers: [&str; N]) -> DnsRecordSet {
    DnsRecordSet {
        hostname: route_hostname(hostname),
        answers: answers
            .into_iter()
            .map(|answer| DnsAnswer::try_new(answer).expect("valid DNS answer"))
            .collect(),
    }
}

fn current_state(projection: DnsProjection) -> DnsProjectionState {
    DnsProjectionState {
        last_good: Some(projection),
        last_error: None,
    }
}

fn failed_state(projection: DnsProjection, error: DnsProjectionError) -> DnsProjectionState {
    DnsProjectionState {
        last_good: Some(projection),
        last_error: Some(error),
    }
}
