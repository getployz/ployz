use super::*;

const PLACE: Place = Place {
    project: None,
    environment: None,
    store: StoreAt::Cloud,
};

fn cx<'run>(rounds: &'run [Round], tool: &str) -> Cx<'run> {
    Cx {
        rounds,
        cursor: 0,
        steps: 0,
        waited_ms: 0,
        slot: Some(WriteSlot::new()),
        place: &PLACE,
        call: Call::new(tool, json!({})),
        call_key: "call-1",
        local_next: None,
    }
}

fn read_round() -> Round {
    Round {
        kind: Kind::Read { count: 0 },
        answers: Vec::new(),
    }
}

fn refused<T>(outcome: Result<T, Stop>) -> Refusal {
    match outcome {
        Err(Stop::Refuse(refusal)) => *refusal,
        Err(Stop::Step(..)) => panic!("expected a refusal, got a step"),
        Ok(_) => panic!("expected a refusal, got an answer"),
    }
}

fn empty_batch() -> Batch {
    Batch {
        environment: EnvironmentRef {
            project: None,
            environment: None,
        },
        commands: Vec::new(),
        expect: None,
    }
}

#[test]
fn a_call_takes_at_most_three_reads_and_writes() {
    let rounds = vec![read_round(), read_round(), read_round(), read_round()];
    let mut cx = cx(&rounds, "environment_show");
    for _ in 0..STEP_BUDGET {
        assert!(cx.read(Vec::new()).is_ok());
    }
    let refusal = refused(cx.read(Vec::new()));
    assert_eq!(refusal.code, RpcErrorCode::Internal);
    assert_eq!(
        refusal.message,
        "A call takes at most three reads and writes"
    );
}

#[test]
fn a_call_writes_at_most_once() {
    let rounds = Vec::new();
    let mut cx = cx(&rounds, "service_add");
    assert!(matches!(
        cx.write(&empty_batch()),
        Err(Stop::Step(step, Kind::Write)) if matches!(*step, Step::Write { .. })
    ));
    let refusal = refused(cx.write(&empty_batch()));
    assert_eq!(refusal.message, "A call writes at most once");
}

#[test]
fn waits_back_off_and_stop_at_the_cap() {
    let rounds = Vec::new();
    let mut cx = cx(&rounds, "deployment_show");
    assert!(matches!(cx.poll(0, 0, Vec::new()), Ok(None)));
    let mut waits = Vec::new();
    for attempt in 0..100 {
        match cx.poll(1_000, attempt, Vec::new()) {
            Ok(None) => break,
            Err(Stop::Step(step, Kind::Wait { ms, .. })) => {
                assert!(matches!(*step, Step::Wait { ms: shown, .. } if shown == ms));
                waits.push(ms);
            }
            Ok(Some(_)) | Err(Stop::Step(..) | Stop::Refuse(_)) => panic!("a poll only waits"),
        }
    }
    assert_eq!(waits.get(..4), Some(&[1_000, 2_000, 4_000, 10_000][..]));
    assert_eq!(waits.iter().sum::<u32>(), WAIT_CAP_SECONDS * 1_000);
}

#[test]
fn an_unknown_write_retries_under_the_same_key_only_when_keyed() {
    let unknown = |tool: &str| {
        let entry = super::super::TOOLS
            .iter()
            .find(|entry| entry.name == tool)
            .unwrap();
        let mut refusal = Refusal::internal("The write may have landed");
        refusal.writes = Writes::Unknown;
        finish(
            refusal,
            entry,
            &Call::new(tool, json!({ "environment": "staging" })),
            "call-1",
        )
    };
    let deploy = unknown("deploy");
    assert_eq!(
        deploy.retry,
        Call::new(
            "deploy",
            json!({
                "environment": "staging",
                "request_key": RequestKey::effective(None, "call-1").as_str(),
            })
        )
    );
    let added = unknown("service_add");
    assert_eq!(
        added.retry,
        Call::new("service_add", json!({ "environment": "staging" }))
    );
}

#[test]
fn a_bad_argument_names_what_fits_and_an_example() {
    let entry = super::super::TOOLS
        .iter()
        .find(|entry| entry.name == "deployment_show")
        .unwrap();
    let refusal = finish(
        Refusal::invalid("Too long", "wait_seconds"),
        entry,
        &Call::new("deployment_show", json!({ "wait_seconds": 9_000 })),
        "call-1",
    );
    assert_eq!(
        refusal.expected.as_ref().and_then(|at| at.get("maximum")),
        Some(&json!(300))
    );
    assert_eq!(refusal.example, Some((entry.example)()));
}

#[test]
fn a_resume_must_answer_the_step_it_was_given() {
    let (step, state) = start("environment_show", json!({}), &PLACE, "call-1");
    let Step::Read { asks } = step else {
        panic!("environment_show reads first");
    };
    let short = asks.iter().skip(1).map(|_| Ok(json!({}))).collect();
    let (step, _) = resume(state.clone(), short);
    let Step::Done {
        result: ToolResult::Refused(refusal),
    } = step
    else {
        panic!("short answers are refused");
    };
    assert_eq!(refusal.code, RpcErrorCode::Internal);
    assert_eq!(refusal.retry, Call::new("environment_show", json!({})));
    let mut idle = state;
    idle.pending = None;
    assert!(matches!(
        resume(idle, Vec::new()).0,
        Step::Done {
            result: ToolResult::Refused(_)
        }
    ));
}
