#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Follow through the Store's interface only, on SQLite and on Postgres (see
//! `backend`): what a Parent deploys arrives staged in each of its Branches, once,
//! tagged with where it came from; a Branch's own change wins and the Parent's
//! value is a Use hint, as is one the Branch discards; it cascades one level per
//! deploy; and a secret follows only into a Branch that never set its own.

use ployz_core::config::{At, Setting};
use ployz_core::{ConfigName, RpcErrorCode, ServiceName};
use ployz_store::RowId;
use ployz_store::{
    Actor, Batch, BatchCommand, Change, ConfigStore, CreateBranch, CreateProject, CreateService,
    DiffQuery, DiffView, Discard, Edit, EnvironmentId, EnvironmentName, EnvironmentQuery,
    EnvironmentRef, HintSource, NeverSync, OrganizationId, ProjectId, ProjectName, Publish,
    SecretValue, ServiceLineageId, ServiceQuery, ServicesQuery, SettingPath, SyncChanges,
    SyncQuery, SyncView, Take, Trusted,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

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

/// production runs `web` (image `web:1`, `PLAIN=1`) and `db`; Branch `fix-web` has
/// its own `web` and uses `db` live.
fn shop() -> (ConfigStore, Actor) {
    shop_in(backend::open())
}

fn shop_in(store: ConfigStore) -> (ConfigStore, Actor) {
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
                    environment: at("production"),
                    name: ServiceName::parse(name).unwrap(),
                    image: Some(image.into()),
                    template: None,
                },
            )
            .unwrap();
    }
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

/// `web`'s values in `environment`.
fn web(store: &ConfigStore, who: &Actor, environment: &str) -> Value {
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

fn diff(store: &ConfigStore, who: &Actor, environment: &str) -> DiffView {
    store
        .read(
            who,
            &DiffQuery {
                environment: at(environment),
            },
        )
        .unwrap()
}

/// What arrived in `environment` and isn't deployed, as `row from`, sorted.
fn incoming(store: &ConfigStore, who: &Actor, environment: &str) -> Vec<String> {
    let mut incoming: Vec<String> = diff(store, who, environment)
        .incoming
        .iter()
        .map(|change| format!("{} {}", change.at, change.from))
        .collect();
    incoming.sort();
    incoming
}

/// The Follow hints in `environment`, as `row = value from`.
fn hints(store: &ConfigStore, who: &Actor, environment: &str) -> Vec<String> {
    diff(store, who, environment)
        .follow_hints
        .iter()
        .map(|hint| format!("{} = {} from {}", hint.at, hint.value, hint.from))
        .collect()
}

/// `web`'s row `at`, as `variables.KEY` or `source`.
fn row(at: &str) -> RowId {
    format!("{}:{at}", uuid(3)).parse().unwrap()
}

fn take(parent: &str, into: &str, rows: &[&str], version: String) -> Take {
    Take {
        from: HintSource::Parent(EnvironmentName::parse(parent).unwrap()),
        into: Some(at(into)),
        rows: Some(rows.iter().map(|at| row(at).into()).collect()),
        version,
    }
}

fn discard(store: &ConfigStore, who: &Actor, environment: &str, path: &str) {
    store
        .write(
            who,
            &Discard {
                environment: at(environment),
                target: Some(ployz_store::DiscardTarget::Head),
                path: Some(SettingPath::parse(path).unwrap()),
                version: None,
            },
        )
        .unwrap();
}

/// The ID of `web` in `environment`: its own, not its lineage.
fn web_id(store: &ConfigStore, who: &Actor, environment: &str) -> String {
    store
        .read(
            who,
            &ServicesQuery {
                environment: at(environment),
            },
        )
        .unwrap()
        .services
        .into_iter()
        .find(|listing| listing.service.name.as_str() == "web")
        .unwrap()
        .service
        .id
        .to_string()
}

/// `web.env.TOKEN` as the browser sealed it for `environment`'s own `web`, holding
/// ciphertext `n`.
fn token(store: &ConfigStore, who: &Actor, environment: &str, n: u8) -> Value {
    let environment_id = match environment {
        "production" => uuid(2),
        "fix-web" => uuid(9),
        "qa" => uuid(10),
        other => panic!("{other}"),
    };
    backend::client_sealed(
        &environment_id,
        &web_id(store, who, environment),
        "TOKEN",
        n,
    )
}

/// The environment `web` deploys with in `environment`, deploying it as `n`.
fn deployed_env(store: &ConfigStore, who: &Actor, environment: &str, n: u8) -> Value {
    let input = deploy(store, who, environment, n);
    let id = web_id(store, who, environment);
    input["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|snapshot| snapshot["serviceId"] == id)
        .unwrap()["resolvedEnv"]
        .clone()
}

#[test]
fn a_parents_deploy_stages_its_changes_in_each_branch_once_tagged_with_where_from() {
    let (store, who) = shop();
    branch(&store, &who, 10, "production", "qa");
    set(
        &store,
        &who,
        "production",
        &[("web.image", json!("web:2")), ("web.env.NEW", json!("1"))],
    );
    // Staged in the Parent isn't deployed: nothing follows yet.
    assert_eq!(web(&store, &who, "fix-web")["image"], json!("web:1"));
    deploy(&store, &who, "production", 2);
    for branch in ["fix-web", "qa"] {
        let web = web(&store, &who, branch);
        assert_eq!(
            (&web["image"], &web["env"]["NEW"]),
            (&json!("web:2"), &json!("1"))
        );
        assert_eq!(
            incoming(&store, &who, branch),
            ["web.env.NEW production", "web.source production"]
        );
        assert!(hints(&store, &who, branch).is_empty());
    }

    // Edited after it arrived, it is the Branch's own.
    set(&store, &who, "qa", &[("web.env.NEW", json!("2"))]);
    assert_eq!(incoming(&store, &who, "qa"), ["web.source production"]);

    // Delivered once: deploying again stages nothing more.
    let before = diff(&store, &who, "fix-web").version;
    deploy(&store, &who, "production", 3);
    assert_eq!(diff(&store, &who, "fix-web").version, before);

    // Deployed in the Branch, it is the Branch's own: no longer incoming.
    deploy(&store, &who, "fix-web", 4);
    assert!(incoming(&store, &who, "fix-web").is_empty());
}

/// Details joins a change to what brought it by row, not by name: a healthcheck's
/// path is a part of the `healthcheck` row that followed.
#[test]
fn each_staged_change_names_the_row_it_falls_in() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.healthcheck", json!("/up"))],
    );
    deploy(&store, &who, "production", 2);
    deploy(&store, &who, "fix-web", 3);
    set(
        &store,
        &who,
        "production",
        &[
            ("web.healthcheck", json!("/live")),
            ("web.image", json!("web:2")),
        ],
    );
    deploy(&store, &who, "production", 4);

    let view = diff(&store, &who, "fix-web");
    let arrived: Vec<_> = view.incoming.iter().map(|change| change.at.row()).collect();
    let web = &view.changes[0];
    assert_eq!(*web.row.at(), At::Node);
    let rows: Vec<(&str, Option<&At>)> = web
        .settings
        .iter()
        .map(|row| (row.path.as_str(), row.row.as_ref().map(RowId::at)))
        .collect();
    assert_eq!(
        rows,
        [
            ("web.source", Some(&At::Setting(Setting::Source))),
            ("web.healthcheck", Some(&At::Setting(Setting::Healthcheck))),
        ]
    );
    for row in &web.settings {
        assert!(arrived.contains(&row.row.as_ref().unwrap()), "{}", row.path);
    }
}

