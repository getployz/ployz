use std::path::PathBuf;

mod backend;

use ployz_core::{RpcError, RpcErrorCode};
use ployz_store::tools::{
    Answer, Ask, Place, Step, StoreAt, Write, catalog, example, resume, start,
};
use ployz_store::{
    Actor, ConfigStore, CreateEnvironment, CreateProject, EnvironmentId, EnvironmentName,
    EnvironmentRef, OrganizationId, ProjectId, ProjectName, SetDefaultEnvironment,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Transcript {
    tool: String,
    input: Value,
    place: Place,
    call_key: String,
    turns: Vec<Turn>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Turn {
    step: Step,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    answers: Vec<Answer>,
}

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/agent_transcripts.json")
}

fn who() -> Actor {
    Actor::system(OrganizationId::parse("org").unwrap())
}

fn shop() -> ConfigStore {
    let store = backend::open();
    store
        .write(
            &who(),
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
            },
        )
        .unwrap();
    store
}

fn no_cloud() -> Answer {
    Answer::Error(RpcError {
        code: RpcErrorCode::Unavailable,
        message: "no Ployz Cloud in this transcript".to_owned(),
        details: Value::Null,
        cause: Vec::new(),
    })
}

fn ask(store: &ConfigStore, ask: &Ask) -> Answer {
    match ask {
        Ask::Store(query) => answered(
            store
                .read(&who(), query)
                .map(|view| serde_json::to_value(view).unwrap()),
        ),
        Ask::Cloud(_) => no_cloud(),
    }
}

fn answered(result: Result<Value, RpcError>) -> Answer {
    match result {
        Ok(value) => Answer::Value(value),
        Err(error) => Answer::Error(error),
    }
}

fn record(tool: &str, store_at: StoreAt) -> Transcript {
    record_on(&shop(), tool, example(tool).unwrap(), store_at)
}

fn record_on(store: &ConfigStore, tool: &str, input: Value, store_at: StoreAt) -> Transcript {
    let place = Place {
        project: Some(ProjectName::parse("shop").unwrap()),
        environment: None,
        store: store_at,
    };
    let call_key = format!("call-{tool}");
    let (mut step, mut state) = start(tool, input.clone(), &place, &call_key);
    let mut turns = Vec::new();
    loop {
        let answers: Vec<Answer> = match &step {
            Step::Read { asks } => asks.iter().map(|each| ask(store, each)).collect(),
            Step::Wait { then, .. } => then.iter().map(|each| ask(store, each)).collect(),
            Step::Write { write } => vec![match write.as_ref() {
                Write::Store(command) => answered(
                    store
                        .write(&who(), command)
                        .map(|written| serde_json::to_value(written).unwrap()),
                ),
                Write::Act(_) => no_cloud(),
            }],
            Step::Done { .. } => {
                turns.push(Turn {
                    step,
                    answers: Vec::new(),
                });
                break;
            }
        };
        let (next, next_state) = resume(state, answers.iter().cloned().map(Into::into).collect());
        turns.push(Turn { step, answers });
        (step, state) = (next, next_state);
    }
    Transcript {
        tool: tool.to_owned(),
        input,
        place,
        call_key,
        turns,
    }
}

fn replay(transcript: &Transcript) -> Vec<Step> {
    let (mut step, mut state) = start(
        &transcript.tool,
        transcript.input.clone(),
        &transcript.place,
        &transcript.call_key,
    );
    let mut steps = Vec::new();
    for turn in &transcript.turns {
        if matches!(step, Step::Done { .. }) {
            steps.push(step);
            break;
        }
        let answers = turn.answers.iter().cloned().map(Into::into).collect();
        let (next, next_state) = resume(state, answers);
        steps.push(step);
        (step, state) = (next, next_state);
    }
    steps
}

#[test]
fn every_transcript_replays_to_its_recorded_steps() {
    let path = snapshot_path();
    if std::env::var_os("PLOYZ_BLESS").is_some() {
        let transcripts: Vec<Transcript> = catalog()
            .iter()
            .flat_map(|def| {
                [
                    record(
                        def.name,
                        StoreAt::Cloud {
                            organization: OrganizationId::parse("org").unwrap(),
                        },
                    ),
                    record(def.name, StoreAt::Local),
                ]
            })
            .collect();
        let mut text = serde_json::to_string_pretty(&transcripts).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
    }
    let saved: Vec<Transcript> =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        saved.len(),
        catalog().len() * 2,
        "a tool was added or removed; rerun with PLOYZ_BLESS=1"
    );
    for transcript in &saved {
        let recorded: Vec<&Step> = transcript.turns.iter().map(|turn| &turn.step).collect();
        let replayed = replay(transcript);
        assert_eq!(
            replayed.iter().collect::<Vec<_>>(),
            recorded,
            "{} at {:?} changed; rerun with PLOYZ_BLESS=1 and review tests/snapshots/agent_transcripts.json",
            transcript.tool,
            transcript.place.store
        );
    }
}

