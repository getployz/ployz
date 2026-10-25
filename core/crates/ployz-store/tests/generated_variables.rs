#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Generated variables through the Store's interface only, on SQLite and on Postgres
//! (see `backend`): `${{ secret(…) }}` makes a plain value once per Environment, a
//! Branch's Own Copy makes its own, a Live Node's is its Parent's, and Sync and Follow
//! move the generator, never the value. Values are compared, never printed.

use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Change, ConfigStore, CreateBranch, CreateProject, CreateService, Edit, EnvironmentId,
    EnvironmentName, EnvironmentRef, OrganizationId, ProjectId, ProjectName, ServiceLineageId,
    ServiceQuery, SettingPath, SyncChanges, SyncQuery, SyncView, UndoSync,
};
use serde_json::{Value, json};

mod backend;

use backend::deploy;

const LETTERS: &str = "${{ secret(32, \"a-zA-Z\") }}";

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn at(environment: &str) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    }
}

/// production: `db` with a generated `PASSWORD`, and `web` reading it.
fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(1)).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
            },
        )
        .unwrap();
    for (n, name, image) in [(3, "web", "web:1"), (4, "db", "postgres:17")] {
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(uuid(n)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: Some(image.into()),
                    template: None,
                },
            )
            .unwrap();
    }
    set(
        &store,
        &who,
        "production",
        &[
            ("db.env.PASSWORD", json!(LETTERS)),
            ("web.env.DB_PASS", json!("${{ db.PASSWORD }}")),
        ],
    )
    .unwrap();
    (store, who)
}

fn set(
    store: &ConfigStore,
    who: &Actor,
    environment: &str,
    changes: &[(&str, Value)],
) -> Result<(), ployz_core::RpcError> {
    store
        .write(
            who,
            &Edit {
                environment: at(environment),
                expect: None,
                changes: changes
                    .iter()
                    .map(|(path, value)| Change::Set {
                        path: SettingPath::parse(path).unwrap(),
                        value: value.clone(),
                    })
                    .collect(),
            },
        )
        .map(drop)
}

/// A Branch `name` of production with Own Copies of `copy` and the rest live.
fn branch(store: &ConfigStore, who: &Actor, n: u8, name: &str, copy: &[&str]) {
    store
        .write(
            who,
            &CreateBranch {
                id: EnvironmentId::parse(uuid(n)).unwrap(),
                from: at("production"),
                name: EnvironmentName::parse(name).unwrap(),
                copy: copy
                    .iter()
                    .map(|name| ployz_store::NodeName::parse(name).unwrap())
                    .collect(),
                live: Vec::new(),
                setup: Vec::new(),
                keep: false,
                fix: None,
            },
        )
        .unwrap();
}

/// Variable `key` of `service` in `environment`, as reads show it.
fn env(store: &ConfigStore, who: &Actor, environment: &str, service: &str, key: &str) -> Value {
    store
        .read(
            who,
            &ServiceQuery {
                environment: at(environment),
                service: ServiceName::parse(service).unwrap(),
            },
        )
        .unwrap()
        .values["env"][key]
        .clone()
}

fn password(store: &ConfigStore, who: &Actor, environment: &str) -> String {
    env(store, who, environment, "db", "PASSWORD")
        .as_str()
        .unwrap()
        .to_owned()
}

/// `value` is `length` ASCII letters, checked without printing it.
fn letters(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_alphabetic())
}

