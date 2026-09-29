#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! The Config Store's behaviour suite, through `read` and `write` only. It runs on
//! in-memory SQLite here and joins the Postgres suite once that adapter exists.

use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Change, Command, ConfigStore, CreateEnvironment, CreateProject, CreateService, Edit,
    EnvironmentId, EnvironmentName, EnvironmentQuery, EnvironmentRef, EnvironmentView,
    OrganizationId, ProjectId, ProjectName, Query, Revision, ServiceId, View, Written,
};
use serde_json::{Value, json};

fn actor(organization: &str) -> Actor {
    Actor {
        organization: OrganizationId::parse(organization).unwrap(),
    }
}

/// A fixed UUID per label, so replays reuse IDs.
fn uuid(label: &str) -> String {
    let hash = label.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
    });
    format!("00000000-0000-4000-8000-{:012x}", hash & 0xffff_ffff_ffff)
}

fn create_project(name: &str) -> Command {
    Command::CreateProject(CreateProject {
        id: ProjectId::parse(uuid(&format!("project-{name}"))).unwrap(),
        name: ProjectName::parse(name).unwrap(),
        default_environment: EnvironmentId::parse(uuid(&format!("env-{name}"))).unwrap(),
    })
}

fn create_service(id: &str, name: &str, image: &str) -> Command {
    Command::CreateService(CreateService {
        id: ServiceId::parse(uuid(id)).unwrap(),
        environment: EnvironmentRef::default(),
        name: ServiceName::parse(name).unwrap(),
        image: image.into(),
    })
}

fn set(path: &str, value: Value) -> Change {
    Change::Set {
        path: path.into(),
        value,
    }
}

fn edit(expect: Option<u64>, changes: Vec<Change>) -> Command {
    Command::Edit(Edit {
        environment: EnvironmentRef::default(),
        expect: expect.map(Revision),
        changes,
    })
}

fn get(store: &ConfigStore, who: &Actor, path: Option<&str>) -> EnvironmentView {
    let query = Query::Environment(EnvironmentQuery {
        environment: EnvironmentRef::default(),
        path: path.map(Into::into),
        all: false,
    });
    let View::Environment(view) = store.read(who, &query).unwrap() else {
        unreachable!("an Environment query reads an Environment")
    };
    view
}

fn value(store: &ConfigStore, who: &Actor, path: &str) -> Value {
    get(store, who, Some(path)).settings.remove(0).value
}

/// A store with Project `shop` and Service `web` running nginx.
fn shop() -> (ConfigStore, Actor) {
    let store = ConfigStore::open("sqlite::memory:").unwrap();
    let who = actor("org");
    store.write(&who, create_project("shop")).unwrap();
    store
        .write(&who, create_service("svc-web", "web", "nginx:1"))
        .unwrap();
    (store, who)
}

#[test]
fn a_new_project_opens_an_empty_default_environment() {
    let store = ConfigStore::open("sqlite::memory:").unwrap();
    let who = actor("org");
    let Written::Project(created) = store.write(&who, create_project("shop")).unwrap() else {
        panic!("expected a Project");
    };
    assert_eq!(created.environment.name.as_str(), "production");
    assert_eq!(created.environment.namespace.as_str(), "shop-production");
    let view = get(&store, &who, None);
    assert_eq!(view.environment, created.environment);
    assert!(view.settings.is_empty());
}

#[test]
fn an_image_service_shows_every_setting_with_its_default() {
    let (store, who) = shop();
    let view = get(&store, &who, Some("web"));
    assert_eq!(view.environment.revision, Revision(2));
    assert_eq!(
        serde_json::to_value(&view.settings).unwrap(),
        json!([
            { "path": "web.cpuLimit", "value": null, "default": null, "apply": "staged" },
            { "path": "web.image", "value": "nginx:1", "default": null, "apply": "staged" },
            { "path": "web.maxRetries", "value": 10, "default": 10, "apply": "staged" },
            { "path": "web.memLimit", "value": null, "default": null, "apply": "staged" },
            { "path": "web.preDeployCommand", "value": null, "default": null, "apply": "staged" },
            { "path": "web.replicas", "value": 1, "default": 1, "apply": "staged" },
            { "path": "web.restartPolicy", "value": "unless-stopped", "default": "unless-stopped", "apply": "staged" },
            { "path": "web.startCommand", "value": null, "default": null, "apply": "staged" },
        ])
    );
    assert_eq!(
        json!(view.values),
        json!({ "image": "nginx:1", "maxRetries": 10, "replicas": 1, "restartPolicy": "unless-stopped" })
    );
}

