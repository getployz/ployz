//! The Config Store's behaviour suite, through its public interface only. It runs on
//! SQLite, and on Postgres when `PLOYZ_STORE_TEST_POSTGRES` names a server as
//! `postgres://USER:PASSWORD@HOST:PORT` (each test gets its own new database).

use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Change, Command, ConfigStore, CreateEnvironment, CreateProject, CreateService, Edit,
    EnvironmentId, EnvironmentName, EnvironmentQuery, EnvironmentRef, EnvironmentView,
    OrganizationId, ProjectId, ProjectName, Query, Revision, ServiceId, SettingPath, View, Written,
};
use serde_json::{Value, json};

/// A new, empty Store database, shared by every handle opened on the URL.
fn fresh_url(dir: &tempfile::TempDir) -> String {
    let Ok(server) = std::env::var("PLOYZ_STORE_TEST_POSTGRES") else {
        return format!("sqlite:{}", dir.path().join("store.db").display());
    };
    let name = format!("store_{}", uuid::Uuid::new_v4().simple());
    postgres::Client::connect(&server, postgres::NoTls)
        .unwrap()
        .batch_execute(&format!("CREATE DATABASE {name}"))
        .unwrap();
    format!("{server}/{name}")
}

/// A new, empty Store.
fn open() -> ConfigStore {
    if std::env::var_os("PLOYZ_STORE_TEST_POSTGRES").is_none() {
        return ConfigStore::open("sqlite::memory:").unwrap();
    }
    ConfigStore::open(&fresh_url(&tempfile::tempdir().unwrap())).unwrap()
}

fn actor(organization: &str) -> Actor {
    Actor {
        organization: OrganizationId::parse(organization).unwrap(),
    }
}

/// A fixed UUID per label, so replays reuse IDs.
fn uuid(label: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, label.as_bytes()).to_string()
}

fn create_project(name: &str) -> CreateProject {
    CreateProject {
        id: ProjectId::parse(uuid(&format!("project-{name}"))).unwrap(),
        name: ProjectName::parse(name).unwrap(),
        default_environment: EnvironmentId::parse(uuid(&format!("env-{name}"))).unwrap(),
    }
}

fn create_environment(id: &str, project: Option<&str>, name: &str) -> CreateEnvironment {
    CreateEnvironment {
        id: EnvironmentId::parse(uuid(id)).unwrap(),
        project: project.map(|project| ProjectName::parse(project).unwrap()),
        name: EnvironmentName::parse(name).unwrap(),
    }
}

fn create_service(id: &str, name: &str, image: &str) -> CreateService {
    CreateService {
        id: ServiceId::parse(uuid(id)).unwrap(),
        environment: EnvironmentRef::default(),
        name: ServiceName::parse(name).unwrap(),
        image: image.into(),
    }
}

fn path(path: &str) -> SettingPath {
    SettingPath::parse(path).unwrap()
}

fn set(at: &str, value: Value) -> Change {
    Change::Set {
        path: path(at),
        value,
    }
}

fn edit(expect: Option<u64>, changes: Vec<Change>) -> Edit {
    Edit {
        environment: EnvironmentRef::default(),
        expect: expect.map(Revision),
        changes,
    }
}

fn get(store: &ConfigStore, who: &Actor, at: Option<&str>) -> EnvironmentView {
    let query = EnvironmentQuery {
        environment: EnvironmentRef::default(),
        path: at.map(path),
    };
    store.environment(who, &query).unwrap()
}

fn value(store: &ConfigStore, who: &Actor, path: &str) -> Value {
    get(store, who, Some(path)).settings.remove(0).value
}

fn paths(paths: &[SettingPath]) -> Vec<String> {
    paths.iter().map(ToString::to_string).collect()
}

/// A store with Project `shop` and Service `web` running nginx.
fn shop() -> (ConfigStore, Actor) {
    let store = open();
    let who = actor("org");
    store.create_project(&who, &create_project("shop")).unwrap();
    store
        .create_service(&who, &create_service("svc-web", "web", "nginx:1"))
        .unwrap();
    (store, who)
}