#[test]
fn a_branchs_own_change_wins_and_the_parents_value_is_a_hint_to_take() {
    let (store, who) = shop();
    // fix-web changes the image and doesn't deploy it: Follow doesn't wait.
    set(&store, &who, "fix-web", &[("web.image", json!("web:mine"))]);
    set(
        &store,
        &who,
        "production",
        &[("web.image", json!("web:2")), ("web.env.PLAIN", json!("2"))],
    );
    deploy(&store, &who, "production", 2);
    let branch = web(&store, &who, "fix-web");
    assert_eq!(
        (&branch["image"], &branch["env"]["PLAIN"]),
        (&json!("web:mine"), &json!("2"))
    );
    assert_eq!(
        hints(&store, &who, "fix-web"),
        [r#"web.source = {"credentials":false,"image":"web:2","type":"image"} from production"#]
    );
    assert_eq!(
        incoming(&store, &who, "fix-web"),
        ["web.env.PLAIN production"]
    );

    // Only the Parent's hints, from the Branch's own Parent, are there to take.
    let wrong = store
        .write(
            &who,
            &take(
                "fix-web",
                "fix-web",
                &["source"],
                diff(&store, &who, "fix-web").version,
            ),
        )
        .unwrap_err();
    assert_eq!(wrong.code, RpcErrorCode::InvalidArgument);
    let unknown = store
        .write(
            &who,
            &take(
                "production",
                "fix-web",
                &["variables.PLAIN"],
                diff(&store, &who, "fix-web").version,
            ),
        )
        .unwrap_err();
    assert_eq!(unknown.code, RpcErrorCode::NotFound);
    let stale = store
        .write(
            &who,
            &take("production", "fix-web", &["source"], "0:0:0".into()),
        )
        .unwrap_err();
    assert_eq!(stale.code, RpcErrorCode::Conflict);

    let version = diff(&store, &who, "fix-web").version;
    let took = store
        .write(&who, &take("production", "fix-web", &["source"], version))
        .unwrap();
    assert_eq!(
        (took.from.name.as_str(), took.into.name.as_str()),
        ("production", "fix-web")
    );
    assert_eq!(web(&store, &who, "fix-web")["image"], json!("web:2"));
    assert!(hints(&store, &who, "fix-web").is_empty());
}

#[test]
fn a_discarded_change_stays_a_hint_until_the_parent_changes_that_setting_again() {
    let (store, who) = shop();
    set(&store, &who, "production", &[("web.image", json!("web:2"))]);
    deploy(&store, &who, "production", 2);
    assert_eq!(web(&store, &who, "fix-web")["image"], json!("web:2"));

    discard(&store, &who, "fix-web", "web.source");
    assert_eq!(web(&store, &who, "fix-web")["image"], json!("web:1"));
    let hint =
        [r#"web.source = {"credentials":false,"image":"web:2","type":"image"} from production"#];
    assert_eq!(hints(&store, &who, "fix-web"), hint);
    assert!(incoming(&store, &who, "fix-web").is_empty());

    // Not on every deploy: it stays a hint.
    deploy(&store, &who, "production", 3);
    assert_eq!(web(&store, &who, "fix-web")["image"], json!("web:1"));
    assert_eq!(hints(&store, &who, "fix-web"), hint);

    // production changes it again: that change follows.
    set(&store, &who, "production", &[("web.image", json!("web:3"))]);
    deploy(&store, &who, "production", 4);
    assert_eq!(web(&store, &who, "fix-web")["image"], json!("web:3"));
    assert!(hints(&store, &who, "fix-web").is_empty());
}

#[test]
fn a_discarded_secret_is_a_hint_and_the_next_rotation_still_follows() {
    let (store, who) = shop();
    let rotate = |secret: u8, n: u8| {
        let sealed = token(&store, &who, "production", secret);
        set(
            &store,
            &who,
            "production",
            &[("web.env.TOKEN", json!({ "secret": sealed }))],
        );
        deploy(&store, &who, "production", n);
    };
    rotate(1, 2);
    assert_eq!(
        incoming(&store, &who, "fix-web"),
        ["web.env.TOKEN production"]
    );
    discard(&store, &who, "fix-web", "web.env.TOKEN");
    assert_eq!(
        hints(&store, &who, "fix-web"),
        [r#"web.env.TOKEN = {"secret":true} from production"#]
    );
    assert_eq!(web(&store, &who, "fix-web")["env"].get("TOKEN"), None);
    assert_eq!(deployed_env(&store, &who, "fix-web", 3).get("TOKEN"), None);

    // The next rotation follows again. A value sealed for production never opens in
    // fix-web: it arrives Not set.
    rotate(2, 4);
    assert!(hints(&store, &who, "fix-web").is_empty());
    assert_eq!(
        incoming(&store, &who, "fix-web"),
        ["web.env.TOKEN production"]
    );
    assert_eq!(
        web(&store, &who, "fix-web")["env"]["TOKEN"],
        json!({ "secret": false })
    );
    assert_eq!(deployed_env(&store, &who, "fix-web", 5).get("TOKEN"), None);
}

#[test]
fn changes_cascade_one_level_per_deploy() {
    let (store, who) = shop();
    branch(&store, &who, 10, "fix-web", "child");
    set(&store, &who, "production", &[("web.image", json!("web:2"))]);
    deploy(&store, &who, "production", 2);
    assert_eq!(web(&store, &who, "fix-web")["image"], json!("web:2"));
    assert_eq!(web(&store, &who, "child")["image"], json!("web:1"));

    deploy(&store, &who, "fix-web", 3);
    assert_eq!(web(&store, &who, "child")["image"], json!("web:2"));
    assert_eq!(incoming(&store, &who, "child"), ["web.source fix-web"]);
}

#[test]
fn a_secret_follows_into_a_branch_that_never_set_its_own_and_not_one_that_did() {
    let (store, who) = shop();
    branch(&store, &who, 10, "production", "qa");
    let prod_1 = token(&store, &who, "production", 1);
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!({ "secret": prod_1 }))],
    );
    deploy(&store, &who, "production", 2);
    let qa_own = token(&store, &who, "qa", 2);
    set(
        &store,
        &who,
        "qa",
        &[("web.env.TOKEN", json!({ "secret": qa_own.clone() }))],
    );

    // production rotates it: fix-web follows, qa keeps its own and gets no hint.
    let prod_2 = token(&store, &who, "production", 3);
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!({ "secret": prod_2 }))],
    );
    deploy(&store, &who, "production", 3);
    assert!(hints(&store, &who, "qa").is_empty());
    assert!(incoming(&store, &who, "qa").is_empty());
    assert_eq!(
        incoming(&store, &who, "fix-web"),
        ["web.env.TOKEN production"]
    );
    assert_eq!(
        web(&store, &who, "fix-web")["env"]["TOKEN"],
        json!({ "secret": false })
    );
    assert_eq!(deployed_env(&store, &who, "fix-web", 4).get("TOKEN"), None);
    assert_eq!(
        deployed_env(&store, &who, "qa", 5)["TOKEN"],
        qa_own["value"]
    );
}