#[test]
fn the_whole_environment_shows_only_what_is_set_unless_all() {
    let (store, who) = shop();
    store
        .write(&who, edit(None, vec![set("web.cpuLimit", json!("0.5"))]))
        .unwrap();
    let view = get(&store, &who, None);
    let paths = view
        .settings
        .iter()
        .map(|row| row.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(paths, ["web.cpuLimit", "web.image"]);
    assert_eq!(view.values, None);
    let query = Query::Environment(EnvironmentQuery {
        all: true,
        ..EnvironmentQuery::default()
    });
    let View::Environment(all) = store.read(&who, &query).unwrap() else {
        unreachable!("an Environment query reads an Environment")
    };
    assert_eq!(all.settings.len(), 8);
    assert_eq!(get(&store, &who, Some("web.cpuLimit")).settings.len(), 1);
}

fn patch(value: Value) -> Change {
    Change::Patch {
        path: "web".into(),
        value,
    }
}

#[test]
fn every_setting_round_trips_get_patch_get() {
    let (store, who) = shop();
    let catalog = ployz_store::catalog::schema(Some("web")).unwrap();
    let mut values = get(&store, &who, Some("web")).values.unwrap();
    for (name, setting) in catalog["properties"].as_object().unwrap() {
        values.insert(name.clone(), setting["examples"][0].clone());
    }
    store
        .write(&who, edit(None, vec![patch(Value::Object(values.clone()))]))
        .unwrap();
    let after = get(&store, &who, Some("web")).values.unwrap();
    assert_eq!(json!(after), json!(values));
    let Written::Edited(again) = store
        .write(&who, edit(None, vec![patch(Value::Object(after))]))
        .unwrap()
    else {
        panic!("expected an edit");
    };
    assert!(again.staged.is_empty(), "sending get back changes nothing");
}

#[test]
fn a_patch_keeps_omitted_settings_and_never_clears() {
    let (store, who) = shop();
    store
        .write(&who, edit(None, vec![set("web.memLimit", json!(2))]))
        .unwrap();
    let Written::Edited(edited) = store
        .write(&who, edit(None, vec![patch(json!({ "replicas": 3 }))]))
        .unwrap()
    else {
        panic!("expected an edit");
    };
    assert_eq!(edited.staged, ["web.replicas"]);
    assert_eq!(value(&store, &who, "web.memLimit"), json!(2.0));
    for (body, code) in [
        (json!({ "memLimit": null }), RpcErrorCode::InvalidArgument),
        (json!({ "replica": 2 }), RpcErrorCode::InvalidArgument),
        (json!([1]), RpcErrorCode::InvalidArgument),
    ] {
        let error = store
            .write(&who, edit(None, vec![patch(body.clone())]))
            .unwrap_err();
        assert_eq!(error.code, code, "{body}");
    }
    let error = store
        .write(
            &who,
            edit(
                None,
                vec![Change::Patch {
                    path: "web.replicas".into(),
                    value: json!({}),
                }],
            ),
        )
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert_eq!(value(&store, &who, "web.memLimit"), json!(2.0));
}

#[test]
fn set_stages_text_values_and_unset_restores_the_default() {
    let (store, who) = shop();
    let Written::Edited(edited) = store
        .write(
            &who,
            edit(
                None,
                vec![
                    set("web.replicas", json!("3")),
                    set("web.startCommand", json!("  nginx -g 'daemon off;' ")),
                    set("web.image", json!("nginx:2")),
                ],
            ),
        )
        .unwrap()
    else {
        panic!("expected an edit");
    };
    assert_eq!(
        edited.staged,
        ["web.replicas", "web.startCommand", "web.image"]
    );
    assert!(edited.immediate.is_empty());
    assert_eq!(edited.environment.revision, Revision(3));
    assert_eq!(value(&store, &who, "web.replicas"), json!(3));
    assert_eq!(
        value(&store, &who, "web.startCommand"),
        json!("nginx -g 'daemon off;'")
    );
    assert_eq!(value(&store, &who, "web.image"), json!("nginx:2"));

    store
        .write(
            &who,
            edit(
                None,
                vec![
                    Change::Unset {
                        path: "web.replicas".into(),
                    },
                    Change::Unset {
                        path: "web.startCommand".into(),
                    },
                ],
            ),
        )
        .unwrap();
    assert_eq!(value(&store, &who, "web.replicas"), json!(1));
    assert_eq!(value(&store, &who, "web.startCommand"), Value::Null);
}

#[test]
fn an_edit_that_changes_nothing_keeps_the_revision() {
    let (store, who) = shop();
    let Written::Edited(edited) = store
        .write(&who, edit(None, vec![set("web.replicas", json!(1))]))
        .unwrap()
    else {
        panic!("expected an edit");
    };
    assert_eq!(edited.environment.revision, Revision(2));
    assert!(edited.staged.is_empty());
}

#[test]
fn an_identical_replay_returns_the_first_result_and_a_different_body_conflicts() {
    let (store, who) = shop();
    let first = get(&store, &who, None);
    let replay = store
        .write(&who, create_service("svc-web", "web", "nginx:1"))
        .unwrap();
    let Written::Service(replayed) = replay else {
        panic!("expected a Service");
    };
    assert_eq!(replayed.environment.revision, Revision(2));
    assert_eq!(get(&store, &who, None), first, "a replay writes nothing");

    for different in [
        create_service("svc-web", "web", "nginx:2"),
        create_service("svc-web", "api", "nginx:1"),
        Command::CreateEnvironment(CreateEnvironment {
            id: EnvironmentId::parse(uuid("svc-web")).unwrap(),
            project: None,
            name: EnvironmentName::parse("staging").unwrap(),
        }),
    ] {
        let error = store.write(&who, different).unwrap_err();
        assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
    }
    let error = store
        .write(&actor("other"), create_project("shop"))
        .unwrap_err();
    assert_eq!(
        error.code,
        RpcErrorCode::Conflict,
        "another Organization reuses the ID"
    );
}

#[test]
fn an_expected_revision_refuses_when_working_state_moved() {
    let (store, who) = shop();
    store
        .write(&who, edit(Some(2), vec![set("web.replicas", json!(2))]))
        .unwrap();
    let error = store
        .write(&who, edit(Some(2), vec![set("web.replicas", json!(4))]))
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert_eq!(error.details, json!({ "revision": 3 }));
    assert_eq!(value(&store, &who, "web.replicas"), json!(2));
}

#[test]
fn concurrent_blind_edits_to_different_settings_all_survive() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}", dir.path().join("store.db").display());
    let who = actor("org");
    let first = ConfigStore::open(&url).unwrap();
    first.write(&who, create_project("shop")).unwrap();
    first
        .write(&who, create_service("svc-web", "web", "nginx:1"))
        .unwrap();
    // Two handles on one file: two CLI processes editing the same Environment.
    let second = ConfigStore::open(&url).unwrap();
    std::thread::scope(|scope| {
        let replicas = scope.spawn(|| {
            for n in 2..12 {
                first
                    .write(&who, edit(None, vec![set("web.replicas", json!(n))]))
                    .unwrap();
            }
        });
        let command = scope.spawn(|| {
            for n in 0..10 {
                second
                    .write(
                        &who,
                        edit(
                            None,
                            vec![set("web.startCommand", json!(format!("run {n}")))],
                        ),
                    )
                    .unwrap();
            }
        });
        replicas.join().unwrap();
        command.join().unwrap();
    });
    assert_eq!(value(&first, &who, "web.replicas"), json!(11));
    assert_eq!(value(&first, &who, "web.startCommand"), json!("run 9"));
    assert_eq!(get(&second, &who, None).environment.revision, Revision(22));
}

