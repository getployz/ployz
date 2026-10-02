#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Never sync through the Store's interface only, on SQLite and on Postgres (see
//! `backend`): a marked setting is never a Sync row in either direction, the Sync
//! view lists it apart, and a Branch of the marking Environment still gets its value.

use ployz_core::{DeployOutcome, DeployPreview, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Admit, BranchQuery, Change, ConfigStore, CreateBranch, CreateProject, CreateService,
    Deploy, DeploymentId, Edit, EnvironmentId, EnvironmentName, EnvironmentQuery, EnvironmentRef,
    MoveQuery, NeverSync, OrganizationId, ProjectId, ProjectName, RunEvidence, RunnerId,
    ServiceLineageId, ServiceQuery, SettingPath, SyncChanges, SyncQuery, SyncView, Trusted,
};
use serde_json::{Value, json};

mod backend;

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn at(environment: &str) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    }
}

/// production runs `web` (`PLAIN=1`); Branch `fix-web` has its own `web`.
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
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(3)).unwrap(),
                environment: at("production"),
                name: ServiceName::parse("web").unwrap(),
                image: Some("web:1".into()),
                template: None,
            },
        )
        .unwrap();
    set(&store, &who, "production", &[("web.env.PLAIN", json!("1"))]);
    deploy(&store, &who, "production", 1);
    branch(&store, &who, 9, "production", "fix-web");
    (store, who)
}

/// Branch `name` off `from`, copying `web`.
fn branch(store: &ConfigStore, who: &Actor, n: u8, from: &str, name: &str) {
    store
        .write(
            who,
            &CreateBranch {
                id: EnvironmentId::parse(uuid(n)).unwrap(),
                from: at(from),
                name: EnvironmentName::parse(name).unwrap(),
                copy: vec![ployz_store::NodeName::parse("web").unwrap()],
                live: Vec::new(),
                setup: Vec::new(),
                keep: false,
                fix: None,
            },
        )
        .unwrap();
}

fn set(store: &ConfigStore, who: &Actor, environment: &str, changes: &[(&str, Value)]) {
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
        .unwrap();
}

fn never_sync(environment: &str, paths: &[&str], off: bool) -> NeverSync {
    NeverSync {
        environment: at(environment),
        paths: paths
            .iter()
            .map(|path| SettingPath::parse(path).unwrap())
            .collect(),
        off,
    }
}

/// What `environment` marks Never sync, as the Environment view lists it.
fn marked(store: &ConfigStore, who: &Actor, environment: &str) -> Vec<String> {
    store
        .read(
            who,
            &EnvironmentQuery {
                environment: at(environment),
                path: None,
                all: false,
            },
        )
        .unwrap()
        .never_synced
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn values(store: &ConfigStore, who: &Actor, environment: &str) -> Value {
    Value::Object(
        store
            .read(
                who,
                &ServiceQuery {
                    environment: at(environment),
                    service: ServiceName::parse("web").unwrap(),
                },
            )
            .unwrap()
            .values,
    )
}

fn view(store: &ConfigStore, who: &Actor) -> SyncView {
    store
        .read(
            who,
            &SyncQuery {
                from: at("fix-web"),
                into: None,
            },
        )
        .unwrap()
}

/// What a Sync from `from` into `into` offers, and what it lists apart as marked in which sides.
fn between(
    store: &ConfigStore,
    who: &Actor,
    (from, into): (&str, &str),
) -> (Vec<String>, Vec<(String, Vec<String>)>) {
    let view = store
        .read(
            who,
            &SyncQuery {
                from: at(from),
                into: Some(at(into)),
            },
        )
        .unwrap();
    let apart = view
        .never_synced
        .iter()
        .map(|row| {
            let sides = row.marked_in.iter().map(ToString::to_string).collect();
            (row.label.clone(), sides)
        })
        .collect();
    (
        labels(&view).into_iter().map(str::to_owned).collect(),
        apart,
    )
}

fn labels(view: &SyncView) -> Vec<&str> {
    let mut labels: Vec<&str> = view.rows.iter().map(|row| row.label.as_str()).collect();
    labels.sort_unstable();
    labels
}

/// The labels of what the Parent's deployed changes would stage in `fix-web`.
fn following(store: &ConfigStore, who: &Actor) -> Vec<String> {
    store
        .read(
            who,
            &MoveQuery::Update {
                into: at("fix-web"),
            },
        )
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row.row)
        .collect()
}