/// What a Deploy of `environment` gives `service` as `key`.
fn deployed(store: &ConfigStore, who: &Actor, environment: &str, n: u8, key: &str) -> Value {
    let input = deploy(store, who, environment, n);
    let id = store
        .read(
            who,
            &ServiceQuery {
                environment: at(environment),
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap()
        .service
        .service
        .id
        .to_string();
    input["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|snapshot| snapshot["serviceId"] == id)
        .unwrap()["resolvedEnv"][key]
        .clone()
}

fn offered(store: &ConfigStore, who: &Actor, from: &str, into: &str) -> SyncView {
    store
        .read(
            who,
            &SyncQuery {
                from: at(from),
                into: Some(at(into)),
            },
        )
        .unwrap()
}

fn labels(view: &SyncView) -> Vec<String> {
    let mut labels: Vec<String> = view.rows.iter().map(|row| row.at.to_string()).collect();
    labels.sort_unstable();
    labels
}

#[test]
fn a_generated_value_is_made_once_and_asking_again_keeps_it() {
    let (store, who) = shop();
    let first = password(&store, &who, "production");
    assert!(letters(&first, 32));

    // Asking again, or setting back the value reads show, keeps it and its generator.
    set(
        &store,
        &who,
        "production",
        &[("db.env.PASSWORD", json!(LETTERS))],
    )
    .unwrap();
    set(
        &store,
        &who,
        "production",
        &[("db.env.PASSWORD", json!(first))],
    )
    .unwrap();
    set(
        &store,
        &who,
        "production",
        &[("db.env.PASSWORD", json!(LETTERS))],
    )
    .unwrap();
    assert!(password(&store, &who, "production") == first, "rotated");

    // Another generator is a manual rotation.
    set(
        &store,
        &who,
        "production",
        &[("db.env.PASSWORD", json!("${{ secret(48, \"a-z\") }}"))],
    )
    .unwrap();
    let second = password(&store, &who, "production");
    assert!(second.len() == 48 && second.bytes().all(|byte| byte.is_ascii_lowercase()));

    // Redeploys never make a new one.
    let once = deployed(&store, &who, "production", 1, "DB_PASS");
    let twice = deployed(&store, &who, "production", 2, "DB_PASS");
    assert!(
        once == json!(second) && twice == once,
        "a deploy changed it"
    );
}

#[test]
fn plain_text_replaces_a_generator_and_an_escaped_call_is_text() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("db.env.PASSWORD", json!("plain"))],
    )
    .unwrap();
    set(
        &store,
        &who,
        "production",
        &[("db.env.LITERAL", json!("$${{ secret(32) }}"))],
    )
    .unwrap();
    assert_eq!(
        env(&store, &who, "production", "db", "LITERAL"),
        json!("$${{ secret(32) }}")
    );
    // A plain value is copied as it is.
    branch(&store, &who, 9, "fix-db", &["db"]);
    assert_eq!(
        env(&store, &who, "fix-db", "db", "PASSWORD"),
        json!("plain")
    );
}

#[test]
fn a_secret_call_the_store_cannot_take_is_refused_without_repeating_it() {
    let (store, who) = shop();
    for value in [
        "pre-${{ secret() }}",
        "${{ secret(7) }}",
        "${{ secret(32, \"q\") }}",
    ] {
        let error = set(
            &store,
            &who,
            "production",
            &[("db.env.OTHER", json!(value))],
        )
        .unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument);
        assert!(error.message.starts_with("OTHER: "), "{}", error.message);
        assert!(!error.message.contains("pre-"), "{}", error.message);
    }
    set(
        &store,
        &who,
        "production",
        &[(
            "db.env.SEALED",
            backend::sealed_secret(&uuid(2), &uuid(4), "SEALED", 1),
        )],
    )
    .unwrap();
    let error = set(
        &store,
        &who,
        "production",
        &[("db.env.SEALED", json!(LETTERS))],
    )
    .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
}

#[test]
fn an_own_copy_makes_its_own_value_and_a_live_node_reads_its_parents() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1);
    let production = password(&store, &who, "production");

    branch(&store, &who, 9, "copy", &["web", "db"]);
    let copy = password(&store, &who, "copy");
    assert!(letters(&copy, 32));
    assert!(copy != production, "the copy carried production's value");
    let resolved = deployed(&store, &who, "copy", 2, "DB_PASS");
    assert!(
        resolved == json!(copy),
        "web didn't read its own db's value"
    );
    // Production keeps its own.
    assert!(password(&store, &who, "production") == production);

    branch(&store, &who, 10, "live", &["web"]);
    let resolved = deployed(&store, &who, "live", 3, "DB_PASS");
    assert!(
        resolved == json!(production),
        "a live db didn't read production's value"
    );
}