#[test]
fn deploy_with_skip_deploy_saves_everything_staged_and_queues_nothing() {
    let store = shop();
    let added = record_on(
        &store,
        "service_add",
        example("service_add").unwrap(),
        StoreAt::Local,
    );
    assert!(matches!(
        added.turns.last().unwrap().step,
        Step::Done { .. }
    ));
    let saving = |store: &ConfigStore, key: &str| {
        record_on(
            store,
            "deploy",
            serde_json::json!({ "skip_deploy": true, "request_key": key }),
            StoreAt::Local,
        )
    };
    let writes = |transcript: &Transcript| {
        transcript
            .turns
            .iter()
            .filter_map(|turn| {
                let Step::Write { write } = &turn.step else {
                    return None;
                };
                Some(serde_json::to_value(write).unwrap())
            })
            .collect::<Vec<_>>()
    };
    let result = |transcript: &Transcript| {
        let step = &transcript.turns.last().unwrap().step;
        let Step::Done { result } = step else {
            panic!("not done: {step:?}");
        };
        serde_json::to_value(result).unwrap()
    };

    let first = saving(&store, "save-first");
    let written = writes(&first);
    assert_eq!(written.len(), 1, "{written:?}");
    assert_eq!(
        written
            .first()
            .and_then(|write| write.pointer("/write/command")),
        Some(&serde_json::json!("publish"))
    );
    let done = result(&first);
    assert_eq!(
        done.pointer("/value/saved/revision"),
        Some(&serde_json::json!(1)),
        "{done}"
    );
    assert_eq!(
        done.pointer("/value/saved/created"),
        Some(&serde_json::json!(true)),
        "{done}"
    );

    let staged = record_on(
        &store,
        "service_update",
        serde_json::json!({ "service": "whoami", "set": { "env": { "LOG_LEVEL": "info" } } }),
        StoreAt::Local,
    );
    assert!(matches!(
        staged.turns.last().unwrap().step,
        Step::Done { .. }
    ));
    // A lost reply's retry answers the first save again; what was staged since stays staged.
    let replayed = result(&saving(&store, "save-first"));
    assert_eq!(
        replayed.pointer("/value/saved"),
        done.pointer("/value/saved"),
        "{replayed}"
    );
    let next = result(&saving(&store, "save-second"));
    assert_eq!(
        next.pointer("/value/saved"),
        Some(&serde_json::json!({ "revision": 2, "created": true })),
        "{next}"
    );
}

#[test]
fn a_replayed_save_refuses_once_the_default_environment_moved() {
    let store = shop();
    store
        .write(
            &who(),
            &CreateEnvironment {
                id: EnvironmentId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                project: None,
                name: EnvironmentName::parse("staging").unwrap(),
            },
        )
        .unwrap();
    let call = |tool: &str, input: Value| {
        let transcript = record_on(&store, tool, input, StoreAt::Local);
        let step = &transcript.turns.last().unwrap().step;
        let Step::Done { result } = step else {
            panic!("not done: {step:?}");
        };
        serde_json::to_value(result).unwrap()
    };
    let save = || {
        call(
            "deploy",
            serde_json::json!({ "skip_deploy": true, "request_key": "save-first" }),
        )
    };

    call("service_add", example("service_add").unwrap());
    let first = save();
    assert_eq!(
        first.pointer("/value/at"),
        Some(&serde_json::json!("shop/production")),
        "{first}"
    );

    store
        .write(
            &who(),
            &SetDefaultEnvironment {
                environment: EnvironmentRef {
                    project: None,
                    environment: Some(EnvironmentName::parse("staging").unwrap()),
                },
            },
        )
        .unwrap();
    call("service_add", example("service_add").unwrap());
    // The same request now names staging: production's save is not its answer.
    let again = save();
    assert_eq!(
        again.pointer("/refusal/code"),
        Some(&serde_json::json!("conflict")),
        "{again}"
    );
}