#[test]
fn a_new_project_opens_an_empty_default_environment() {
    let store = open();
    let who = actor("org");
    let created = store.create_project(&who, &create_project("shop")).unwrap();
    assert_eq!(created.environment.name.as_str(), "production");
    let view = get(&store, &who, None);
    assert_eq!(view.environment, created.environment);
    assert!(view.settings.is_empty());
}

#[test]
fn an_image_service_shows_every_setting_with_its_default() {
    let store = open();
    let who = actor("org");
    store.create_project(&who, &create_project("shop")).unwrap();
    let created = store
        .create_service(&who, &create_service("svc-web", "web", "nginx:1"))
        .unwrap();
    assert_eq!(
        paths(&created.staged),
        ["web.command", "web.image", "web.replicas"]
    );
    let view = get(&store, &who, Some("web"));
    assert_eq!(view.environment.revision, Revision(2));
    assert_eq!(
        serde_json::to_value(&view.settings).unwrap(),
        json!([
            { "path": "web.command", "value": null, "default": null, "apply": "staged" },
            { "path": "web.image", "value": "nginx:1", "default": null, "apply": "staged" },
            { "path": "web.replicas", "value": 1, "default": 1, "apply": "staged" },
        ])
    );
}

#[test]
fn set_stages_text_values_and_unset_restores_the_default() {
    let (store, who) = shop();
    let edited = store
        .edit(
            &who,
            &edit(
                None,
                vec![
                    set("web.replicas", json!("3")),
                    set("web.command", json!("  nginx -g 'daemon off;' ")),
                    set("web.image", json!("nginx:2")),
                ],
            ),
        )
        .unwrap();
    assert_eq!(
        paths(&edited.staged),
        ["web.replicas", "web.command", "web.image"]
    );
    assert!(edited.immediate.is_empty());
    assert_eq!(edited.environment.revision, Revision(3));
    assert_eq!(value(&store, &who, "web.replicas"), json!(3));
    assert_eq!(
        value(&store, &who, "web.command"),
        json!("nginx -g 'daemon off;'")
    );
    assert_eq!(value(&store, &who, "web.image"), json!("nginx:2"));

    store
        .edit(
            &who,
            &edit(
                None,
                vec![
                    Change::Unset {
                        path: path("web.replicas"),
                    },
                    Change::Unset {
                        path: path("web.command"),
                    },
                ],
            ),
        )
        .unwrap();
    assert_eq!(value(&store, &who, "web.replicas"), json!(1));
    assert_eq!(value(&store, &who, "web.command"), Value::Null);
}

#[test]
fn an_edit_that_changes_nothing_keeps_the_revision() {
    let (store, who) = shop();
    let edited = store
        .edit(&who, &edit(None, vec![set("web.replicas", json!(1))]))
        .unwrap();
    assert_eq!(edited.environment.revision, Revision(2));
    assert_eq!(paths(&edited.staged), ["web.replicas"]);
}

#[test]
fn an_identical_replay_returns_the_first_result_and_a_different_body_conflicts() {
    let (store, who) = shop();
    let first = get(&store, &who, None);
    let replay = store
        .write(
            &who,
            &Command::CreateService(create_service("svc-web", "web", "nginx:1")),
        )
        .unwrap();
    let Written::Service(replayed) = replay else {
        panic!("expected a Service");
    };
    assert_eq!(replayed.environment.revision, Revision(2));
    assert_eq!(get(&store, &who, None), first, "a replay writes nothing");

    for different in [
        Command::CreateService(create_service("svc-web", "web", "nginx:2")),
        Command::CreateService(create_service("svc-web", "api", "nginx:1")),
        Command::CreateEnvironment(create_environment("svc-web", None, "staging")),
        // The Default Environment's ID is the create's too.
        Command::CreateEnvironment(create_environment("env-shop", None, "staging")),
        Command::CreateProject(CreateProject {
            default_environment: EnvironmentId::parse(uuid("svc-web")).unwrap(),
            ..create_project("blog")
        }),
    ] {
        let error = store.write(&who, &different).unwrap_err();
        assert_eq!(
            error.code,
            RpcErrorCode::Conflict,
            "{different:?}: {error:?}"
        );
    }
    let error = store
        .create_project(&actor("other"), &create_project("shop"))
        .unwrap_err();
    assert_eq!(
        error.code,
        RpcErrorCode::Conflict,
        "another Organization reuses the ID"
    );
}