#[test]
fn a_followed_variable_the_branch_removes_is_its_own() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.image", json!("web:2")), ("web.env.NEW", json!("1"))],
    );
    deploy(&store, &who, "production", 2);
    store
        .write(
            &who,
            &Edit {
                environment: at("fix-web"),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse("web.env.NEW").unwrap(),
                }],
            },
        )
        .unwrap();
    assert_eq!(incoming(&store, &who, "fix-web"), ["web.source production"]);
}

#[test]
fn a_change_the_branch_refuses_is_a_hint_and_the_rest_still_follows() {
    let (store, who) = shop();
    // fix-web made its own `api`: production's `api` clashes with it by name.
    for (n, environment) in [(12, "fix-web"), (11, "production")] {
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(uuid(n)).unwrap(),
                    environment: at(environment),
                    name: ServiceName::parse("api").unwrap(),
                    image: Some(format!("api:{n}")),
                    template: None,
                },
            )
            .unwrap();
    }
    set(&store, &who, "production", &[("web.image", json!("web:2"))]);
    deploy(&store, &who, "production", 2);
    assert_eq!(web(&store, &who, "fix-web")["image"], json!("web:2"));
    assert_eq!(incoming(&store, &who, "fix-web"), ["web.source production"]);
    let hints = hints(&store, &who, "fix-web");
    assert!(
        hints.iter().any(|hint| hint.starts_with("api ")),
        "{hints:?}"
    );
}

/// Never sync from Details on `web`'s `path` in fix-web, the review at `version`:
/// discard what arrived and mark its row `at`, as one Batch.
fn never_sync_arrived(at_row: &str, path: &str, version: String) -> Batch {
    Batch {
        environment: at("fix-web"),
        commands: vec![
            BatchCommand::NeverSync(NeverSync {
                environment: EnvironmentRef::default(),
                rows: vec![row(at_row).into()],
                off: false,
            }),
            BatchCommand::Discard(Discard {
                environment: EnvironmentRef::default(),
                target: Some(ployz_store::DiscardTarget::Head),
                path: Some(SettingPath::parse(path).unwrap()),
                version: Some(version),
            }),
        ],
        expect: None,
    }
}

#[test]
fn never_sync_stages_a_published_arrival_reversal_until_save() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.healthcheck", json!("/old"))],
    );
    deploy(&store, &who, "production", 2);
    deploy(&store, &who, "fix-web", 3);
    let old = web(&store, &who, "fix-web")["healthcheck"].clone();
    set(
        &store,
        &who,
        "production",
        &[("web.healthcheck", json!("/new"))],
    );
    deploy(&store, &who, "production", 4);
    let publish = Publish {
        id: None,
        environment: at("fix-web"),
        version: Some(diff(&store, &who, "fix-web").version),
        message: None,
        accept_volume_loss: Vec::new(),
    };
    store
        .write_trusted(&who, &publish, &Trusted::default())
        .unwrap();
    let history = store
        .read(
            &who,
            &ployz_store::HistoryQuery {
                environment: at("fix-web"),
                limit: None,
            },
        )
        .unwrap()
        .revisions;
    let version = diff(&store, &who, "fix-web").version;
    store
        .write(
            &who,
            &never_sync_arrived("healthcheck", "web.healthcheck", version),
        )
        .unwrap();
    assert_eq!(web(&store, &who, "fix-web")["healthcheck"], old);
    assert_eq!(
        store
            .read(
                &who,
                &ployz_store::HistoryQuery {
                    environment: at("fix-web"),
                    limit: None,
                }
            )
            .unwrap()
            .revisions,
        history
    );
    let inverse = diff(&store, &who, "fix-web");
    assert!(!inverse.published);
    assert!(inverse.draft_count > 0);
    let excluded = store
        .read(
            &who,
            &EnvironmentQuery {
                environment: at("fix-web"),
                path: None,
                all: false,
            },
        )
        .unwrap()
        .never_synced;
    assert_eq!(excluded.len(), 1);
    store
        .write_trusted(
            &who,
            &Publish {
                id: None,
                environment: at("fix-web"),
                version: Some(inverse.version),
                message: None,
                accept_volume_loss: Vec::new(),
            },
            &Trusted::default(),
        )
        .unwrap();
    assert!(diff(&store, &who, "fix-web").published);
    assert_eq!(
        store
            .read(
                &who,
                &EnvironmentQuery {
                    environment: at("fix-web"),
                    path: None,
                    all: false
                }
            )
            .unwrap()
            .never_synced,
        excluded
    );
}

#[test]
fn never_sync_on_what_arrived_discards_it_and_marks_its_row_or_neither() {
    let (store, who) = shop();
    set(&store, &who, "production", &[("web.image", json!("web:2"))]);
    deploy(&store, &who, "production", 2);
    let marked = || {
        store
            .read(
                &who,
                &EnvironmentQuery {
                    environment: at("fix-web"),
                    path: None,
                    all: false,
                },
            )
            .unwrap()
            .never_synced
            .len()
    };

    // The review moved on: the discard is refused, and so is the mark.
    let stale = never_sync_arrived("source", "web.source", "0:stale".into());
    let refused = store.write(&who, &stale).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    assert_eq!(marked(), 0);
    assert_eq!(web(&store, &who, "fix-web")["image"], json!("web:2"));

    let version = diff(&store, &who, "fix-web").version;
    store
        .write(&who, &never_sync_arrived("source", "web.source", version))
        .unwrap();
    assert_eq!(marked(), 1);
    assert_eq!(web(&store, &who, "fix-web")["image"], json!("web:1"));
    assert!(incoming(&store, &who, "fix-web").is_empty());
    assert!(hints(&store, &who, "fix-web").is_empty());

    // production's next image doesn't follow.
    set(&store, &who, "production", &[("web.image", json!("web:3"))]);
    deploy(&store, &who, "production", 3);
    assert_eq!(web(&store, &who, "fix-web")["image"], json!("web:1"));
}

/// Edit `environment` as `changes` say, `None` unsetting, with Cloud's evidence for
/// the repositories `acme/web` and `acme/docs`.
fn edit_git(
    store: &ConfigStore,
    who: &Actor,
    environment: &str,
    changes: &[(&str, Option<Value>)],
) {
    let changes = changes
        .iter()
        .map(|(path, value)| {
            let path = SettingPath::parse(path).unwrap();
            match value {
                Some(value) => Change::Set {
                    path,
                    value: value.clone(),
                },
                None => Change::Unset { path },
            }
        })
        .collect();
    let edit = Edit {
        environment: at(environment),
        expect: None,
        changes,
    };
    store
        .write_trusted(who, &edit, &backend::acme_repositories())
        .unwrap();
}

