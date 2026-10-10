#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Never sync through the Store's interface only, on SQLite and on Postgres (see
//! `backend`): a marked setting is never a Sync row in either direction, the Sync
//! view lists it apart, and a Branch of the marking Environment still gets its value.

use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::RowId;
use ployz_store::{
    Actor, BranchQuery, Change, ConfigStore, CreateBranch, CreateProject, CreateService, Edit,
    EnvironmentId, EnvironmentName, EnvironmentQuery, EnvironmentRef, NamedRow, NeverSync,
    OrganizationId, ProjectId, ProjectName, RenameService, ServiceLineageId, ServiceQuery,
    SettingPath, SyncChanges, SyncQuery, SyncView,
};
use serde_json::{Value, json};

mod backend;
use backend::deploy;

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

/// `web`'s row `at`, as `variables.KEY` or `startCommand`.
fn row(at: &str) -> RowId {
    format!("{}:{at}", uuid(3)).parse().unwrap()
}

fn never_sync(environment: &str, rows: &[&str], off: bool) -> NeverSync {
    NeverSync {
        environment: at(environment),
        rows: rows.iter().map(|at| row(at).into()).collect(),
        off,
    }
}

fn labels_of(rows: &[NamedRow]) -> Vec<String> {
    rows.iter().map(NamedRow::to_string).collect()
}

/// What `environment` marks Never sync, as the Environment view lists it.
fn marked(store: &ConfigStore, who: &Actor, environment: &str) -> Vec<RowId> {
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
                when: None,
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
                when: None,
            },
        )
        .unwrap();
    let apart = view
        .never_synced
        .iter()
        .map(|row| {
            let sides = row
                .marks
                .iter()
                .map(|mark| mark.environment.to_string())
                .collect();
            (row.at.to_string(), sides)
        })
        .collect();
    (labels(&view), apart)
}

fn labels(view: &SyncView) -> Vec<String> {
    let mut labels: Vec<String> = view.rows.iter().map(|row| row.at.to_string()).collect();
    labels.sort_unstable();
    labels
}