#[test]
fn a_misspelled_field_is_refused_rather_than_ignored() {
    let command = |guard: &str| {
        json!({
            "command": "edit",
            guard: 2,
            "changes": [{ "op": "set", "path": "web.replicas", "value": 2 }],
        })
    };
    assert!(serde_json::from_value::<Command>(command("expect")).is_ok());
    assert!(serde_json::from_value::<Command>(command("expected")).is_err());
    let query = json!({ "query": "environment", "environment": { "env": "staging" } });
    assert!(serde_json::from_value::<Query>(query).is_err());
}

#[test]
fn an_expected_revision_refuses_when_working_state_moved() {
    let (store, who) = shop();
    store
        .edit(&who, &edit(Some(2), vec![set("web.replicas", json!(2))]))
        .unwrap();
    let error = store
        .edit(&who, &edit(Some(2), vec![set("web.replicas", json!(4))]))
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert_eq!(error.details, json!({ "revision": 3 }));
    assert_eq!(value(&store, &who, "web.replicas"), json!(2));
}

#[test]
fn concurrent_blind_edits_to_different_settings_all_survive() {
    let dir = tempfile::tempdir().unwrap();
    let url = fresh_url(&dir);
    let who = actor("org");
    let first = ConfigStore::open(&url).unwrap();
    first.create_project(&who, &create_project("shop")).unwrap();
    first
        .create_service(&who, &create_service("svc-web", "web", "nginx:1"))
        .unwrap();
    // Two handles on one file: two CLI processes editing the same Environment.
    let second = ConfigStore::open(&url).unwrap();
    std::thread::scope(|scope| {
        let replicas = scope.spawn(|| {
            for n in 2..12 {
                first
                    .edit(&who, &edit(None, vec![set("web.replicas", json!(n))]))
                    .unwrap();
            }
        });
        let command = scope.spawn(|| {
            for n in 0..10 {
                second
                    .edit(
                        &who,
                        &edit(None, vec![set("web.command", json!(format!("run {n}")))]),
                    )
                    .unwrap();
            }
        });
        replicas.join().unwrap();
        command.join().unwrap();
    });
    assert_eq!(value(&first, &who, "web.replicas"), json!(11));
    assert_eq!(value(&first, &who, "web.command"), json!("run 9"));
    assert_eq!(get(&second, &who, None).environment.revision, Revision(22));
}

#[test]
fn a_failed_edit_writes_none_of_its_changes() {
    let (store, who) = shop();
    let error = store
        .edit(
            &who,
            &edit(
                None,
                vec![
                    set("web.replicas", json!(5)),
                    set("web.replicas", json!(99)),
                ],
            ),
        )
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert!(
        !error.message.contains("99"),
        "an invalid value is never echoed: {error:?}"
    );
    assert_eq!(value(&store, &who, "web.replicas"), json!(1));
}

#[test]
fn wrong_paths_and_values_name_the_fix() {
    let error = SettingPath::parse("web.replica").unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        error.details,
        json!({ "settings": ["command", "image", "replicas"] })
    );
    let error = SettingPath::parse("Web!.replicas").unwrap_err();
    assert!(!error.message.contains("Web!"), "{error:?}");

    let (store, who) = shop();
    let cases = [
        (set("api.replicas", json!(2)), RpcErrorCode::NotFound),
        (set("web", json!(2)), RpcErrorCode::InvalidArgument),
        (
            set("web.replicas", Value::Null),
            RpcErrorCode::InvalidArgument,
        ),
        (
            set("web.replicas", json!("many")),
            RpcErrorCode::InvalidArgument,
        ),
        (set("web.image", json!(7)), RpcErrorCode::InvalidArgument),
        (
            Change::Unset {
                path: path("web.image"),
            },
            RpcErrorCode::InvalidArgument,
        ),
    ];
    for (change, code) in cases {
        let error = store
            .edit(&who, &edit(None, vec![change.clone()]))
            .unwrap_err();
        assert_eq!(error.code, code, "{change:?}: {error:?}");
    }
    let error = store
        .edit(&who, &edit(None, vec![set("api.replicas", json!(2))]))
        .unwrap_err();
    assert_eq!(error.details, json!({ "services": ["web"] }));
}

