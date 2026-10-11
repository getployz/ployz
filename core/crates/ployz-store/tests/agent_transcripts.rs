//! Every tool's example driven to its result against a SQLite Config Store, each
//! step recorded with the answers it got. Ployz Cloud's conformance test replays
//! the same transcripts through the Node SDK and expects the same steps.

use std::path::PathBuf;

use ployz_core::{RpcError, RpcErrorCode};
use ployz_store::tools::{
    Answer, Ask, Place, Step, StoreAt, Write, catalog, example, resume, start,
};
use ployz_store::{
    Actor, ConfigStore, CreateProject, EnvironmentId, OrganizationId, ProjectId, ProjectName,
    SealingKey,
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
    let store = ConfigStore::open(
        "sqlite::memory:",
        SealingKey::new(b"test-encryption-secret").unwrap(),
    )
    .unwrap();
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
    let store = shop();
    let input = example(tool).unwrap();
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
            Step::Read { asks } => asks.iter().map(|each| ask(&store, each)).collect(),
            Step::Wait { then, .. } => then.iter().map(|each| ask(&store, each)).collect(),
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

/// The steps `transcript`'s call takes when fed its recorded answers.
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
            .flat_map(|def| [record(def.name, StoreAt::Cloud), record(def.name, StoreAt::Local)])
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