#[test]
fn a_source_discarded_after_two_follows_lets_the_next_one_follow() {
    let (store, who) = shop();
    // `web` starts without a source on both sides.
    edit_git(&store, &who, "production", &[("web.image", None)]);
    branch(&store, &who, 10, "production", "fix-git");
    // Empty to Git, then Git to an image, each followed.
    edit_git(
        &store,
        &who,
        "production",
        &[("web.repository", Some(json!("acme/web")))],
    );
    deploy(&store, &who, "production", 4);
    edit_git(
        &store,
        &who,
        "production",
        &[
            ("web.repository", None),
            ("web.image", Some(json!("web:2"))),
        ],
    );
    deploy(&store, &who, "production", 5);
    assert_eq!(web(&store, &who, "fix-git")["image"], json!("web:2"));

    discard(&store, &who, "fix-git", "web.source");
    let values = web(&store, &who, "fix-git");
    assert_eq!(
        (&values["image"], &values["repository"]),
        (&Value::Null, &Value::Null)
    );

    // The base is production's source from before either Follow, so the next one follows.
    edit_git(
        &store,
        &who,
        "production",
        &[
            ("web.image", None),
            ("web.repository", Some(json!("acme/docs"))),
        ],
    );
    deploy(&store, &who, "production", 6);
    assert_eq!(hints(&store, &who, "fix-git"), Vec::<String>::new());
    assert_eq!(
        web(&store, &who, "fix-git")["repository"],
        json!("acme/docs")
    );
}

/// Include everything `from` offers into `into`'s draft as Sync `n`.
fn include(store: &ConfigStore, who: &Actor, from: &str, into: &str, n: u8) -> ployz_store::Synced {
    let view = store
        .read(
            who,
            &ployz_store::SyncQuery {
                from: at(from),
                into: Some(at(into)),
            },
        )
        .unwrap();
    store
        .write(
            who,
            &ployz_store::SyncChanges {
                from: at(from),
                into: Some(at(into)),
                close_after: false,
                version: view.version,
                picks: None,
                skip: Vec::new(),
                values: std::collections::BTreeMap::new(),
                id: Some(
                    ployz_store::SyncId::parse(format!("00000000-0000-4000-8000-0000000002{n:02}"))
                        .unwrap(),
                ),
            },
        )
        .unwrap()
}

/// `environment`'s proposals: source and how many rows each still owns.
fn included(store: &ConfigStore, who: &Actor, environment: &str) -> Vec<(String, usize)> {
    diff(store, who, environment)
        .included
        .into_iter()
        .map(|included| {
            let name = match included.source {
                ployz_store::ProposalSource::Environment { name, .. }
                | ployz_store::ProposalSource::PullRequest { name, .. } => name,
            };
            (name, included.changes)
        })
        .collect()
}

#[test]
fn a_follow_never_overwrites_a_row_a_proposal_owns() {
    let (store, who) = shop();
    branch(&store, &who, 10, "fix-web", "qa");
    set(&store, &who, "qa", &[("web.env.PLAIN", json!("qa"))]);
    include(&store, &who, "qa", "fix-web", 1);
    assert_eq!(included(&store, &who, "fix-web"), [("qa".to_owned(), 1)]);
    set(&store, &who, "production", &[("web.env.PLAIN", json!("2"))]);
    deploy(&store, &who, "production", 2);
    // fix-web keeps what it included; production's value is a hint, and the
    // proposal still owns its row.
    assert_eq!(web(&store, &who, "fix-web")["env"]["PLAIN"], json!("qa"));
    assert_eq!(
        hints(&store, &who, "fix-web"),
        [r#"web.env.PLAIN = "2" from production"#]
    );
    assert_eq!(included(&store, &who, "fix-web"), [("qa".to_owned(), 1)]);
}

#[test]
fn a_follow_into_a_draft_does_not_join_its_proposal() {
    let (store, who) = shop();
    branch(&store, &who, 10, "fix-web", "qa");
    set(&store, &who, "qa", &[("web.env.MINE", json!("qa"))]);
    include(&store, &who, "qa", "fix-web", 1);
    set(&store, &who, "production", &[("web.env.PLAIN", json!("2"))]);
    deploy(&store, &who, "production", 2);
    assert_eq!(web(&store, &who, "fix-web")["env"]["PLAIN"], json!("2"));
    // What followed is fix-web's, not qa's: only MINE is the proposal's.
    assert_eq!(included(&store, &who, "fix-web"), [("qa".to_owned(), 1)]);
}

#[test]
fn a_follow_landing_on_a_row_its_own_pair_owns_releases_it() {
    let (store, who) = shop();
    set(&store, &who, "production", &[("web.env.PLAIN", json!("2"))]);
    // fix-web includes production's draft, then production deploys a newer value.
    include(&store, &who, "production", "fix-web", 1);
    assert_eq!(
        included(&store, &who, "fix-web"),
        [("production".to_owned(), 1)]
    );
    set(&store, &who, "production", &[("web.env.PLAIN", json!("3"))]);
    deploy(&store, &who, "production", 2);
    assert_eq!(web(&store, &who, "fix-web")["env"]["PLAIN"], json!("3"));
    assert_eq!(
        included(&store, &who, "fix-web"),
        [("production".to_owned(), 0)]
    );
}

#[test]
fn a_service_the_parent_removes_and_adds_again_follows_into_a_readable_branch() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &ployz_store::RemoveService {
                environment: at("production"),
                service: ServiceName::parse("db").unwrap(),
            },
        )
        .unwrap();
    deploy(&store, &who, "production", 2);
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(5)).unwrap(),
                environment: at("production"),
                name: ServiceName::parse("db").unwrap(),
                image: Some("postgres:18".into()),
                template: None,
            },
        )
        .unwrap();
    deploy(&store, &who, "production", 3);
    set(&store, &who, "production", &[("web.env.PLAIN", json!("2"))]);
    deploy(&store, &who, "production", 4);

    assert_eq!(
        incoming(&store, &who, "fix-web"),
        ["db production", "web.env.PLAIN production"]
    );
}

/// The Services `environment`'s Working State holds, by name.
fn services(store: &ConfigStore, who: &Actor, environment: &str) -> Vec<String> {
    store
        .read(
            who,
            &ServicesQuery {
                environment: at(environment),
            },
        )
        .unwrap()
        .services
        .into_iter()
        .map(|listing| listing.service.name.to_string())
        .collect()
}

/// production adds `gone` with `SHARED=1` and deploys it as `n`, then removes it and
/// deploys that as `n + 1`.
fn add_and_remove_gone(store: &ConfigStore, who: &Actor, n: u8, between: impl FnOnce()) {
    store
        .write(
            who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(6)).unwrap(),
                environment: at("production"),
                name: ServiceName::parse("gone").unwrap(),
                image: Some("nginx:1.29-alpine".into()),
                template: None,
            },
        )
        .unwrap();
    set(store, who, "production", &[("gone.env.SHARED", json!("1"))]);
    deploy(store, who, "production", n);
    between();
    store
        .write(
            who,
            &ployz_store::RemoveService {
                environment: at("production"),
                service: ServiceName::parse("gone").unwrap(),
            },
        )
        .unwrap();
    deploy(store, who, "production", n + 1);
}

/// #3705: a Service the Parent adds and then removes leaves nothing staged in a
/// Branch that never deployed it.
#[test]
fn a_service_the_parent_adds_then_removes_follows_out_of_the_branch() {
    let (store, who) = shop();
    add_and_remove_gone(&store, &who, 2, || {
        assert!(services(&store, &who, "fix-web").contains(&"gone".to_owned()));
    });

    assert!(!services(&store, &who, "fix-web").contains(&"gone".to_owned()));
    assert!(
        diff(&store, &who, "fix-web")
            .changes
            .iter()
            .all(|change| change.name != "gone")
    );
    assert!(incoming(&store, &who, "fix-web").is_empty());
}