#[test]
fn malformed_ids_and_names_are_refused_without_echo() {
    type Parse = fn(&str) -> Result<(), ployz_core::RpcError>;
    let cases: [(Parse, &str); 7] = [
        (|value| OrganizationId::parse(value).map(drop), "bad org!"),
        (|value| OrganizationId::parse(value).map(drop), ""),
        (|value| ProjectId::parse(value).map(drop), "not-a-uuid"),
        (|value| EnvironmentId::parse(value).map(drop), "staging"),
        (|value| ServiceId::parse(value).map(drop), "web"),
        (|value| ProjectName::parse(value).map(drop), "Shop_1"),
        (|value| EnvironmentName::parse(value).map(drop), "-prod"),
    ];
    for (parse, value) in cases {
        let error = parse(value).unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{value:?}");
        assert!(
            value.is_empty() || !error.message.contains(value),
            "{value:?} echoed: {error:?}"
        );
    }
}

#[test]
fn names_resolve_within_one_organization() {
    let (store, who) = shop();
    store.create_project(&who, &create_project("blog")).unwrap();
    let query = Query::Environment(EnvironmentQuery::default());
    let error = store.read(&who, &query).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Ambiguous);
    assert_eq!(error.details, json!({ "projects": ["blog", "shop"] }));

    let error = store.read(&actor("other"), &query).unwrap_err();
    assert_eq!(
        error.code,
        RpcErrorCode::NotFound,
        "another Organization sees nothing"
    );
    assert_eq!(error.details, json!({ "next": "ployz project new NAME" }));

    let mut duplicate = create_service("svc-api", "web", "nginx:1");
    duplicate.environment.project = Some(ProjectName::parse("shop").unwrap());
    let error = store.create_service(&who, &duplicate).unwrap_err();
    assert_eq!(
        error.code,
        RpcErrorCode::Conflict,
        "names are unique in an Environment"
    );
}

#[test]
fn environments_are_created_in_a_named_project_and_addressed_by_name() {
    let (store, who) = shop();
    store.create_project(&who, &create_project("blog")).unwrap();
    store
        .create_environment(
            &who,
            &create_environment("env-staging", Some("shop"), "staging"),
        )
        .unwrap();
    let mut query = EnvironmentQuery {
        environment: EnvironmentRef {
            project: Some(ProjectName::parse("shop").unwrap()),
            environment: Some(EnvironmentName::parse("staging").unwrap()),
        },
        path: None,
    };
    let view = store.environment(&who, &query).unwrap();
    assert!(view.settings.is_empty(), "web lives in production only");

    query.environment.environment = Some(EnvironmentName::parse("preview").unwrap());
    let error = store.environment(&who, &query).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::NotFound);
    assert_eq!(
        error.details,
        json!({ "next": "ployz env new preview --project shop" })
    );
}

#[test]
fn project_and_environment_names_never_clash_across_projects() {
    let store = open();
    let who = actor("org");
    store.create_project(&who, &create_project("a-b")).unwrap();
    store.create_project(&who, &create_project("a")).unwrap();
    store
        .create_environment(&who, &create_environment("env-c", Some("a-b"), "c"))
        .unwrap();
    store
        .create_environment(&who, &create_environment("env-b-c", Some("a"), "b-c"))
        .unwrap();
}

#[test]
fn the_wire_and_typed_forms_agree() {
    let store = open();
    let who = actor("org");
    let Written::Project(created) = store
        .write(&who, &Command::CreateProject(create_project("shop")))
        .unwrap()
    else {
        panic!("expected a Project");
    };
    let View::Environment(view) = store
        .read(&who, &Query::Environment(EnvironmentQuery::default()))
        .unwrap();
    assert_eq!(view.environment, created.environment);
    assert_eq!(
        store.create_project(&who, &create_project("shop")).unwrap(),
        created,
        "a typed replay of a wire create"
    );
}

#[test]
fn only_postgres_and_sqlite_urls_open() {
    let error = ConfigStore::open("mysql://localhost/store").err().unwrap();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
}