/// A Sync from fix-web into its Parent of `picks`, or of every ticked row.
fn sync(view: &SyncView, picks: Option<Vec<ployz_store::RowId>>) -> SyncChanges {
    let picks = picks.map(|picks| picks.into_iter().map(Into::into).collect());
    SyncChanges {
        from: at("fix-web"),
        into: None,
        when: None,
        version: view.version.clone(),
        picks,
        skip: Vec::new(),
        values: Default::default(),
        id: None,
    }
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
            &never_sync("fix-web", &["variables.APP_ENV", "startCommand"], false),
        )
        .unwrap();
    assert_eq!(marked_now.environment.name.as_str(), "fix-web");
    assert_eq!(
        labels_of(&marked_now.never_synced),
        ["web.env.APP_ENV", "web.startCommand"]
    );
    // Marking again changes nothing; the receiver marks its own.
    store
        .write(&who, &never_sync("fix-web", &["variables.APP_ENV"], false))
        .unwrap();
    store
        .write(&who, &never_sync("production", &["variables.PLAIN"], false))
        .unwrap();
    assert_eq!(marked(&store, &who, "production"), [row("variables.PLAIN")]);

    let offered = view(&store, &who);
    assert_eq!(labels(&offered), ["web.source"]);
    let apart: Vec<(String, Vec<&str>)> = offered
        .never_synced
        .iter()
        .map(|row| {
            (
                row.at.to_string(),
                row.marks
                    .iter()
                    .map(|mark| mark.environment.as_str())
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        apart,
        [
            ("web.startCommand".to_owned(), vec!["fix-web"]),
            ("web.env.APP_ENV".to_owned(), vec!["fix-web"]),
            ("web.env.PLAIN".to_owned(), vec!["production"]),
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

    // Picked, a marked setting is refused; synced by default, it stays put.
    let pick = offered.never_synced[0].at.row().clone();
    let refused = store
        .write(&who, &sync(&offered, Some(vec![pick])))
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    store.write(&who, &sync(&offered, None)).unwrap();
    let production = values(&store, &who, "production");
    assert_eq!(production["image"], json!("web:2"));
    assert_eq!(production["env"], json!({"PLAIN": "1"}));
    assert_eq!(production["startCommand"], Value::Null);

    // Synced again, it is offered again.
    store
        .write(&who, &never_sync("production", &["variables.PLAIN"], true))
        .unwrap();
    assert!(marked(&store, &who, "production").is_empty());
    assert_eq!(labels(&view(&store, &who)), ["web.env.PLAIN"]);
}

#[test]
fn a_branch_follows_its_parents_value_for_a_setting_the_parent_marks() {
    let (store, who) = shop();
    store
        .write(&who, &never_sync("production", &["variables.PLAIN"], false))
        .unwrap();
    set(&store, &who, "production", &[("web.env.PLAIN", json!("2"))]);
    deploy(&store, &who, "production", 2);
    // The mark doesn't carry into the Branch: production's value follows into it.
    assert_eq!(values(&store, &who, "fix-web")["env"]["PLAIN"], json!("2"));
    assert!(marked(&store, &who, "fix-web").is_empty());

    // Marked in the Branch, production's change never arrives there.
    store
        .write(&who, &never_sync("fix-web", &["variables.PLAIN"], false))
        .unwrap();
    set(&store, &who, "production", &[("web.env.PLAIN", json!("3"))]);
    deploy(&store, &who, "production", 3);
    assert_eq!(values(&store, &who, "fix-web")["env"]["PLAIN"], json!("2"));
    // Nor would a Sync from production carry it.
    let (rows, apart) = between(&store, &who, ("production", "fix-web"));
    assert!(rows.is_empty(), "{rows:?}");
    assert_eq!(
        apart,
        [("web.env.PLAIN".to_owned(), vec!["fix-web".to_owned()])]
    );
}

/// A mark names a row of a node the Environment has, even one it lacks so far:
/// the receiver keeps a new row out.
#[test]
fn never_sync_names_a_row_of_a_node_the_environment_has() {
    let (store, who) = shop();
    let elsewhere = NeverSync {
        rows: vec![format!("{}:startCommand", uuid(5)).as_str().into()],
        ..never_sync("fix-web", &[], false)
    };
    let missing = store.write(&who, &elsewhere).unwrap_err();
    assert_eq!(missing.code, RpcErrorCode::InvalidArgument);
    assert!(missing.message.contains("has nothing at"), "{missing:?}");
    assert!(marked(&store, &who, "fix-web").is_empty());

    store
        .write(&who, &never_sync("production", &["variables.NEW"], false))
        .unwrap();
    set(&store, &who, "fix-web", &[("web.env.NEW", json!("1"))]);
    let offered = view(&store, &who);
    assert!(offered.rows.is_empty());
    assert_eq!(offered.never_synced[0].at.to_string(), "web.env.NEW");
}

/// Names resolve in the marking Environment's own configuration: a root marks by
/// name, a Branch marks a setting it never changed or one at its default, and a
/// prefix marks every row under it.
#[test]
fn rows_are_marked_by_name_in_the_marking_environments_own_configuration() {
    let (store, who) = shop();
    let by_name = |environment: &str, rows: &[&str], off| NeverSync {
        rows: rows.iter().map(|row| (*row).into()).collect(),
        ..never_sync(environment, &[], off)
    };
    set(&store, &who, "production", &[("web.env.OTHER", json!("2"))]);
    let marked = store
        .write(&who, &by_name("production", &["web.env"], false))
        .unwrap();
    assert_eq!(
        labels_of(&marked.never_synced),
        ["web.env.OTHER", "web.env.PLAIN"]
    );
    let marked = store
        .write(&who, &by_name("fix-web", &["web.env.PLAIN"], false))
        .unwrap();
    assert_eq!(labels_of(&marked.never_synced), ["web.env.PLAIN"]);
    let unmarked = store
        .write(&who, &by_name("production", &["web.env.OTHER"], true))
        .unwrap();
    assert_eq!(labels_of(&unmarked.never_synced), ["web.env.PLAIN"]);
    // A setting at its default is marked by name as by RowId.
    let marked = store
        .write(&who, &by_name("fix-web", &["web.startCommand"], false))
        .unwrap();
    assert_eq!(
        labels_of(&marked.never_synced),
        ["web.env.PLAIN", "web.startCommand"]
    );
    let unknown = store
        .write(&who, &by_name("fix-web", &["web.env.NOPE"], false))
        .unwrap_err();
    assert_eq!(unknown.code, RpcErrorCode::NotFound);
}

#[test]
fn marks_hold_between_any_two_environments_but_a_parents_own_toward_its_direct_branch() {
    // production → fix-web → deep, and production → qa.
    let (store, who) = shop();
    branch(&store, &who, 10, "fix-web", "deep");
    branch(&store, &who, 11, "production", "qa");
    store
        .write(&who, &never_sync("production", &["variables.PLAIN"], false))
        .unwrap();
    set(&store, &who, "production", &[("web.env.PLAIN", json!("2"))]);
    set(
        &store,
        &who,
        "fix-web",
        &[("web.startCommand", json!("serve --debug"))],
    );
    store
        .write(&who, &never_sync("fix-web", &["startCommand"], false))
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

/// A mark on what a new Service can't arrive without keeps the Service out, and the
/// Sync view names that mark so it can be undone.
#[test]
fn a_new_services_row_kept_out_by_a_mark_names_the_mark_to_undo() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(4)).unwrap(),
                environment: at("fix-web"),
                name: ServiceName::parse("api").unwrap(),
                image: Some("api:1".into()),
                template: None,
            },
        )
        .unwrap();
    // The source is one row: its image is no row of its own.
    let mark = |row: &str| {
        store.write(
            &who,
            &NeverSync {
                environment: at("fix-web"),
                rows: vec![row.into()],
                off: false,
            },
        )
    };
    let none = mark("api.image").unwrap_err();
    assert_eq!(none.message, "No row named api.image here");
    mark("api.source").unwrap();
    let apart = view(&store, &who).never_synced;
    assert_eq!(apart.len(), 1);
    assert_eq!(apart[0].at.to_string(), "api");
    let [mark] = apart[0].marks.as_slice() else {
        panic!("one mark: {:?}", apart[0].marks);
    };
    assert_eq!(mark.environment.as_str(), "fix-web");
    assert_ne!(mark.row, *apart[0].at.row());

    store
        .write(
            &who,
            &NeverSync {
                environment: at(mark.environment.as_str()),
                rows: vec![mark.row.clone().into()],
                off: true,
            },
        )
        .unwrap();
    let offered = view(&store, &who);
    assert!(offered.never_synced.is_empty());
    assert!(labels(&offered).contains(&"api".to_owned()));
}