/// The Parent's own changes after it arrived don't make it the Branch's.
#[test]
fn a_service_the_parent_changed_then_removed_follows_out_of_the_branch() {
    let (store, who) = shop();
    add_and_remove_gone(&store, &who, 2, || {
        set(
            &store,
            &who,
            "production",
            &[("gone.image", json!("nginx:1.30-alpine"))],
        );
        deploy(&store, &who, "production", 10);
    });

    assert!(!services(&store, &who, "fix-web").contains(&"gone".to_owned()));
    assert!(
        diff(&store, &who, "fix-web")
            .changes
            .iter()
            .all(|change| change.name != "gone")
    );
}

/// A new Config the Parent mounts, then unmounts and removes in one deploy, leaves
/// the Branch with neither.
#[test]
fn a_config_the_parent_unmounts_and_removes_follows_out_of_the_branch() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse(uuid(7)).unwrap(),
                environment: at("production"),
                name: ConfigName::parse("extra").unwrap(),
                mounts: vec![ployz_store::ConfigMountAt {
                    service: ServiceName::parse("web").unwrap(),
                    dir: "/etc/extra".into(),
                }],
            },
        )
        .unwrap();
    deploy(&store, &who, "production", 2);
    let staged = |name: &str| {
        diff(&store, &who, "fix-web")
            .changes
            .iter()
            .any(|change| change.name == name)
    };
    assert!(staged("extra"));
    store
        .write(
            &who,
            &ployz_store::DetachConfig {
                environment: at("production"),
                service: ServiceName::parse("web").unwrap(),
                config: ConfigName::parse("extra").unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::DeleteConfig {
                environment: at("production"),
                config: ConfigName::parse("extra").unwrap(),
            },
        )
        .unwrap();
    deploy(&store, &who, "production", 3);

    assert!(!staged("extra"));
}

/// The ID of Service `name` in `environment`: its own, not its lineage.
fn service_id(store: &ConfigStore, who: &Actor, environment: &str, name: &str) -> String {
    store
        .read(
            who,
            &ServicesQuery {
                environment: at(environment),
            },
        )
        .unwrap()
        .services
        .into_iter()
        .find(|listing| listing.service.name.as_str() == name)
        .unwrap()
        .service
        .id
        .to_string()
}

fn create_gone(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(6)).unwrap(),
                environment: at("production"),
                name: ServiceName::parse("gone").unwrap(),
                image: Some("nginx:1.29-alpine".into()),
                template: None,
            },
        )
        .unwrap();
}

fn remove_gone(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &ployz_store::RemoveService {
                environment: at("production"),
                service: ServiceName::parse("gone").unwrap(),
            },
        )
        .unwrap();
}

/// Whether `environment` stages a change to node `name`.
fn staged(store: &ConfigStore, who: &Actor, environment: &str, name: &str) -> bool {
    diff(store, who, environment)
        .changes
        .iter()
        .any(|change| change.name == name)
}

/// A secret sealed for the Parent arrives Not set, which is as it arrived.
#[test]
fn a_service_with_a_parents_secret_follows_out_of_the_branch() {
    let (store, who) = shop();
    create_gone(&store, &who);
    let sealed = backend::client_sealed(
        &uuid(2),
        &service_id(&store, &who, "production", "gone"),
        "TOKEN",
        1,
    );
    set(
        &store,
        &who,
        "production",
        &[("gone.env.TOKEN", json!({ "secret": sealed }))],
    );
    deploy(&store, &who, "production", 2);
    assert!(staged(&store, &who, "fix-web", "gone"));
    remove_gone(&store, &who);
    deploy(&store, &who, "production", 3);

    assert!(!staged(&store, &who, "fix-web", "gone"));
}

/// What the Parent described or exported arrives with the variable, so it is as it
/// arrived.
#[test]
fn a_service_with_a_variable_the_parent_exported_follows_out_of_the_branch() {
    let (store, who) = shop();
    create_gone(&store, &who);
    set(
        &store,
        &who,
        "production",
        &[("gone.env.SHARED", json!({ "value": "1", "exported": true }))],
    );
    deploy(&store, &who, "production", 2);
    assert!(staged(&store, &who, "fix-web", "gone"));
    remove_gone(&store, &who);
    deploy(&store, &who, "production", 3);

    assert!(!staged(&store, &who, "fix-web", "gone"));
}

/// Two new Services, one referencing the other, removed in one deploy, both leave,
/// whichever is tried first.
#[test]
fn a_service_and_the_one_referencing_it_follow_out_of_the_branch_together() {
    let (store, who) = shop();
    // Its lineage sorts before `gone`'s, so it is tried while still referenced.
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(5)).unwrap(),
                environment: at("production"),
                name: ServiceName::parse("alpha").unwrap(),
                image: Some("nginx:1.29-alpine".into()),
                template: None,
            },
        )
        .unwrap();
    create_gone(&store, &who);
    set(
        &store,
        &who,
        "production",
        &[("gone.env.ALPHA_URL", json!("${{ alpha.PRIVATE_URL }}"))],
    );
    deploy(&store, &who, "production", 2);
    assert!(staged(&store, &who, "fix-web", "alpha"));
    for name in ["gone", "alpha"] {
        store
            .write(
                &who,
                &ployz_store::RemoveService {
                    environment: at("production"),
                    service: ServiceName::parse(name).unwrap(),
                },
            )
            .unwrap();
    }
    deploy(&store, &who, "production", 3);

    assert!(!staged(&store, &who, "fix-web", "gone"));
    assert!(!staged(&store, &who, "fix-web", "alpha"));
}

/// A node another of the Branch's nodes references stays, so the reference holds.
#[test]
fn a_service_the_branch_references_stays_when_the_parent_removes_it() {
    let (store, who) = shop();
    create_gone(&store, &who);
    deploy(&store, &who, "production", 2);
    set(
        &store,
        &who,
        "fix-web",
        &[("web.env.GONE_URL", json!("${{ gone.PRIVATE_URL }}"))],
    );
    remove_gone(&store, &who);
    deploy(&store, &who, "production", 3);

    assert!(staged(&store, &who, "fix-web", "gone"));
}

/// A new Config mounted on a new Service, both removed in one deploy, both leave,
/// whichever is tried first.
#[test]
fn a_config_and_the_service_mounting_it_follow_out_of_the_branch_together() {
    let (store, who) = shop();
    create_gone(&store, &who);
    // Its lineage sorts before the Service's, so it is tried while still mounted.
    store
        .write(
            &who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse(uuid(5)).unwrap(),
                environment: at("production"),
                name: ConfigName::parse("extra").unwrap(),
                mounts: vec![ployz_store::ConfigMountAt {
                    service: ServiceName::parse("gone").unwrap(),
                    dir: "/etc/extra".into(),
                }],
            },
        )
        .unwrap();
    deploy(&store, &who, "production", 2);
    assert!(staged(&store, &who, "fix-web", "gone"));
    assert!(staged(&store, &who, "fix-web", "extra"));
    remove_gone(&store, &who);
    store
        .write(
            &who,
            &ployz_store::DeleteConfig {
                environment: at("production"),
                config: ConfigName::parse("extra").unwrap(),
            },
        )
        .unwrap();
    deploy(&store, &who, "production", 3);

    assert!(!staged(&store, &who, "fix-web", "gone"));
    assert!(!staged(&store, &who, "fix-web", "extra"));
}