#[test]
fn sync_moves_a_generator_and_never_a_value() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1);
    branch(&store, &who, 9, "fix-db", &["db"]);
    // Each holds its own value of the same generator: nothing to sync either way.
    assert!(labels(&offered(&store, &who, "fix-db", "production")).is_empty());
    assert!(labels(&offered(&store, &who, "production", "fix-db")).is_empty());

    let generator = "${{ secret(40, \"a-zA-Z\") }}";
    set(
        &store,
        &who,
        "fix-db",
        &[("db.env.PASSWORD", json!(generator))],
    )
    .unwrap();
    let view = offered(&store, &who, "fix-db", "production");
    assert_eq!(labels(&view), ["db.env.PASSWORD"]);
    assert_eq!(view.rows[0].from, json!(generator));
    store
        .write(
            &who,
            &SyncChanges {
                from: at("fix-db"),
                into: Some(at("production")),
                close_after: false,
                version: view.version.clone(),
                picks: None,
                skip: Vec::new(),
                values: std::collections::BTreeMap::new(),
                id: None,
            },
        )
        .unwrap();
    let production = password(&store, &who, "production");
    assert!(letters(&production, 40));
    assert!(
        production != password(&store, &who, "fix-db"),
        "the Sync carried the Branch's value"
    );
}

#[test]
fn follow_brings_a_new_generator_and_the_branch_makes_its_own() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1);
    branch(&store, &who, 9, "fix-db", &["db"]);
    set(
        &store,
        &who,
        "production",
        &[("db.env.TOKEN", json!(LETTERS))],
    )
    .unwrap();
    deploy(&store, &who, "production", 2);
    let token = |environment| env(&store, &who, environment, "db", "TOKEN");
    let (production, branch) = (token("production"), token("fix-db"));
    assert!(letters(branch.as_str().unwrap(), 32));
    assert!(branch != production, "Follow carried production's value");
}

#[test]
fn an_own_copy_of_a_live_node_makes_its_own_value() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1);
    branch(&store, &who, 9, "fix-web", &["web"]);
    deploy(&store, &who, "fix-web", 2);
    store
        .write(
            &who,
            &ployz_store::CopyNode {
                environment: at("fix-web"),
                node: ServiceName::parse("db").unwrap(),
                expect: None,
            },
        )
        .unwrap();
    let copy = password(&store, &who, "fix-web");
    assert!(letters(&copy, 32));
    assert!(
        copy != password(&store, &who, "production"),
        "the copy carried production's value"
    );
}

#[test]
fn undoing_a_sync_puts_back_the_value_the_receiver_held() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1);
    branch(&store, &who, 9, "fix-db", &["db"]);
    let before = password(&store, &who, "production");
    set(
        &store,
        &who,
        "fix-db",
        &[("db.env.PASSWORD", json!("${{ secret(40, \"a-zA-Z\") }}"))],
    )
    .unwrap();
    let view = offered(&store, &who, "fix-db", "production");
    let synced = store
        .write(
            &who,
            &SyncChanges {
                from: at("fix-db"),
                into: Some(at("production")),
                close_after: false,
                version: view.version.clone(),
                picks: None,
                skip: Vec::new(),
                values: std::collections::BTreeMap::new(),
                id: None,
            },
        )
        .unwrap();
    assert!(password(&store, &who, "production") != before);
    store
        .write(
            &who,
            &UndoSync {
                sync: synced.sync.clone(),
            },
        )
        .unwrap();
    assert!(
        password(&store, &who, "production") == before,
        "undo made a new value"
    );
}

fn save(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &ployz_store::Command::Publish(ployz_store::Publish {
                id: None,
                environment: at("production"),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
}

#[test]
fn sealing_a_generated_value_keeps_history_readable_and_restorable() {
    let (store, who) = shop();
    save(&store, &who);
    set(
        &store,
        &who,
        "production",
        &[(
            "db.env.PASSWORD",
            backend::sealed_secret(&uuid(2), &uuid(4), "PASSWORD", 1),
        )],
    )
    .unwrap();
    save(&store, &who);
    let history = store
        .read(
            &who,
            &ployz_store::HistoryQuery {
                environment: at("production"),
                limit: None,
            },
        )
        .unwrap();
    let first = history.revisions.last().unwrap().revision;
    let preview = store
        .read(
            &who,
            &ployz_store::HistoryPreviewQuery {
                environment: at("production"),
                revision: first,
                action: ployz_store::HistoryAction::Restore,
                change: None,
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::StageHistory {
                environment: at("production"),
                revision: first,
                action: ployz_store::HistoryAction::Restore,
                change: None,
                version: preview.version,
                accept_overwrite: true,
            },
        )
        .unwrap();
    // History never brings back plain text for a sealed key: it stays sealed.
    assert_eq!(
        env(&store, &who, "production", "db", "PASSWORD"),
        json!({ "secret": true })
    );
    save(&store, &who);
}