#[test]
fn a_failed_edit_writes_none_of_its_changes() {
    let (store, who) = shop();
    let error = store
        .write(
            &who,
            edit(
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
    let (store, who) = shop();
    let cases = [
        (set("web.replica", json!(2)), RpcErrorCode::InvalidArgument),
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
        (
            Change::Unset {
                path: "web.image".into(),
            },
            RpcErrorCode::InvalidArgument,
        ),
    ];
    for (change, code) in cases {
        let error = store
            .write(&who, edit(None, vec![change.clone()]))
            .unwrap_err();
        assert_eq!(error.code, code, "{change:?}: {error:?}");
    }
    let error = store
        .write(&who, edit(None, vec![set("web.replica", json!(2))]))
        .unwrap_err();
    assert_eq!(error.details["did_you_mean"], "replicas");
    assert_eq!(
        error.details["valid_children"],
        json!([
            "cpuLimit",
            "image",
            "maxRetries",
            "memLimit",
            "preDeployCommand",
            "replicas",
            "restartPolicy",
            "startCommand"
        ])
    );
    let error = store
        .write(&who, edit(None, vec![set("web.cpuLimit", json!(65))]))
        .unwrap_err();
    assert_eq!(
        error.details,
        json!({
            "setting": "cpuLimit",
            "expected": { "type": "number", "exclusiveMinimum": 0, "maximum": 64 },
            "example": 0.5,
        })
    );
    let error = store
        .write(&who, edit(None, vec![set("api.replicas", json!(2))]))
        .unwrap_err();
    assert_eq!(
        error.details,
        json!({ "did_you_mean": null, "valid_children": ["web"] })
    );
    let error = store
        .write(&who, edit(None, vec![set("wbe.replicas", json!(2))]))
        .unwrap_err();
    assert_eq!(error.details["did_you_mean"], "web");
}

#[test]
fn names_resolve_within_one_organization() {
    let (store, who) = shop();
    store.write(&who, create_project("blog")).unwrap();
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

    let Command::CreateService(mut duplicate) = create_service("svc-api", "web", "nginx:1") else {
        unreachable!()
    };
    duplicate.environment.project = Some(ProjectName::parse("shop").unwrap());
    let error = store
        .write(&who, Command::CreateService(duplicate))
        .unwrap_err();
    assert_eq!(
        error.code,
        RpcErrorCode::Conflict,
        "names are unique in an Environment"
    );
}

#[test]
fn environments_are_created_in_a_named_project_and_addressed_by_name() {
    let (store, who) = shop();
    store.write(&who, create_project("blog")).unwrap();
    let Written::Environment(created) = store
        .write(
            &who,
            Command::CreateEnvironment(CreateEnvironment {
                id: EnvironmentId::parse(uuid("env-staging")).unwrap(),
                project: Some(ProjectName::parse("shop").unwrap()),
                name: EnvironmentName::parse("staging").unwrap(),
            }),
        )
        .unwrap()
    else {
        panic!("expected an Environment");
    };
    assert_eq!(created.environment.namespace.as_str(), "shop-staging");
    let query = Query::Environment(EnvironmentQuery {
        environment: EnvironmentRef {
            project: Some(ProjectName::parse("shop").unwrap()),
            environment: Some(EnvironmentName::parse("staging").unwrap()),
        },
        path: None,
        all: true,
    });
    let View::Environment(view) = store.read(&who, &query).unwrap() else {
        unreachable!("an Environment query reads an Environment")
    };
    assert!(view.settings.is_empty(), "web lives in production only");
}

#[test]
fn only_sqlite_urls_open() {
    let error = ConfigStore::open("postgres://localhost/store")
        .err()
        .unwrap();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
}