/// A Branch that changed the Service it followed keeps it: a Follow never removes
/// what a Branch made its own.
#[test]
fn a_service_the_branch_edited_stays_when_the_parent_removes_it() {
    let (store, who) = shop();
    add_and_remove_gone(&store, &who, 2, || {
        set(&store, &who, "fix-web", &[("gone.env.MINE", json!("1"))]);
    });

    // A removal never moves, so it is no hint either: the Service stays as the
    // Branch left it, staged.
    assert!(services(&store, &who, "fix-web").contains(&"gone".to_owned()));
    assert!(hints(&store, &who, "fix-web").is_empty());
    assert!(
        diff(&store, &who, "fix-web")
            .changes
            .iter()
            .any(|change| change.name == "gone")
    );
}

/// A change no row tracks, such as a variable's export, makes it the Branch's own too.
#[test]
fn a_service_the_branch_exported_a_variable_of_stays_when_the_parent_removes_it() {
    let (store, who) = shop();
    add_and_remove_gone(&store, &who, 2, || {
        set(
            &store,
            &who,
            "fix-web",
            &[("gone.env.SHARED.exported", json!(true))],
        );
    });

    assert!(services(&store, &who, "fix-web").contains(&"gone".to_owned()));
}

/// A Branch that deployed the Service it followed keeps it.
#[test]
fn a_service_the_branch_deployed_stays_when_the_parent_removes_it() {
    let (store, who) = shop();
    add_and_remove_gone(&store, &who, 2, || {
        deploy(&store, &who, "fix-web", 10);
    });

    // Left out: nothing staged, nothing to take, and it keeps running.
    assert!(services(&store, &who, "fix-web").contains(&"gone".to_owned()));
    assert!(diff(&store, &who, "fix-web").changes.is_empty());
    assert!(hints(&store, &who, "fix-web").is_empty());
}

/// A Branch that saved the Service it followed keeps it, as if it deployed it.
#[test]
fn a_service_the_branch_saved_stays_when_the_parent_removes_it() {
    let (store, who) = shop();
    add_and_remove_gone(&store, &who, 2, || {
        let publish = Publish {
            id: None,
            environment: at("fix-web"),
            version: Some(diff(&store, &who, "fix-web").version),
            message: None,
            accept_volume_loss: Vec::new(),
        };
        store
            .write_trusted(&who, &publish, &Trusted::default())
            .unwrap();
    });

    assert!(services(&store, &who, "fix-web").contains(&"gone".to_owned()));
}

#[test]
fn a_left_out_service_the_parent_removes_and_undoes_stays_left_out() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &ployz_store::RemoveService {
                environment: at("production"),
                service: ServiceName::parse("db").unwrap(),
            },
        )
        .unwrap();
    set(&store, &who, "production", &[("web.env.PLAIN", json!("2"))]);
    deploy(&store, &who, "production", 2);
    let undo = ployz_store::HistoryPreviewQuery {
        environment: at("production"),
        revision: ployz_store::Revision(2),
        action: ployz_store::HistoryAction::Undo,
        change: None,
    };
    let preview = store.read(&who, &undo).unwrap();
    store
        .write(
            &who,
            &ployz_store::StageHistory {
                environment: at("production"),
                revision: undo.revision,
                action: undo.action,
                change: None,
                version: preview.version,
                accept_overwrite: true,
            },
        )
        .unwrap();
    deploy(&store, &who, "production", 3);

    assert_eq!(
        incoming(&store, &who, "fix-web"),
        ["web.env.PLAIN production"]
    );
}

#[test]
fn a_secret_a_branch_synced_to_its_parent_does_not_follow_back() {
    // With a value sealed for production, and with none (it lands Not set there).
    for given in [Some(2), None] {
        let (store, who) = shop();
        let own = token(&store, &who, "fix-web", 1);
        set(
            &store,
            &who,
            "fix-web",
            &[("web.env.TOKEN", json!({ "secret": own.clone() }))],
        );
        let view: SyncView = store
            .read(
                &who,
                &SyncQuery {
                    from: at("fix-web"),
                    into: Some(at("production")),
                },
            )
            .unwrap();
        let values = given
            .map(|n| {
                let sealed = token(&store, &who, "production", n);
                (
                    "web.env.TOKEN".into(),
                    serde_json::from_value(sealed).unwrap(),
                )
            })
            .into_iter()
            .collect();
        store
            .write(
                &who,
                &SyncChanges {
                    from: at("fix-web"),
                    into: Some(at("production")),
                    close_after: false,
                    version: view.version,
                    picks: Some(vec!["web.env.TOKEN".into()]),
                    skip: Vec::new(),
                    values,
                    id: None,
                },
            )
            .unwrap();
        deploy(&store, &who, "production", 2);

        assert!(incoming(&store, &who, "fix-web").is_empty(), "{given:?}");
        assert!(hints(&store, &who, "fix-web").is_empty(), "{given:?}");
        assert_eq!(
            web(&store, &who, "fix-web")["env"]["TOKEN"],
            json!({ "secret": true }),
            "{given:?}"
        );
        assert_eq!(
            deployed_env(&store, &who, "fix-web", 3)["TOKEN"],
            own["value"],
            "{given:?}"
        );

        // production then sets its own: sealed for production, it would land Not set.
        let rotated = token(&store, &who, "production", 5);
        set(
            &store,
            &who,
            "production",
            &[("web.env.TOKEN", json!({ "secret": rotated }))],
        );
        deploy(&store, &who, "production", 4);
        assert!(incoming(&store, &who, "fix-web").is_empty(), "{given:?}");
        assert_eq!(
            deployed_env(&store, &who, "fix-web", 5)["TOKEN"],
            own["value"],
            "{given:?}"
        );
    }
}

#[test]
fn a_secret_the_parent_synced_into_its_branch_with_the_branchs_value_does_not_follow() {
    let (store, who) = shop();
    let parents = token(&store, &who, "production", 1);
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!({ "secret": parents }))],
    );
    let view: SyncView = store
        .read(
            &who,
            &SyncQuery {
                from: at("production"),
                into: Some(at("fix-web")),
            },
        )
        .unwrap();
    let own = token(&store, &who, "fix-web", 2);
    store
        .write(
            &who,
            &SyncChanges {
                from: at("production"),
                into: Some(at("fix-web")),
                close_after: false,
                version: view.version,
                picks: Some(vec!["web.env.TOKEN".into()]),
                skip: Vec::new(),
                values: BTreeMap::from([(
                    "web.env.TOKEN".into(),
                    serde_json::from_value::<SecretValue>(own.clone()).unwrap(),
                )]),
                id: None,
            },
        )
        .unwrap();
    deploy(&store, &who, "fix-web", 2);
    deploy(&store, &who, "production", 3);

    // production deployed the value it synced: fix-web keeps the one it was given.
    assert!(incoming(&store, &who, "fix-web").is_empty());
    assert_eq!(
        deployed_env(&store, &who, "fix-web", 4)["TOKEN"],
        own["value"]
    );
}