/// Deploy `environment` in full and record every Service applied.
fn deploy(store: &ConfigStore, who: &Actor, environment: &str, n: u8) {
    let id = DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap();
    store
        .write_trusted(
            who,
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: at(environment),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &Trusted::default(),
        )
        .unwrap();
    let runner = RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&id, &runner).unwrap();
    let names: Vec<String> = claimed
        .intent
        .target
        .iter()
        .map(|service| service.name.to_string())
        .collect();
    let operation = |index: usize| {
        json!({"type": "remove_container", "machine_id": "a".repeat(32),
               "container_id": format!("{index:x}").repeat(64)})
    };
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": claimed.intent.namespace,
        "operations": names.iter().enumerate().map(|(index, name)| json!({
            "index": index, "machine_id": "a".repeat(32), "service_name": name,
            "operation": operation(index), "status": {"type": "pending"}
        })).collect::<Vec<_>>(),
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(&id, &runner, RunEvidence::Prepared(preview))
        .unwrap();
    let outcome: DeployOutcome<ployz_core::ExecutionError> = serde_json::from_value(json!({
        "type": "success",
        "completed": (0..names.len()).map(operation).collect::<Vec<_>>()
    }))
    .unwrap();
    store
        .record(
            &id,
            &runner,
            RunEvidence::Executed {
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
}

#[test]
fn a_setting_either_side_marks_never_sync_is_never_a_row_and_is_listed_apart() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "fix-web",
        &[
            ("web.image", json!("web:2")),
            ("web.startCommand", json!("serve --debug")),
            ("web.env.PLAIN", json!("2")),
            ("web.env.APP_ENV", json!("fix")),
        ],
    );
    let marked_now = store
        .write(
            &who,
            &never_sync("fix-web", &["web.env.APP_ENV", "web.startCommand"], false),
        )
        .unwrap();
    assert_eq!(marked_now.environment.name.as_str(), "fix-web");
    let listed: Vec<String> = marked_now
        .never_synced
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(listed, ["web.env.APP_ENV", "web.startCommand"]);
    // Marking again changes nothing; the receiver marks its own.
    store
        .write(&who, &never_sync("fix-web", &["web.env.APP_ENV"], false))
        .unwrap();
    store
        .write(&who, &never_sync("production", &["web.env.PLAIN"], false))
        .unwrap();
    assert_eq!(marked(&store, &who, "production"), ["web.env.PLAIN"]);

    let offered = view(&store, &who);
    assert_eq!(labels(&offered), ["web.image"]);
    let apart: Vec<(&str, Vec<&str>)> = offered
        .never_synced
        .iter()
        .map(|row| {
            (
                row.label.as_str(),
                row.marked_in.iter().map(EnvironmentName::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        apart,
        [
            ("web.env.APP_ENV", vec!["fix-web"]),
            ("web.env.PLAIN", vec!["production"]),
            ("web.startCommand", vec!["fix-web"]),
        ]
    );
    let to_parent = store
        .read(
            &who,
            &BranchQuery {
                environment: at("fix-web"),
            },
        )
        .unwrap()
        .to_parent;
    assert_eq!(to_parent, 1);

    // Picked by key, a marked setting is refused; synced by default, it stays put.
    let refused = store
        .write(
            &who,
            &SyncChanges {
                from: at("fix-web"),
                picks: Some(vec![offered.never_synced[0].key.clone()]),
                ..SyncChanges::default()
            },
        )
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::NotFound);
    store
        .write(
            &who,
            &SyncChanges {
                from: at("fix-web"),
                ..SyncChanges::default()
            },
        )
        .unwrap();
    let production = values(&store, &who, "production");
    assert_eq!(production["image"], json!("web:2"));
    assert_eq!(production["env"], json!({"PLAIN": "1"}));
    assert_eq!(production["startCommand"], Value::Null);

    // Synced again, it is offered again.
    store
        .write(&who, &never_sync("production", &["web.env.PLAIN"], true))
        .unwrap();
    assert!(marked(&store, &who, "production").is_empty());
    assert_eq!(labels(&view(&store, &who)), ["web.env.PLAIN"]);
}

#[test]
fn a_branch_follows_its_parents_value_for_a_setting_the_parent_marks() {
    let (store, who) = shop();
    store
        .write(&who, &never_sync("production", &["web.env.PLAIN"], false))
        .unwrap();
    set(&store, &who, "production", &[("web.env.PLAIN", json!("2"))]);
    deploy(&store, &who, "production", 2);
    // The mark doesn't carry into the Branch: production's value follows into it.
    assert_eq!(values(&store, &who, "fix-web")["env"]["PLAIN"], json!("2"));
    assert!(marked(&store, &who, "fix-web").is_empty());

    // Marked in the Branch, production's change never arrives there.
    store
        .write(&who, &never_sync("fix-web", &["web.env.PLAIN"], false))
        .unwrap();
    set(&store, &who, "production", &[("web.env.PLAIN", json!("3"))]);
    deploy(&store, &who, "production", 3);
    assert_eq!(values(&store, &who, "fix-web")["env"]["PLAIN"], json!("2"));
    assert!(following(&store, &who).is_empty());
}

#[test]
fn never_sync_names_a_setting_the_environment_has() {
    let (store, who) = shop();
    let whole = store
        .write(&who, &never_sync("fix-web", &["web"], false))
        .unwrap_err();
    assert_eq!(whole.code, RpcErrorCode::InvalidArgument);
    let missing = store
        .write(&who, &never_sync("fix-web", &["api.env.KEY"], false))
        .unwrap_err();
    assert_eq!(missing.code, RpcErrorCode::NotFound);
    assert!(marked(&store, &who, "fix-web").is_empty());
}

#[test]
fn marks_hold_between_any_two_environments_but_a_parents_own_toward_its_direct_branch() {
    // production → fix-web → deep, and production → qa.
    let (store, who) = shop();
    branch(&store, &who, 10, "fix-web", "deep");
    branch(&store, &who, 11, "production", "qa");
    store
        .write(&who, &never_sync("production", &["web.env.PLAIN"], false))
        .unwrap();
    set(&store, &who, "production", &[("web.env.PLAIN", json!("2"))]);
    set(
        &store,
        &who,
        "fix-web",
        &[("web.startCommand", json!("serve --debug"))],
    );
    store
        .write(&who, &never_sync("fix-web", &["web.startCommand"], false))
        .unwrap();
    set(&store, &who, "deep", &[("web.env.PLAIN", json!("3"))]);
    let marked_in = |label: &str, side: &str| vec![(label.to_owned(), vec![side.to_owned()])];

    // Into its direct Branch, the Parent's own mark is waived.
    let (rows, apart) = between(&store, &who, ("production", "fix-web"));
    assert_eq!(
        (rows, apart),
        (vec!["web.env.PLAIN".to_owned()], Vec::new())
    );
    // Root into a Branch it didn't make.
    let (rows, apart) = between(&store, &who, ("production", "deep"));
    assert!(!rows.contains(&"web.env.PLAIN".to_owned()));
    assert_eq!(apart, marked_in("web.env.PLAIN", "production"));
    // Sideways.
    let (rows, apart) = between(&store, &who, ("fix-web", "qa"));
    assert!(!rows.contains(&"web.startCommand".to_owned()));
    assert_eq!(apart, marked_in("web.startCommand", "fix-web"));
    // Skipping a level, into a receiver that marked it.
    let (rows, apart) = between(&store, &who, ("deep", "production"));
    assert!(!rows.contains(&"web.env.PLAIN".to_owned()));
    assert_eq!(apart, marked_in("web.env.PLAIN", "production"));
}