/// production renames `web` to `frontend`: one row, however each side names it, is
/// marked from either side and unmarked the same way.
#[test]
fn a_row_is_marked_from_either_side_whatever_each_names_it() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &RenameService {
                environment: at("production"),
                service: ServiceName::parse("web").unwrap(),
                name: ServiceName::parse("frontend").unwrap(),
            },
        )
        .unwrap();
    set(&store, &who, "fix-web", &[("web.env.PLAIN", json!("2"))]);
    assert_eq!(*view(&store, &who).rows[0].at.row(), row("variables.PLAIN"));
    for (environment, label) in [
        ("fix-web", "web.env.PLAIN"),
        ("production", "frontend.env.PLAIN"),
    ] {
        let marked = store
            .write(&who, &never_sync(environment, &["variables.PLAIN"], false))
            .unwrap();
        assert_eq!(labels_of(&marked.never_synced), [label]);
    }
    let apart = &view(&store, &who).never_synced;
    assert_eq!(apart.len(), 1);
    assert_eq!(apart[0].marks.len(), 2);
    for environment in ["fix-web", "production"] {
        store
            .write(&who, &never_sync(environment, &["variables.PLAIN"], true))
            .unwrap();
    }
    assert!(view(&store, &who).never_synced.is_empty());
}

#[test]
fn a_branchs_copy_of_a_service_is_named_by_its_lineages_row() {
    let (store, who) = shop();
    // fix-web's web is a copy: a new id, the same lineage, so the same row.
    let services = store
        .read(
            &who,
            &ployz_store::ServicesQuery {
                environment: at("fix-web"),
            },
        )
        .unwrap()
        .services;
    let web = &services[0];
    assert_ne!(web.service.id.as_str(), uuid(3));
    assert_eq!(web.row, RowId::node(&uuid(3)));
    store
        .write(
            &who,
            &NeverSync {
                environment: at("fix-web"),
                rows: vec![web.row.clone().into()],
                off: false,
            },
        )
        .unwrap();
    assert_eq!(
        marked(&store, &who, "fix-web"),
        std::slice::from_ref(&web.row)
    );
}

/// Include fix-web's ticked rows into production; the proposal it made.
fn include(store: &ConfigStore, who: &Actor) -> ployz_store::ProposalId {
    let mut changes = sync(&view(store, who), None);
    changes.id = Some(ployz_store::SyncId::parse("00000000-0000-4000-8000-000000000201").unwrap());
    let ployz_store::SyncedWhen::Now { proposal, .. } = store.write(who, &changes).unwrap().when
    else {
        panic!("a Sync into a Branch's Parent stages now")
    };
    proposal
}

#[test]
fn marking_an_owned_row_never_sync_blocks_its_refresh_and_remove_still_inverts_it() {
    let (store, who) = shop();
    set(&store, &who, "fix-web", &[("web.env.PLAIN", json!("2"))]);
    let proposal = include(&store, &who);
    assert_eq!(
        values(&store, &who, "production")["env"]["PLAIN"],
        json!("2")
    );
    store
        .write(&who, &never_sync("production", &["variables.PLAIN"], false))
        .unwrap();
    set(&store, &who, "fix-web", &[("web.env.PLAIN", json!("3"))]);
    let (offered, apart) = between(&store, &who, ("fix-web", "production"));
    assert!(offered.is_empty(), "{offered:?}");
    assert_eq!(
        apart,
        [("web.env.PLAIN".to_owned(), vec!["production".to_owned()])]
    );
    // The mark keeps the row out of later Syncs; Remove still puts back what was.
    let removed = store
        .write(
            &who,
            &ployz_store::RemoveProposal {
                environment: at("production"),
                proposal,
                version: None,
            },
        )
        .unwrap();
    assert!(removed.removed);
    assert_eq!(
        values(&store, &who, "production")["env"]["PLAIN"],
        json!("1")
    );
}