/// Sealing a plain value a Branch follows leaves no copy of it beside the Branch.
#[test]
fn a_seal_that_follows_leaves_no_plain_text_in_the_arrival() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    seal_token(&store, &who, 3);
    assert_eq!(
        incoming(&store, &who, "fix-web"),
        ["web.env.TOKEN production"]
    );
    assert_eq!(
        web(&store, &who, "fix-web")["env"]["TOKEN"],
        json!({ "secret": false })
    );
    assert_eq!(backend::tables_holding(&url, "plain-private"), [""; 0]);
}

/// A Branch made while the value was plain keeps no copy of it in what it syncs over,
/// only in its Node Introduction. The seal follows into it cleanly, and its review
/// still lists it, from sealed rather than from the plain value (#3587).
#[test]
fn a_seal_that_follows_into_a_branch_made_while_plain_leaves_no_plain_base() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    seal_token(&store, &who, 3);
    assert_eq!(incoming(&store, &who, "qa"), ["web.env.TOKEN production"]);
    assert_eq!(
        web(&store, &who, "qa")["env"]["TOKEN"],
        json!({ "secret": false })
    );
    assert!(
        diff(&store, &who, "qa")
            .changes
            .iter()
            .flat_map(|change| &change.settings)
            .any(|setting| setting.path == "web.env.TOKEN")
    );
    let shown = serde_json::to_string(&diff(&store, &who, "qa")).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
    assert_eq!(
        backend::tables_holding(&url, "plain-private"),
        ["config_node_introduction"]
    );
    deploy(&store, &who, "qa", 4);
    assert_eq!(
        backend::tables_holding(&url, "plain-private"),
        ["config_node_introduction"]
    );
}

/// A Branch that deletes a variable sealed since it was made reviews the deletion
/// from sealed, not from the plain value its Node Introduction keeps.
#[test]
fn a_branch_deleting_a_value_sealed_since_it_was_made_never_shows_the_plain_value() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    seal_token(&store, &who, 3);
    store
        .write(
            &who,
            &Edit {
                environment: at("qa"),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse("web.env.TOKEN").unwrap(),
                }],
            },
        )
        .unwrap();

    let review = diff(&store, &who, "qa");
    let token = review
        .changes
        .iter()
        .flat_map(|change| &change.settings)
        .find(|setting| setting.path == "web.env.TOKEN")
        .unwrap();
    assert_eq!(token.before, json!({ "secret": true }));
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
}

/// A Branch that deletes a variable sealed since it was made, then adds it again
/// plain, reviews the new value against sealed, not against the earlier plain one.
#[test]
fn a_branch_adding_back_plain_a_value_sealed_since_it_was_made_never_shows_the_plain_value() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    seal_token(&store, &who, 3);
    unset(&store, &who, "qa", "web.env.TOKEN");
    set(&store, &who, "qa", &[("web.env.TOKEN", json!("plain-new"))]);

    let review = diff(&store, &who, "qa");
    assert_eq!(
        token_row(&review),
        (json!({ "secret": true }), json!("plain-new"))
    );
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
}

/// A Branch that deleted a variable before its Parent sealed it reviews the
/// deletion from sealed.
#[test]
fn a_branch_that_deleted_a_value_before_its_parent_sealed_it_never_shows_the_plain_value() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    unset(&store, &who, "qa", "web.env.TOKEN");
    seal_token(&store, &who, 3);

    let review = diff(&store, &who, "qa");
    assert_eq!(token_row(&review), (json!({ "secret": true }), Value::Null));
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
}

/// Discarding a Branch's delete of a variable its Parent sealed since brings it
/// back Not set, never as the earlier plain value.
#[test]
fn discarding_a_delete_its_parent_sealed_since_brings_it_back_not_set() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    unset(&store, &who, "qa", "web.env.TOKEN");
    seal_token(&store, &who, 3);

    discard(&store, &who, "qa", "web.env.TOKEN");

    let values = web(&store, &who, "qa");
    assert_eq!(values["env"]["TOKEN"], json!({ "secret": false }));
    // The row stays: the variable needs a value before it deploys.
    assert_eq!(
        token_row(&diff(&store, &who, "qa")),
        (json!({ "secret": true }), json!({ "secret": false }))
    );
    let shown = serde_json::to_string(&diff(&store, &who, "qa")).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
}

/// Discarding a Branch's delete of a variable its Parent replaced with a plain one
/// after a seal brings back that plain value.
#[test]
fn discarding_a_delete_its_parent_added_plain_after_a_seal_brings_the_value_back() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    seal_token(&store, &who, 3);
    unset(&store, &who, "production", "web.env.TOKEN");
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("public-new"))],
    );
    deploy(&store, &who, "production", 4);
    branch(&store, &who, 10, "production", "qa");
    unset(&store, &who, "qa", "web.env.TOKEN");

    discard(&store, &who, "qa", "web.env.TOKEN");

    assert_eq!(web(&store, &who, "qa")["env"]["TOKEN"], json!("public-new"));
}

/// A Branch's in-place plain edit of a variable its Parent sealed since reviews
/// from sealed, never from the plain value it was made with.
#[test]
fn an_in_place_edit_made_before_its_parent_sealed_never_shows_the_plain_value() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    set(
        &store,
        &who,
        "qa",
        &[("web.env.TOKEN", json!("plain-edit"))],
    );
    seal_token(&store, &who, 3);

    let review = diff(&store, &who, "qa");
    assert_eq!(
        token_row(&review),
        (json!({ "secret": true }), json!("plain-edit"))
    );
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
}

/// Discarding that edit brings the variable back Not set.
#[test]
fn discarding_an_in_place_edit_its_parent_sealed_since_brings_it_back_not_set() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    set(
        &store,
        &who,
        "qa",
        &[("web.env.TOKEN", json!("plain-edit"))],
    );
    seal_token(&store, &who, 3);

    discard(&store, &who, "qa", "web.env.TOKEN");

    let values = web(&store, &who, "qa");
    assert_eq!(values["env"]["TOKEN"], json!({ "secret": false }));
    let shown = serde_json::to_string(&diff(&store, &who, "qa")).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
}

/// An in-place edit of a variable nothing sealed still shows the value it replaced.
#[test]
fn an_in_place_edit_of_a_plain_value_shows_the_value_it_replaced() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    set(
        &store,
        &who,
        "qa",
        &[("web.env.TOKEN", json!("plain-edit"))],
    );

    assert_eq!(
        token_row(&diff(&store, &who, "qa")),
        (json!("plain-private"), json!("plain-edit"))
    );
}

/// A Branch of a Branch that deleted a variable before the root sealed it never
/// shows the plain value, though its Parent deleted it too and so took no seal.
#[test]
fn a_branch_of_a_branch_that_deleted_a_value_before_the_root_sealed_it_never_shows_it() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    branch(&store, &who, 11, "qa", "qa-two");
    unset(&store, &who, "qa", "web.env.TOKEN");
    unset(&store, &who, "qa-two", "web.env.TOKEN");
    seal_token(&store, &who, 3);

    let review = diff(&store, &who, "qa-two");
    assert_eq!(token_row(&review), (json!({ "secret": true }), Value::Null));
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
}

/// Nor does one three Branches down.
#[test]
fn a_branch_three_down_that_deleted_a_value_before_the_root_sealed_it_never_shows_it() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    branch(&store, &who, 11, "qa", "qa-two");
    branch(&store, &who, 12, "qa-two", "qa-three");
    unset(&store, &who, "qa-three", "web.env.TOKEN");
    seal_token(&store, &who, 3);

    let review = diff(&store, &who, "qa-three");
    assert_eq!(token_row(&review), (json!({ "secret": true }), Value::Null));
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
}

/// A Parent's rename of a variable the root sealed since keeps its seal for a
/// Branch made from the Parent after the rename.
#[test]
fn a_branch_of_a_branch_follows_a_renamed_variable_to_the_root_seal() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    store
        .write(
            &who,
            &Edit {
                environment: at("qa"),
                expect: None,
                changes: vec![Change::Rename {
                    path: SettingPath::parse("web.env.TOKEN").unwrap(),
                    to: "API_TOKEN".to_owned(),
                    update_references: false,
                }],
            },
        )
        .unwrap();
    branch(&store, &who, 11, "qa", "qa-two");
    unset(&store, &who, "qa", "web.env.API_TOKEN");
    unset(&store, &who, "qa-two", "web.env.API_TOKEN");
    seal_token(&store, &who, 3);

    let review = diff(&store, &who, "qa-two");
    let row = review
        .changes
        .iter()
        .flat_map(|change| &change.settings)
        .find(|setting| setting.path == "web.env.API_TOKEN")
        .unwrap();
    assert_eq!(
        (row.before.clone(), row.after.clone()),
        (json!({ "secret": true }), Value::Null)
    );
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
}

/// A Parent's plain variable added after the root sealed one under its key isn't
/// the one the root sealed: a Branch made from it that deletes it still shows it.
#[test]
fn a_branch_of_a_branch_deleting_a_value_its_parent_added_plain_after_a_seal_still_shows_it() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    seal_token(&store, &who, 3);
    unset(&store, &who, "qa", "web.env.TOKEN");
    set(
        &store,
        &who,
        "qa",
        &[("web.env.TOKEN", json!("public-new"))],
    );
    branch(&store, &who, 11, "qa", "qa-two");
    unset(&store, &who, "qa-two", "web.env.TOKEN");

    assert_eq!(
        token_row(&diff(&store, &who, "qa-two")),
        (json!("public-new"), Value::Null)
    );
}

/// Discarding a Branch's change to a variable it sealed since it was made never
/// overwrites the sealed value, with the earlier plain one or with Not set.
#[test]
fn discarding_a_variable_a_branch_sealed_since_keeps_its_sealed_value() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    let sealed = token(&store, &who, "qa", 1);
    set(
        &store,
        &who,
        "qa",
        &[("web.env.TOKEN", json!({ "secret": sealed }))],
    );

    let refused = store
        .write(
            &who,
            &Discard {
                environment: at("qa"),
                target: Some(ployz_store::DiscardTarget::Head),
                path: Some(SettingPath::parse("web.env.TOKEN").unwrap()),
                version: None,
            },
        )
        .unwrap_err();

    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    assert_eq!(
        web(&store, &who, "qa")["env"]["TOKEN"],
        json!({ "secret": true })
    );
}

/// Discarding a Branch's delete of a variable nothing sealed brings back its value.
#[test]
fn discarding_a_delete_nothing_sealed_brings_the_value_back() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    unset(&store, &who, "qa", "web.env.TOKEN");

    discard(&store, &who, "qa", "web.env.TOKEN");

    assert_eq!(
        web(&store, &who, "qa")["env"]["TOKEN"],
        json!("plain-private")
    );
}

/// A Parent's seal of a variable it has since replaced with a plain one doesn't
/// hide the plain one a Branch made afterwards deletes.
#[test]
fn a_branch_deleting_a_value_its_parent_added_plain_after_a_seal_still_shows_it() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    seal_token(&store, &who, 3);
    unset(&store, &who, "production", "web.env.TOKEN");
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("public-new"))],
    );
    deploy(&store, &who, "production", 4);
    branch(&store, &who, 10, "production", "qa");
    unset(&store, &who, "qa", "web.env.TOKEN");

    assert_eq!(
        token_row(&diff(&store, &who, "qa")),
        (json!("public-new"), Value::Null)
    );
}

/// A value nothing sealed stays readable in a Branch's review after it adds the
/// variable again.
#[test]
fn a_branch_adding_back_a_value_nothing_sealed_still_shows_it() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    unset(&store, &who, "qa", "web.env.TOKEN");
    set(&store, &who, "qa", &[("web.env.TOKEN", json!("plain-new"))]);

    assert_eq!(
        token_row(&diff(&store, &who, "qa")),
        (json!("plain-private"), json!("plain-new"))
    );
}

/// A Branch that saved a value plain, took its Parent's seal of it, then added it
/// again plain keeps no earlier plain value in History, while the plain value added
/// since stays.
#[test]
fn a_branch_adding_back_plain_a_value_sealed_since_keeps_no_earlier_plain_value_in_history() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    deploy(&store, &who, "qa", 3);
    seal_token(&store, &who, 4);
    assert_eq!(
        web(&store, &who, "qa")["env"]["TOKEN"],
        json!({ "secret": false })
    );
    unset(&store, &who, "qa", "web.env.TOKEN");
    set(&store, &who, "qa", &[("web.env.TOKEN", json!("plain-new"))]);
    deploy(&store, &who, "qa", 5);

    assert!(!backend::tables_holding(&url, "plain-private").contains(&"config_saved".to_owned()));
    assert!(backend::tables_holding(&url, "plain-new").contains(&"config_saved".to_owned()));
}

fn unset(store: &ConfigStore, who: &Actor, environment: &str, path: &str) {
    store
        .write(
            who,
            &Edit {
                environment: at(environment),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse(path).unwrap(),
                }],
            },
        )
        .unwrap();
}

/// The `before` and `after` of the review's `web.env.TOKEN` row.
fn token_row(review: &DiffView) -> (Value, Value) {
    let token = review
        .changes
        .iter()
        .flat_map(|change| &change.settings)
        .find(|setting| setting.path == "web.env.TOKEN")
        .unwrap();
    (token.before.clone(), token.after.clone())
}

/// Sealing a value in a Working State empties it in the bases its Branch shares,
/// before anything is saved or deployed.
#[test]
fn sealing_a_value_empties_it_in_the_bases_a_branch_shares() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    let sealed = token(&store, &who, "production", 1);
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!({ "secret": sealed }))],
    );

    let holding = backend::tables_holding(&url, "plain-private");
    assert!(
        !holding.contains(&"config_environment_branch".to_owned())
            && !holding.contains(&"config_sync_base".to_owned()),
        "{holding:?}"
    );
}

/// Seal production's `web.env.TOKEN` and deploy it as `n`.
fn seal_token(store: &ConfigStore, who: &Actor, n: u8) {
    let sealed = token(store, who, "production", 1);
    set(
        store,
        who,
        "production",
        &[("web.env.TOKEN", json!({ "secret": sealed }))],
    );
    deploy(store, who, "production", n);
}
