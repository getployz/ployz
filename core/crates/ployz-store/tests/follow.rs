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
use backend::{deploy, deploy_in_batches};

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

/// Nor does one three Branches down, though neither Branch above it took the seal.
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
    unset(&store, &who, "qa", "web.env.TOKEN");
    unset(&store, &who, "qa-two", "web.env.TOKEN");
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

/// A Parent's own in-place change isn't what the root sealed: a Branch made from
/// it after the change that deletes it still shows it, and a discard brings it back.
#[test]
fn a_branch_of_a_branch_deleting_a_value_its_parent_changed_before_a_seal_still_shows_it() {
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
    branch(&store, &who, 11, "qa", "qa-two");
    unset(&store, &who, "qa-two", "web.env.TOKEN");
    seal_token(&store, &who, 3);

    assert_eq!(
        token_row(&diff(&store, &who, "qa-two")),
        (json!("plain-edit"), Value::Null)
    );
    discard(&store, &who, "qa-two", "web.env.TOKEN");
    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!("plain-edit")
    );
}

/// A Branch of a Branch that changes in place a value the root sealed since
/// reviews it from sealed.
#[test]
fn a_branch_of_a_branch_changing_a_value_before_the_root_sealed_it_never_shows_it() {
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
    set(
        &store,
        &who,
        "qa-two",
        &[("web.env.TOKEN", json!("plain-edit"))],
    );
    seal_token(&store, &who, 3);

    let review = diff(&store, &who, "qa-two");
    assert_eq!(
        token_row(&review),
        (json!({ "secret": true }), json!("plain-edit"))
    );
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
}

/// A Branch's rename of a variable its Parent sealed since reviews the old name
/// from sealed.
#[test]
fn a_rename_made_before_its_parent_sealed_reviews_the_old_name_from_sealed() {
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
    seal_token(&store, &who, 3);

    let review = diff(&store, &who, "qa");
    assert_eq!(
        token_row(&review).0,
        json!({ "secret": true }),
        "{review:?}"
    );
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

/// production holds `web.env.TOKEN` plain and deployed, and `qa`, made from it,
/// deployed it too.
fn deployed_qa(store: &ConfigStore, who: &Actor) {
    set(
        store,
        who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(store, who, "production", 2);
    branch(store, who, 10, "production", "qa");
    deploy(store, who, "qa", 20);
}

/// A Branch that deployed a value and then changed it in place before its Parent
/// sealed it reviews from sealed, never from the value it runs.
#[test]
fn a_deployed_branchs_in_place_edit_made_before_its_parent_sealed_never_shows_the_plain_value() {
    let (store, who) = shop();
    deployed_qa(&store, &who);
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
fn discarding_a_deployed_branchs_edit_its_parent_sealed_since_brings_it_back_not_set() {
    let (store, who) = shop();
    deployed_qa(&store, &who);
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

/// A deployed Branch that took its Parent's seal after deleting the variable
/// reviews from sealed, and a discard keeps it Not set.
#[test]
fn a_deployed_branch_that_took_its_parents_seal_never_shows_or_discards_back_the_plain_value() {
    let (store, who) = shop();
    deployed_qa(&store, &who);
    unset(&store, &who, "qa", "web.env.TOKEN");
    seal_token(&store, &who, 3);
    let version = diff(&store, &who, "qa").version;
    store
        .write(
            &who,
            &take("production", "qa", &["variables.TOKEN"], version),
        )
        .unwrap();

    let review = diff(&store, &who, "qa");
    assert_eq!(token_row(&review).0, json!({ "secret": true }));
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
    discard(&store, &who, "qa", "web.env.TOKEN");
    assert_eq!(
        web(&store, &who, "qa")["env"]["TOKEN"],
        json!({ "secret": false })
    );
}

/// An undeployed Branch that took its Parent's seal after deleting the variable
/// discards it back Not set, never as the plain value it was made with.
#[test]
fn a_branch_that_took_its_parents_seal_discards_it_back_not_set() {
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
    let version = diff(&store, &who, "qa").version;
    store
        .write(
            &who,
            &take("production", "qa", &["variables.TOKEN"], version),
        )
        .unwrap();

    discard(&store, &who, "qa", "web.env.TOKEN");

    assert_eq!(
        web(&store, &who, "qa")["env"]["TOKEN"],
        json!({ "secret": false })
    );
}

/// A deployed Branch of a Branch that changed a value in place before the root
/// sealed it reviews from sealed.
#[test]
fn a_deployed_branch_of_a_branch_changing_a_value_before_the_root_sealed_it_never_shows_it() {
    let (store, who) = shop();
    deployed_qa(&store, &who);
    branch(&store, &who, 11, "qa", "qa-two");
    deploy(&store, &who, "qa-two", 21);
    unset(&store, &who, "qa", "web.env.TOKEN");
    set(
        &store,
        &who,
        "qa-two",
        &[("web.env.TOKEN", json!("plain-edit"))],
    );
    seal_token(&store, &who, 3);

    let review = diff(&store, &who, "qa-two");
    assert_eq!(
        token_row(&review),
        (json!({ "secret": true }), json!("plain-edit"))
    );
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
}

/// The Environment that seals a value it runs plain reviews that change from
/// sealed before the seal deploys.
#[test]
fn sealing_a_deployed_value_never_shows_it_before_the_seal_deploys() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    let sealed = token(&store, &who, "production", 1);
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!({ "secret": sealed }))],
    );

    let review = diff(&store, &who, "production");
    assert_eq!(token_row(&review).0, json!({ "secret": true }));
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
}

/// A sealed value with a value is never overwritten by a discard, with the plain
/// value it replaced or with Not set.
#[test]
fn discarding_a_seal_before_it_deploys_never_brings_the_plain_value_back() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 2);
    let sealed = token(&store, &who, "production", 1);
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!({ "secret": sealed }))],
    );

    let discarded = store.write(
        &who,
        &Discard {
            environment: at("production"),
            target: Some(ployz_store::DiscardTarget::Head),
            path: Some(SettingPath::parse("web.env.TOKEN").unwrap()),
            version: None,
        },
    );

    assert_eq!(discarded.unwrap_err().code, RpcErrorCode::Conflict);
    assert_eq!(
        web(&store, &who, "production")["env"]["TOKEN"],
        json!({ "secret": true })
    );
}

/// A value a deployed Branch set and deployed itself before any seal was never
/// sealed: its review still shows it, and a discard brings it back.
#[test]
fn a_deployed_branchs_own_deployed_value_still_shows_after_its_parent_seals() {
    let (store, who) = shop();
    deployed_qa(&store, &who);
    set(&store, &who, "qa", &[("web.env.TOKEN", json!("plain-own"))]);
    deploy(&store, &who, "qa", 22);
    set(
        &store,
        &who,
        "qa",
        &[("web.env.TOKEN", json!("plain-edit"))],
    );
    seal_token(&store, &who, 3);

    assert_eq!(
        token_row(&diff(&store, &who, "qa")),
        (json!("plain-own"), json!("plain-edit"))
    );
    discard(&store, &who, "qa", "web.env.TOKEN");
    assert_eq!(web(&store, &who, "qa")["env"]["TOKEN"], json!("plain-own"));
}

/// A value a deployed Branch set and saved itself before any seal was never sealed
/// either: its review still shows it.
#[test]
fn a_deployed_branchs_own_saved_value_still_shows_after_its_parent_seals() {
    let (store, who) = shop();
    deployed_qa(&store, &who);
    set(&store, &who, "qa", &[("web.env.TOKEN", json!("plain-own"))]);
    let publish = Publish {
        id: None,
        environment: at("qa"),
        version: Some(diff(&store, &who, "qa").version),
        message: None,
        accept_volume_loss: Vec::new(),
    };
    store
        .write_trusted(&who, &publish, &Trusted::default())
        .unwrap();
    set(
        &store,
        &who,
        "qa",
        &[("web.env.TOKEN", json!("plain-edit"))],
    );
    seal_token(&store, &who, 3);

    let review = diff(&store, &who, "qa");
    let draft = review
        .draft_changes
        .iter()
        .flat_map(|change| &change.settings)
        .find(|setting| setting.path == "web.env.TOKEN")
        .unwrap();
    assert_eq!(
        (draft.before.clone(), draft.after.clone()),
        (json!("plain-own"), json!("plain-edit"))
    );
}

/// Discarding back to Saved State a deployed Branch's edit of a value its Parent
/// sealed since brings it back Not set.
#[test]
fn discarding_to_saved_a_value_its_parent_sealed_since_brings_it_back_not_set() {
    let (store, who) = shop();
    deployed_qa(&store, &who);
    set(
        &store,
        &who,
        "qa",
        &[("web.env.TOKEN", json!("plain-edit"))],
    );
    seal_token(&store, &who, 3);

    store
        .write(
            &who,
            &Discard {
                environment: at("qa"),
                target: Some(ployz_store::DiscardTarget::Saved),
                path: Some(SettingPath::parse("web.env.TOKEN").unwrap()),
                version: None,
            },
        )
        .unwrap();

    assert_eq!(
        web(&store, &who, "qa")["env"]["TOKEN"],
        json!({ "secret": false })
    );
}

/// A deployed Branch's in-place edit of a value nothing sealed still shows the
/// value it replaced.
#[test]
fn a_deployed_branchs_in_place_edit_of_a_plain_value_shows_the_value_it_replaced() {
    let (store, who) = shop();
    deployed_qa(&store, &who);
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

/// The mask is only shown: what a deploy plans and ships, and the versions a deploy
/// expects, are byte for byte what they were before it. The snapshot was recorded
/// without the mask.
#[test]
fn hiding_a_sealed_value_changes_no_plan_version_or_effect() {
    let (store, who) = shop();
    deployed_qa(&store, &who);
    set(
        &store,
        &who,
        "qa",
        &[("web.env.TOKEN", json!("plain-edit"))],
    );
    seal_token(&store, &who, 3);

    let review = diff(&store, &who, "qa");
    let plan = store
        .read(
            &who,
            &ployz_store::PlanQuery {
                environment: at("qa"),
                services: Vec::new(),
            },
        )
        .unwrap();
    let shipped = deploy(&store, &who, "qa", 23);
    let snapshot = json!({
        "review": {
            "version": review.version,
            "total_count": review.total_count,
            "draft_count": review.draft_count,
            "effects": review.effects,
        },
        "plan": {
            "version": plan.version,
            "namespace": plan.namespace,
            "unresolved": plan.unresolved,
        },
        "shipped": shipped,
    });
    let mut text = serde_json::to_string_pretty(&normalized(&snapshot)).unwrap();
    text.push('\n');
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots/sealed_mask_plan.json");
    if std::env::var_os("PLOYZ_BLESS").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap_or_default(),
        text,
        "rerun with PLOYZ_BLESS=1 and review {}",
        path.display()
    );
}

/// `value` with each ID a run mints by chance named by its first place in it.
fn normalized(value: &Value) -> Value {
    fn walk(value: &Value, seen: &mut Vec<String>) -> Value {
        match value {
            Value::String(text) => {
                let mut out = String::new();
                let mut rest = text.as_str();
                while let Some(at) = minted(rest) {
                    out.push_str(&rest[..at]);
                    let id = &rest[at..at + 36];
                    let n = seen.iter().position(|was| was == id).unwrap_or_else(|| {
                        seen.push(id.to_owned());
                        seen.len() - 1
                    });
                    out.push_str(&format!("<id-{n}>"));
                    rest = &rest[at + 36..];
                }
                out.push_str(rest);
                Value::String(out)
            }
            Value::Array(items) => {
                Value::Array(items.iter().map(|item| walk(item, seen)).collect())
            }
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), walk(value, seen)))
                    .collect(),
            ),
            other @ (Value::Null | Value::Bool(_) | Value::Number(_)) => other.clone(),
        }
    }
    // A UUID the test didn't fix.
    fn minted(text: &str) -> Option<usize> {
        let bytes = text.as_bytes();
        (0..bytes.len().saturating_sub(35)).find(|&at| {
            text.get(at..at + 36).is_some_and(|id| {
                id.char_indices().all(|(i, c)| match i {
                    8 | 13 | 18 | 23 => c == '-',
                    _ => c.is_ascii_hexdigit(),
                }) && !id.starts_with("00000000-0000-4000-8000-")
            })
        })
    }
    walk(value, &mut Vec::new())
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

/// production adds Service `name` as lineage `n`, `FOO=1`.
fn add_service(store: &ConfigStore, who: &Actor, n: u8, name: &str) {
    store
        .write(
            who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(n)).unwrap(),
                environment: at("production"),
                name: ServiceName::parse(name).unwrap(),
                image: Some("nginx:1.29-alpine".into()),
                template: None,
            },
        )
        .unwrap();
    set(
        store,
        who,
        "production",
        &[(&format!("{name}.env.FOO"), json!("1"))],
    );
}

/// production adds `alpha` as lineage `n` and `beta` as `n + 1`, which references it.
fn add_pair(store: &ConfigStore, who: &Actor, n: u8, (alpha, beta): (&str, &str)) {
    add_service(store, who, n, alpha);
    add_service(store, who, n + 1, beta);
    set(
        store,
        who,
        "production",
        &[(
            &format!("{beta}.env.A"),
            json!(format!("${{{{ {alpha}.FOO }}}}")),
        )],
    );
}

fn remove_services(store: &ConfigStore, who: &Actor, names: &[&str]) {
    for name in names {
        store
            .write(
                who,
                &ployz_store::RemoveService {
                    environment: at("production"),
                    service: ServiceName::parse(*name).unwrap(),
                },
            )
            .unwrap();
    }
}

/// Whether `environment`'s Working State holds Service `name`.
fn holds(store: &ConfigStore, who: &Actor, environment: &str, name: &str) -> bool {
    services(store, who, environment).contains(&name.to_owned())
}

/// #3733: the runner confirms a removal before the one referencing it, so the first
/// Follow keeps the referenced node; the next one, for the referrer, takes it too.
#[test]
fn a_node_confirmed_before_its_referrer_still_follows_out_of_the_branch() {
    let (store, who) = shop();
    add_pair(&store, &who, 20, ("alpha", "beta"));
    deploy(&store, &who, "production", 2);
    assert!(staged(&store, &who, "fix-web", "alpha"));
    remove_services(&store, &who, &["alpha", "beta"]);
    deploy_in_batches(&store, &who, "production", 3, &[&["alpha"], &["beta"]]);

    assert!(!holds(&store, &who, "fix-web", "alpha"));
    assert!(!holds(&store, &who, "fix-web", "beta"));
}

/// #3733 as the soak found it: a node the Branch references stays beside them.
#[test]
fn a_node_confirmed_before_its_referrer_follows_out_beside_one_the_branch_references() {
    let (store, who) = shop();
    add_service(&store, &who, 22, "refd");
    add_pair(&store, &who, 20, ("alpha", "beta"));
    deploy(&store, &who, "production", 2);
    set(
        &store,
        &who,
        "fix-web",
        &[("web.env.REF", json!("${{ refd.FOO }}"))],
    );
    remove_services(&store, &who, &["alpha", "beta", "refd"]);
    deploy_in_batches(
        &store,
        &who,
        "production",
        3,
        &[&["refd"], &["alpha"], &["beta"]],
    );

    assert!(!holds(&store, &who, "fix-web", "alpha"));
    assert!(!holds(&store, &who, "fix-web", "beta"));
    assert!(holds(&store, &who, "fix-web", "refd"));
}

/// Round g of the soak: three pairs and a referenced node, each confirmed alone.
#[test]
fn pairs_confirmed_one_by_one_all_follow_out_of_the_branch() {
    let (store, who) = shop();
    add_service(&store, &who, 30, "refd");
    for (n, pair) in [
        (20, ("alpha1", "beta1")),
        (22, ("alpha2", "beta2")),
        (24, ("alpha3", "beta3")),
    ] {
        add_pair(&store, &who, n, pair);
    }
    deploy(&store, &who, "production", 2);
    set(
        &store,
        &who,
        "fix-web",
        &[("web.env.REF", json!("${{ refd.FOO }}"))],
    );
    let all = [
        "refd", "alpha1", "alpha2", "beta1", "alpha3", "beta3", "beta2",
    ];
    remove_services(&store, &who, &all);
    let batches: Vec<&[&str]> = all.iter().map(std::slice::from_ref).collect();
    deploy_in_batches(&store, &who, "production", 3, &batches);

    for name in &all[1..] {
        assert!(!holds(&store, &who, "fix-web", name), "{name}");
    }
    assert!(holds(&store, &who, "fix-web", "refd"));
}

/// A node the Branch kept while it referenced it goes at the Parent's next deploy
/// once nothing references it.
#[test]
fn a_kept_node_the_branch_stops_referencing_follows_out_at_the_next_deploy() {
    let (store, who) = shop();
    add_service(&store, &who, 20, "alpha");
    deploy(&store, &who, "production", 2);
    set(
        &store,
        &who,
        "fix-web",
        &[("web.env.REF", json!("${{ alpha.FOO }}"))],
    );
    remove_services(&store, &who, &["alpha"]);
    deploy(&store, &who, "production", 3);
    assert!(holds(&store, &who, "fix-web", "alpha"));
    store
        .write(
            &who,
            &Edit {
                environment: at("fix-web"),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse("web.env.REF").unwrap(),
                }],
            },
        )
        .unwrap();
    set(&store, &who, "production", &[("db.env.OTHER", json!("1"))]);
    deploy(&store, &who, "production", 4);

    assert!(!holds(&store, &who, "fix-web", "alpha"));
}

/// The referrer confirmed first: the referenced node goes in the next Follow.
#[test]
fn a_referrer_confirmed_first_takes_what_it_references_out_too() {
    let (store, who) = shop();
    add_pair(&store, &who, 20, ("alpha", "beta"));
    deploy(&store, &who, "production", 2);
    remove_services(&store, &who, &["alpha", "beta"]);
    deploy_in_batches(&store, &who, "production", 3, &[&["beta"], &["alpha"]]);

    assert!(!holds(&store, &who, "fix-web", "alpha"));
    assert!(!holds(&store, &who, "fix-web", "beta"));
}

/// What the Branch made its own stays, however the runner confirms the removals.
#[test]
fn a_node_the_branch_made_its_own_stays_across_confirmed_batches() {
    type Own = fn(&ConfigStore, &Actor);
    let changed: Own = |store, who| set(store, who, "fix-web", &[("alpha.env.MINE", json!("1"))]);
    let saved: Own = |store, who| {
        let publish = Publish {
            id: None,
            environment: at("fix-web"),
            version: Some(diff(store, who, "fix-web").version),
            message: None,
            accept_volume_loss: Vec::new(),
        };
        store
            .write_trusted(who, &publish, &Trusted::default())
            .unwrap();
    };
    let deployed: Own = |store, who| {
        deploy(store, who, "fix-web", 10);
    };
    let referenced: Own = |store, who| {
        set(
            store,
            who,
            "fix-web",
            &[("web.env.REF", json!("${{ alpha.FOO }}"))],
        )
    };
    for (case, own) in [
        ("changed", changed),
        ("saved", saved),
        ("deployed", deployed),
        ("referenced", referenced),
    ] {
        let (store, who) = shop();
        add_pair(&store, &who, 20, ("alpha", "beta"));
        deploy(&store, &who, "production", 2);
        own(&store, &who);
        remove_services(&store, &who, &["alpha", "beta"]);
        deploy_in_batches(&store, &who, "production", 3, &[&["alpha"], &["beta"]]);
        assert!(holds(&store, &who, "fix-web", "alpha"), "{case}");
    }
}

/// A Branch being shut down gets nothing taken back: Follow skips it.
#[test]
fn a_branch_being_shut_down_keeps_what_followed() {
    let (store, who) = shop();
    add_pair(&store, &who, 20, ("alpha", "beta"));
    deploy(&store, &who, "production", 2);
    store
        .write_trusted(
            &who,
            &ployz_store::Admit::ShutDown(ployz_store::ShutDown {
                id: ployz_store::DeploymentId::parse(uuid(50)).unwrap(),
                environment: at("fix-web"),
                version: None,
                accept_volume_loss: Vec::new(),
            }),
            &Trusted::default(),
        )
        .unwrap();
    remove_services(&store, &who, &["alpha", "beta"]);
    deploy_in_batches(&store, &who, "production", 3, &[&["alpha"], &["beta"]]);

    assert!(holds(&store, &who, "fix-web", "alpha"));
    assert!(holds(&store, &who, "fix-web", "beta"));
}

/// A forgotten Cluster empties Applied State without the Parent removing anything:
/// what followed stays while the next deploy has yet to confirm it again.
#[test]
fn a_node_the_parent_still_holds_stays_after_its_cluster_is_forgotten() {
    let (store, who) = shop();
    add_service(&store, &who, 20, "alpha");
    deploy(&store, &who, "production", 2);
    assert!(staged(&store, &who, "fix-web", "alpha"));
    store
        .system(
            &who.organization,
            &ployz_store::SystemEvent::ClusterForgotten,
            &Trusted::default(),
        )
        .unwrap();
    set(&store, &who, "production", &[("web.env.PLAIN", json!("2"))]);
    let admitted = backend::admit(&store, &who, "production", 3);
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&admitted, &runner).unwrap();
    // The runner confirms web first: alpha is still on its way.
    backend::confirm(&store, &admitted, &claimed, &[&["web"], &["alpha"]], 1);
    assert!(holds(&store, &who, "fix-web", "alpha"));
}

/// Admit Deployment `n` of production, shipping again Deployment 1, from before any
/// node a test adds.
fn roll_back(store: &ConfigStore, who: &Actor, n: u8) -> ployz_store::DeploymentId {
    let id = |n: u8| {
        ployz_store::DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}"))
            .unwrap()
    };
    store
        .write_trusted(
            who,
            &ployz_store::Admit::Retry(ployz_store::Retry {
                id: id(n),
                deployment: id(1),
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
            &Trusted::default(),
        )
        .unwrap();
    id(n)
}

/// Redeploying an older revision removes what came after it, though the latest Saved
/// State still holds it: the removal follows as one saved does.
#[test]
fn a_node_a_rollback_removes_follows_out_of_the_branch() {
    let (store, who) = shop();
    add_service(&store, &who, 20, "alpha");
    deploy(&store, &who, "production", 2);
    assert!(staged(&store, &who, "fix-web", "alpha"));
    let rollback = roll_back(&store, &who, 3);
    backend::run(&store, &rollback);

    assert!(!holds(&store, &who, "fix-web", "alpha"));
}

/// A rollback confirming a removal before the one referencing it takes both out too.
#[test]
fn a_node_a_rollback_confirms_before_its_referrer_follows_out_of_the_branch() {
    let (store, who) = shop();
    add_pair(&store, &who, 20, ("alpha", "beta"));
    deploy(&store, &who, "production", 2);
    assert!(staged(&store, &who, "fix-web", "alpha"));
    let rollback = roll_back(&store, &who, 3);
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&rollback, &runner).unwrap();
    backend::finish_confirming(&store, &rollback, claimed, &[&["alpha"], &["beta"]]);

    assert!(!holds(&store, &who, "fix-web", "alpha"));
    assert!(!holds(&store, &who, "fix-web", "beta"));
}

/// `qa` from production, `qa-two` from `qa`, after production's plain `TOKEN` took
/// `plain-private` by Follow into `qa`: `qa`'s own Node Introduction holds
/// `plain-one`, so only what `qa` shares with production ties `qa-two`'s value to
/// production's seal (#3727). `qa` then deletes `TOKEN`, so the seal never lands in
/// it, and `qa-two`, deployed, changes `TOKEN` in place.
fn followed_then_branched(store: &ConfigStore, who: &Actor) {
    set(
        store,
        who,
        "production",
        &[("web.env.TOKEN", json!("plain-one"))],
    );
    deploy(store, who, "production", 2);
    branch(store, who, 10, "production", "qa");
    set(
        store,
        who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(store, who, "production", 3);
    assert_eq!(
        web(store, who, "qa")["env"]["TOKEN"],
        json!("plain-private")
    );
    deploy(store, who, "qa", 4);
    branch(store, who, 11, "qa", "qa-two");
    deploy(store, who, "qa-two", 5);
    unset(store, who, "qa", "web.env.TOKEN");
    set(
        store,
        who,
        "qa-two",
        &[("web.env.TOKEN", json!("plain-edit"))],
    );
}

/// A Branch of a Branch whose Parent took the value by Follow reviews its change
/// from the root's seal (#3727, case 1).
#[test]
fn a_branch_of_a_branch_whose_parent_took_a_value_by_follow_never_shows_it_once_sealed() {
    let (store, who) = shop();
    followed_then_branched(&store, &who);
    seal_token(&store, &who, 6);

    let review = diff(&store, &who, "qa-two");
    assert_eq!(
        token_row(&review),
        (json!({ "secret": true }), json!("plain-edit"))
    );
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
    discard(&store, &who, "qa-two", "web.env.TOKEN");
    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!({ "secret": false })
    );
}

/// Nor one whose variable reached its Parent after it was made (#3727, case 3).
#[test]
fn a_branch_of_a_branch_never_shows_a_value_that_arrived_after_it_was_made_once_sealed() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    branch(&store, &who, 10, "production", "qa");
    deploy(&store, &who, "qa", 2);
    branch(&store, &who, 11, "qa", "qa-two");
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 3);
    deploy(&store, &who, "qa", 4);
    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!("plain-private")
    );
    deploy(&store, &who, "qa-two", 5);
    unset(&store, &who, "qa", "web.env.TOKEN");
    set(
        &store,
        &who,
        "qa-two",
        &[("web.env.TOKEN", json!("plain-edit"))],
    );
    seal_token(&store, &who, 6);

    let review = diff(&store, &who, "qa-two");
    assert_eq!(
        token_row(&review),
        (json!({ "secret": true }), json!("plain-edit"))
    );
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
    // What arrived in `qa-two` from `qa` holds no copy either.
    let holding = backend::tables_holding(&url, "plain-private");
    assert!(
        !holding.contains(&"config_sync_arrival".to_owned()),
        "{holding:?}"
    );
}

/// `qa`, `qa-two` and `qa-three`, each a Branch of the one before, made while
/// production's `TOKEN` was `plain-private`; `qa` changes it itself, so
/// production's seal never lands there (#3739).
fn three_down(store: &ConfigStore, who: &Actor) {
    set(
        store,
        who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(store, who, "production", 2);
    branch(store, who, 10, "production", "qa");
    branch(store, who, 11, "qa", "qa-two");
    branch(store, who, 12, "qa-two", "qa-three");
    set(store, who, "qa", &[("web.env.TOKEN", json!("plain-edit"))]);
    set(
        store,
        who,
        "qa-two",
        &[("web.env.TOKEN", json!("plain-edit-two"))],
    );
}

/// A seal empties the value in what every Branch below shares and was made with,
/// however far down, not only in its own Branches' (#3739).
#[test]
fn a_seal_empties_the_value_in_what_every_branch_below_shares() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    three_down(&store, &who);
    seal_token(&store, &who, 3);

    // `qa-three` never changed the value it took from `qa-two`, so the seal past
    // `qa`'s change empties its Working State too (#3760).
    assert_eq!(
        backend::tables_holding(&url, "plain-private"),
        ["config_node_introduction"]
    );
}

/// A Branch's own value is no copy of the sealed one: it stays where it was, and
/// what the Branch below compares with it still holds it (#3739).
#[test]
fn a_seal_leaves_a_branchs_own_value_in_what_the_branches_below_share() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    three_down(&store, &who);
    deploy(&store, &who, "qa", 3);
    deploy(&store, &who, "qa-two", 4);
    // Rows holding `marker` in what any Branch was made with, shares and holds as
    // arrived.
    let held = |marker: &str| -> Vec<i64> {
        [
            ("config_environment_branch", "made_with"),
            ("config_sync_base", "base"),
            ("config_sync_arrival", "value"),
            ("config_sync_arrival", "prior"),
        ]
        .iter()
        .map(|(table, column)| {
            backend::count(
                &url,
                &format!("SELECT COUNT(*) FROM {table} WHERE {column} LIKE '%{marker}%'"),
            )
        })
        .collect()
    };
    let (edit, edit_two) = (held("plain-edit\""), held("plain-edit-two"));
    assert!(edit[2] > 0, "{edit:?}");
    seal_token(&store, &who, 5);

    assert_eq!(held("plain-edit\""), edit);
    assert_eq!(held("plain-edit-two"), edit_two);
    assert_eq!(
        token_row(&diff(&store, &who, "qa-three")).1,
        json!("plain-edit-two")
    );
}

/// What a value production deleted after sealing and added again plain is never
/// read as sealed, nor emptied below, however often production saves since.
#[test]
fn a_value_added_again_plain_after_a_seal_still_shows_and_stays_in_the_branches_below() {
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
    branch(&store, &who, 11, "qa", "qa-two");
    seal_token(&store, &who, 3);
    deploy(&store, &who, "qa", 4);
    unset(&store, &who, "production", "web.env.TOKEN");
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("public-new"))],
    );
    deploy(&store, &who, "production", 5);
    deploy(&store, &who, "qa", 6);
    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!("public-new")
    );
    deploy(&store, &who, "qa-two", 7);
    branch(&store, &who, 12, "qa-two", "qa-three");
    set(
        &store,
        &who,
        "qa-two",
        &[("web.env.TOKEN", json!("plain-edit"))],
    );
    set(&store, &who, "production", &[("web.env.PLAIN", json!("2"))]);
    deploy(&store, &who, "production", 8);

    assert_eq!(
        token_row(&diff(&store, &who, "qa-two")),
        (json!("public-new"), json!("plain-edit"))
    );
    let three = uuid(12);
    for sql in [
        format!(
            "SELECT COUNT(*) FROM config_environment_branch \
             WHERE environment_id = '{three}' AND made_with LIKE '%public-new%'"
        ),
        format!(
            "SELECT COUNT(*) FROM config_sync_base \
             WHERE (environment_id = '{three}' OR other_id = '{three}') \
             AND base LIKE '%public-new%'"
        ),
    ] {
        assert_eq!(backend::count(&url, &sql), 1, "{sql}");
    }
}

/// Every read a display, an agent or an MCP tool makes of `environment`, as JSON,
/// but its History: that keeps what a Branch saved until the seal reaches it.
fn every_read(store: &ConfigStore, who: &Actor, environment: &str, parent: &str) -> String {
    let reads = [
        serde_json::to_value(diff(store, who, environment)).unwrap(),
        serde_json::to_value(
            store
                .read(
                    who,
                    &EnvironmentQuery {
                        environment: at(environment),
                        path: None,
                        all: true,
                    },
                )
                .unwrap(),
        )
        .unwrap(),
        serde_json::to_value(
            store
                .read(
                    who,
                    &ServicesQuery {
                        environment: at(environment),
                    },
                )
                .unwrap(),
        )
        .unwrap(),
        web(store, who, environment),
        serde_json::to_value(
            store
                .read(
                    who,
                    &ployz_store::PlanQuery {
                        environment: at(environment),
                        services: Vec::new(),
                    },
                )
                .unwrap(),
        )
        .unwrap(),
        serde_json::to_value(
            store
                .read(
                    who,
                    &ployz_store::BranchQuery {
                        environment: at(environment),
                    },
                )
                .unwrap(),
        )
        .unwrap(),
        serde_json::to_value(
            store
                .read(
                    who,
                    &SyncQuery {
                        from: at(parent),
                        into: Some(at(environment)),
                    },
                )
                .unwrap(),
        )
        .unwrap(),
        serde_json::to_value(
            store
                .read(
                    who,
                    &SyncQuery {
                        from: at(environment),
                        into: Some(at(parent)),
                    },
                )
                .unwrap(),
        )
        .unwrap(),
    ];
    serde_json::to_string(&reads).unwrap()
}

/// Put back what Branch `n` was made with and shares as they were when this ran
/// [`keep_bases`]: storage written before a seal emptied values below its Branches.
fn restore_bases(url: &str, n: u8) {
    let branch = uuid(n);
    backend::execute(
        url,
        &format!(
            "UPDATE config_sync_base SET base = (SELECT k.base FROM kept_base k \
             WHERE k.environment_id = config_sync_base.environment_id \
             AND k.other_id = config_sync_base.other_id) \
             WHERE environment_id = '{branch}' OR other_id = '{branch}'"
        ),
    );
    backend::execute(
        url,
        &format!(
            "UPDATE config_environment_branch SET made_with = (SELECT k.made_with \
             FROM kept_made_with k WHERE k.environment_id = '{branch}') \
             WHERE environment_id = '{branch}'"
        ),
    );
    backend::execute(url, "DROP TABLE kept_base");
    backend::execute(url, "DROP TABLE kept_made_with");
}

fn keep_bases(url: &str) {
    backend::execute(
        url,
        "CREATE TABLE kept_base AS SELECT * FROM config_sync_base",
    );
    backend::execute(
        url,
        "CREATE TABLE kept_made_with AS SELECT * FROM config_environment_branch",
    );
}

/// Bases written before a seal emptied values below the Branches that share them
/// still hold the plain value: no read shows it.
#[test]
fn no_read_shows_a_value_a_base_below_still_holds_from_before_the_seal_emptied_it() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    followed_then_branched(&store, &who);
    keep_bases(&url);
    seal_token(&store, &who, 6);
    restore_bases(&url, 11);
    let holding = backend::tables_holding(&url, "plain-private");
    assert!(
        holding.contains(&"config_sync_base".to_owned())
            && holding.contains(&"config_environment_branch".to_owned()),
        "{holding:?}"
    );

    for (environment, parent) in [("qa-two", "qa"), ("qa", "production")] {
        let shown = every_read(&store, &who, environment, parent);
        assert!(!shown.contains("plain-private"), "{environment}: {shown}");
    }
    assert_eq!(
        token_row(&diff(&store, &who, "qa-two")),
        (json!({ "secret": true }), json!("plain-edit"))
    );
}

/// `qa-two`'s Sync base with `qa` and what it was made with, counted where they
/// hold `marker`.
fn below_holding(url: &str, marker: &str) -> i64 {
    holding_of(url, 11, marker)
}

/// Branch `n`'s Sync bases and what it was made with, counted where they hold
/// `marker`.
fn holding_of(url: &str, n: u8, marker: &str) -> i64 {
    let branch = uuid(n);
    backend::count(
        url,
        &format!(
            "SELECT COUNT(*) FROM config_sync_base \
             WHERE (environment_id = '{branch}' OR other_id = '{branch}') \
             AND base LIKE '%{marker}%'"
        ),
    ) + backend::count(
        url,
        &format!(
            "SELECT COUNT(*) FROM config_environment_branch \
             WHERE environment_id = '{branch}' AND made_with LIKE '%{marker}%'"
        ),
    )
}

/// production's seal empties the value in what a Branch of a Branch shares and was
/// made with, though the Branch between changed it and so never took the seal
/// (#3739, as reported).
#[test]
fn a_seal_empties_the_value_in_what_a_branch_of_a_branch_shares() {
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
    branch(&store, &who, 11, "qa", "qa-two");
    deploy(&store, &who, "qa-two", 4);
    set(
        &store,
        &who,
        "qa",
        &[("web.env.TOKEN", json!("plain-edit"))],
    );
    set(
        &store,
        &who,
        "qa-two",
        &[("web.env.TOKEN", json!("plain-edit-two"))],
    );
    assert_eq!(below_holding(&url, "plain-private"), 2);
    seal_token(&store, &who, 5);

    assert_eq!(below_holding(&url, "plain-private"), 0);
}

/// Saving the sealed variable again in production empties what a seal made before
/// left plain below its Branches.
#[test]
fn saving_a_seal_again_empties_the_value_a_base_below_kept_from_before() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    followed_then_branched(&store, &who);
    keep_bases(&url);
    seal_token(&store, &who, 6);
    restore_bases(&url, 11);
    assert_eq!(below_holding(&url, "plain-private"), 2);
    // A Save that leaves the sealed value as it was reads nothing below.
    set(&store, &who, "production", &[("web.env.PLAIN", json!("2"))]);
    deploy(&store, &who, "production", 7);
    assert_eq!(below_holding(&url, "plain-private"), 2);

    let sealed = token(&store, &who, "production", 2);
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!({ "secret": sealed }))],
    );
    deploy(&store, &who, "production", 8);
    assert_eq!(below_holding(&url, "plain-private"), 0);
}

/// A Branch of a Branch whose Parent took by Follow a template production rewrote
/// with a rename reviews its delete from production's seal (#3727, case 2).
#[test]
fn a_branch_of_a_branch_never_shows_a_template_a_rename_rewrote_once_sealed() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private-${{ PLAIN }}"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    store
        .write(
            &who,
            &Edit {
                environment: at("production"),
                expect: None,
                changes: vec![Change::Rename {
                    path: SettingPath::parse("web.env.PLAIN").unwrap(),
                    to: "PLAIN_TWO".to_owned(),
                    update_references: true,
                }],
            },
        )
        .unwrap();
    deploy(&store, &who, "production", 3);
    assert_eq!(
        web(&store, &who, "qa")["env"]["TOKEN"],
        json!("plain-private-${{ PLAIN_TWO }}")
    );
    deploy(&store, &who, "qa", 4);
    branch(&store, &who, 11, "qa", "qa-two");
    deploy(&store, &who, "qa-two", 5);
    unset(&store, &who, "qa", "web.env.TOKEN");
    unset(&store, &who, "qa-two", "web.env.TOKEN");
    seal_token(&store, &who, 6);

    let review = diff(&store, &who, "qa-two");
    assert_eq!(token_row(&review), (json!({ "secret": true }), Value::Null));
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
}

/// Removing the Service after sealing its variable keeps the seal for the Branches
/// below that share the value.
#[test]
fn a_branch_of_a_branch_never_shows_a_sealed_value_once_its_service_is_removed_above() {
    let (store, who) = shop();
    followed_then_branched(&store, &who);
    seal_token(&store, &who, 6);
    remove_services(&store, &who, &["web"]);

    let review = diff(&store, &who, "qa-two");
    assert_eq!(
        token_row(&review),
        (json!({ "secret": true }), json!("plain-edit"))
    );
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
}

/// A value production never held, though a Sync base production shares holds it,
/// stays in what a Branch below was made with and shares with its Parent.
#[test]
fn a_seal_leaves_a_value_only_a_sync_base_of_the_sealer_holds() {
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
    set(
        &store,
        &who,
        "qa",
        &[("web.env.TOKEN", json!("public-local"))],
    );
    branch(&store, &who, 11, "qa", "qa-two");
    set(&store, &who, "production", &[("web.env.PLAIN", json!("2"))]);
    let version = store
        .read(
            &who,
            &SyncQuery {
                from: at("production"),
                into: Some(at("qa-two")),
            },
        )
        .unwrap()
        .version;
    store
        .write(
            &who,
            &SyncChanges {
                from: at("production"),
                into: Some(at("qa-two")),
                close_after: false,
                version,
                picks: Some(vec![row("variables.PLAIN").into()]),
                skip: Vec::new(),
                values: BTreeMap::new(),
                id: None,
            },
        )
        .unwrap();
    let (qa, two) = (uuid(10), uuid(11));
    let held = || {
        backend::count(
            &url,
            &format!(
                "SELECT COUNT(*) FROM config_environment_branch \
                 WHERE environment_id = '{two}' AND made_with LIKE '%public-local%'"
            ),
        ) + backend::count(
            &url,
            &format!(
                "SELECT COUNT(*) FROM config_sync_base WHERE environment_id = '{qa}' \
                 AND other_id = '{two}' AND base LIKE '%public-local%'"
            ),
        )
    };
    assert_eq!(held(), 2);
    seal_token(&store, &who, 3);

    assert_eq!(held(), 2);
}

/// A Branch that took the seal by Follow before Ployz 0.3.15 left the value plain
/// in what the Branches below it share: saving the seal again in production
/// clears it, by the value that Branch shared, which production no longer holds.
#[test]
fn saving_a_seal_again_empties_what_a_branch_that_took_it_left_below() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-one"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(&store, &who, "production", 3);
    deploy(&store, &who, "qa", 4);
    branch(&store, &who, 11, "qa", "qa-two");
    branch(&store, &who, 12, "qa-two", "qa-three");
    set(
        &store,
        &who,
        "qa-two",
        &[("web.env.TOKEN", json!("plain-edit"))],
    );
    keep_bases(&url);
    seal_token(&store, &who, 5);
    assert_eq!(
        web(&store, &who, "qa")["env"]["TOKEN"],
        json!({ "secret": false })
    );
    restore_bases(&url, 12);
    assert_eq!(holding_of(&url, 12, "plain-private"), 2);

    let sealed = token(&store, &who, "production", 2);
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!({ "secret": sealed }))],
    );
    deploy(&store, &who, "production", 6);
    assert_eq!(holding_of(&url, 12, "plain-private"), 0);
}

/// Nor one whose Parent took the value by Follow and changed it itself since: what
/// the two share is still the value production sealed.
#[test]
fn a_branch_of_a_branch_whose_parent_changed_a_followed_value_never_shows_it_once_sealed() {
    let (store, who) = shop();
    followed_then_branched(&store, &who);
    set(&store, &who, "qa", &[("web.env.TOKEN", json!("plain-qa"))]);
    seal_token(&store, &who, 6);

    let review = diff(&store, &who, "qa-two");
    assert_eq!(
        token_row(&review),
        (json!({ "secret": true }), json!("plain-edit"))
    );
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private"), "{shown}");
}

/// production's `TOKEN=plain-private`, deployed down `qa` and `qa-two`; `qa` then
/// changes it with `change`, staged only, before production seals it (#3760).
fn sealed_past_a_change(store: &ConfigStore, who: &Actor, change: impl Fn(&ConfigStore, &Actor)) {
    set(
        store,
        who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(store, who, "production", 2);
    branch(store, who, 10, "production", "qa");
    deploy(store, who, "qa", 3);
    branch(store, who, 11, "qa", "qa-two");
    deploy(store, who, "qa-two", 4);
    change(store, who);
    seal_token(store, who, 5);
}

/// The seal stops as a hint in a Branch that deleted the value, yet reaches the
/// Branch below, which never touched it: it holds the variable Not set, staged, and
/// deploys without it (#3760).
#[test]
fn a_seal_reaches_a_branch_of_a_branch_whose_parent_deleted_the_value() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    sealed_past_a_change(&store, &who, |store, who| {
        unset(store, who, "qa", "web.env.TOKEN");
    });

    assert_eq!(hints(&store, &who, "qa").len(), 1);
    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!({ "secret": false })
    );
    let shown = every_read(&store, &who, "qa-two", "qa");
    assert!(!shown.contains("plain-private"), "{shown}");
    assert_eq!(deployed_env(&store, &who, "qa-two", 6).get("TOKEN"), None);
    // Its History keeps what it saved before the seal, as a Branch's does (#3756).
    let holding = backend::tables_holding(&url, "plain-private");
    assert!(
        !holding.contains(&"config_environment".to_owned()),
        "{holding:?}"
    );

    // What `qa` deploys since lands below as it would have.
    deploy(&store, &who, "qa", 7);
    assert_eq!(web(&store, &who, "qa-two")["env"].get("TOKEN"), None);
    assert!(hints(&store, &who, "qa-two").is_empty());
}

/// As a deletion, a value the Branch between set itself: the Branch below takes the
/// seal, then that value once the Branch between deploys it (#3760).
#[test]
fn a_seal_reaches_a_branch_of_a_branch_whose_parent_set_its_own_value() {
    let (store, who) = shop();
    sealed_past_a_change(&store, &who, |store, who| {
        set(store, who, "qa", &[("web.env.TOKEN", json!("plain-edit"))]);
    });

    // The Branch between keeps its own change; only a hint names the seal.
    assert_eq!(web(&store, &who, "qa")["env"]["TOKEN"], json!("plain-edit"));
    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!({ "secret": false })
    );
    let shown = every_read(&store, &who, "qa-two", "qa");
    assert!(!shown.contains("plain-private"), "{shown}");

    deploy(&store, &who, "qa", 6);
    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!("plain-edit")
    );
    assert!(hints(&store, &who, "qa-two").is_empty());
}

/// A Branch below that changed the variable itself keeps its own value: the seal
/// never overwrites it, staged or deployed (#3760).
#[test]
fn a_seal_past_a_change_leaves_a_value_the_branch_below_set_itself() {
    for deployed in [false, true] {
        let (store, who) = shop();
        sealed_past_a_change(&store, &who, |store, who| {
            set(
                store,
                who,
                "qa-two",
                &[("web.env.TOKEN", json!("plain-edit-two"))],
            );
            if deployed {
                deploy(store, who, "qa-two", 9);
            }
            unset(store, who, "qa", "web.env.TOKEN");
        });

        assert_eq!(
            web(&store, &who, "qa-two")["env"]["TOKEN"],
            json!("plain-edit-two"),
            "{deployed}"
        );
    }
}

/// Discarding the seal that reached a Branch of a Branch leaves it Not set, never
/// the plain value it held before (#3760).
#[test]
fn discarding_a_seal_that_reached_a_branch_of_a_branch_leaves_it_not_set() {
    let (store, who) = shop();
    sealed_past_a_change(&store, &who, |store, who| {
        unset(store, who, "qa", "web.env.TOKEN");
    });
    discard(&store, &who, "qa-two", "web.env.TOKEN");

    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!({ "secret": false })
    );
    let shown = every_read(&store, &who, "qa-two", "qa");
    assert!(!shown.contains("plain-private"), "{shown}");
}

/// A seal the Branch between takes reaches the Branch below at its own deploy, one
/// level per deploy as ever: nothing seals it before (#3760).
#[test]
fn a_seal_a_branch_takes_reaches_the_branch_below_only_at_its_deploy() {
    let (store, who) = shop();
    sealed_past_a_change(&store, &who, |_, _| {});

    assert_eq!(
        web(&store, &who, "qa")["env"]["TOKEN"],
        json!({ "secret": false })
    );
    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!("plain-private")
    );
}

/// A value a Branch below set itself and synced into its Parent is its own, though
/// it matches the sealed one and the base they share: the seal leaves it (#3760).
#[test]
fn a_seal_past_a_change_leaves_a_value_the_branch_below_set_and_synced_up() {
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
    deploy(&store, &who, "qa", 3);
    branch(&store, &who, 11, "qa", "qa-two");
    set(
        &store,
        &who,
        "qa-two",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    sync_token(&store, &who, "qa-two", "qa");
    set(
        &store,
        &who,
        "qa",
        &[("web.env.TOKEN", json!("plain-edit-two"))],
    );
    seal_token(&store, &who, 5);

    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!("plain-private")
    );
}

/// A shut-down Branch below keeps its Working State as Follow leaves it (#3760).
#[test]
fn a_seal_past_a_change_leaves_a_shut_down_branch_below() {
    let (store, who) = shop();
    sealed_past_a_change(&store, &who, |store, who| {
        store
            .write_trusted(
                who,
                &ployz_store::Admit::ShutDown(ployz_store::ShutDown {
                    id: ployz_store::DeploymentId::parse(uuid(50)).unwrap(),
                    environment: at("qa-two"),
                    version: None,
                    accept_volume_loss: Vec::new(),
                }),
                &Trusted::default(),
            )
            .unwrap();
        unset(store, who, "qa", "web.env.TOKEN");
    });

    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!("plain-private")
    );
}

/// Sync `web.env.TOKEN` from `from` into `into`.
fn sync_token(store: &ConfigStore, who: &Actor, from: &str, into: &str) {
    let view: SyncView = store
        .read(
            who,
            &SyncQuery {
                from: at(from),
                into: Some(at(into)),
            },
        )
        .unwrap();
    store
        .write(
            who,
            &SyncChanges {
                from: at(from),
                into: Some(at(into)),
                close_after: false,
                version: view.version,
                picks: Some(vec!["web.env.TOKEN".into()]),
                skip: Vec::new(),
                values: BTreeMap::new(),
                id: None,
            },
        )
        .unwrap();
}

/// A Branch below that marks the variable Never sync keeps it, as Follow does (#3760).
#[test]
fn a_seal_past_a_change_leaves_a_row_the_branch_below_marks_never_sync() {
    let (store, who) = shop();
    sealed_past_a_change(&store, &who, |store, who| {
        store
            .write(
                who,
                &NeverSync {
                    environment: at("qa-two"),
                    rows: vec![row("variables.TOKEN").into()],
                    off: false,
                },
            )
            .unwrap();
        unset(store, who, "qa", "web.env.TOKEN");
    });

    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!("plain-private")
    );
}

/// A Branch that marks the variable Never sync never takes the seal, so neither does
/// a Branch below it, nor below the Branch between when that one marks it (#3760).
#[test]
fn a_seal_past_a_change_stops_at_a_never_sync_mark_on_the_way() {
    for marker in ["qa", "qa-two"] {
        let (store, who) = shop();
        sealed_past_a_change(&store, &who, |store, who| {
            branch(store, who, 12, "qa-two", "qa-three");
            store
                .write(
                    who,
                    &NeverSync {
                        environment: at(marker),
                        rows: vec![row("variables.TOKEN").into()],
                        off: false,
                    },
                )
                .unwrap();
            set(store, who, "qa", &[("web.env.TOKEN", json!("plain-edit"))]);
        });

        let below = if marker == "qa" { "qa-two" } else { "qa-three" };
        assert_eq!(
            web(&store, &who, below)["env"]["TOKEN"],
            json!("plain-private"),
            "{marker}"
        );
    }
}

/// `environment`'s History, as JSON.
fn history_of(store: &ConfigStore, who: &Actor, environment: &str) -> String {
    serde_json::to_string(
        &store
            .read(
                who,
                &ployz_store::HistoryQuery {
                    environment: at(environment),
                    limit: None,
                },
            )
            .unwrap(),
    )
    .unwrap()
}

/// `qa`, a Branch of production, deployed `plain-private`, then changed it with
/// `change` so the seal production makes since stays back there (#3756).
fn qa_saved_before_a_seal(store: &ConfigStore, who: &Actor, change: impl Fn(&ConfigStore, &Actor)) {
    set(
        store,
        who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    deploy(store, who, "production", 2);
    branch(store, who, 10, "production", "qa");
    deploy(store, who, "qa", 3);
    change(store, who);
    seal_token(store, who, 4);
}

fn edit_qa(store: &ConfigStore, who: &Actor) {
    set(store, who, "qa", &[("web.env.TOKEN", json!("plain-edit"))]);
}

/// A Branch's History reads a value it was made with and an Environment above sealed
/// since as Sealed, as its review does (#3756).
#[test]
fn a_branchs_history_shows_a_value_its_parent_sealed_since_as_sealed() {
    let (store, who) = shop();
    qa_saved_before_a_seal(&store, &who, edit_qa);

    let history = history_of(&store, &who, "qa");
    assert!(!history.contains("plain-private"), "{history}");
}

/// Restoring that version, after the Branch deleted the variable, brings it back Not
/// set, never plain (#3756).
#[test]
fn restoring_a_branchs_version_from_before_a_seal_above_brings_it_back_not_set() {
    let (store, who) = shop();
    qa_saved_before_a_seal(&store, &who, |store, who| {
        unset(store, who, "qa", "web.env.TOKEN");
    });
    let revision = store
        .read(
            &who,
            &ployz_store::HistoryQuery {
                environment: at("qa"),
                limit: None,
            },
        )
        .unwrap()
        .revisions
        .first()
        .unwrap()
        .revision;
    let restore = ployz_store::HistoryPreviewQuery {
        environment: at("qa"),
        revision,
        action: ployz_store::HistoryAction::Restore,
        change: None,
    };
    let preview = store.read(&who, &restore).unwrap();
    assert!(
        !serde_json::to_string(&preview)
            .unwrap()
            .contains("plain-private"),
        "{preview:?}"
    );
    store
        .write(
            &who,
            &ployz_store::StageHistory {
                environment: at("qa"),
                revision,
                action: restore.action,
                change: None,
                version: preview.version,
                accept_overwrite: true,
            },
        )
        .unwrap();

    assert_eq!(
        web(&store, &who, "qa")["env"]["TOKEN"],
        json!({ "secret": false })
    );
}

/// A value the Branch saved itself before the seal above still shows in its History:
/// it wasn't the sealed one (#3756).
#[test]
fn a_branchs_history_still_shows_a_value_it_set_itself() {
    let (store, who) = shop();
    qa_saved_before_a_seal(&store, &who, edit_qa);
    deploy(&store, &who, "qa", 5);

    assert!(history_of(&store, &who, "qa").contains("plain-edit"));
}

/// Undoing one change between two values an Environment above sealed since still
/// finds that change, and brings back no plain value (#3756).
#[test]
fn undoing_a_change_between_two_values_sealed_above_finds_it() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-first"))],
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    deploy(&store, &who, "qa", 3);
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-second"))],
    );
    deploy(&store, &who, "production", 4);
    deploy(&store, &who, "qa", 5);
    assert!(history_of(&store, &who, "qa").contains("plain-second"));
    edit_qa(&store, &who);
    seal_token(&store, &who, 6);
    let history = history_of(&store, &who, "qa");
    assert!(
        !history.contains("plain-first") && !history.contains("plain-second"),
        "{history}"
    );
    let revision = store
        .read(
            &who,
            &ployz_store::HistoryQuery {
                environment: at("qa"),
                limit: None,
            },
        )
        .unwrap()
        .revisions
        .iter()
        .map(|revision| revision.revision)
        .max()
        .unwrap();

    let preview = store
        .read(
            &who,
            &ployz_store::HistoryPreviewQuery {
                environment: at("qa"),
                revision,
                action: ployz_store::HistoryAction::Undo,
                change: Some("web.env.TOKEN".into()),
            },
        )
        .unwrap();
    let shown = serde_json::to_string(&preview).unwrap();
    assert!(
        !shown.contains("plain-first") && !shown.contains("plain-second"),
        "{shown}"
    );
}

/// production's `TOKEN1..=n` plain, deployed down `qa` and `qa-two`; `qa` changes
/// each, staged, and production seals each, staged, not yet deployed (#3780).
fn many_sealed_past_a_change(store: &ConfigStore, who: &Actor, n: u8) -> Vec<String> {
    let keys: Vec<String> = (1..=n).map(|index| format!("TOKEN{index}")).collect();
    let paths: Vec<String> = keys.iter().map(|key| format!("web.env.{key}")).collect();
    let values = |make: &dyn Fn(&str) -> Value| -> Vec<(&str, Value)> {
        paths
            .iter()
            .zip(&keys)
            .map(|(path, key)| (path.as_str(), make(key)))
            .collect()
    };
    set(
        store,
        who,
        "production",
        &values(&|key| json!(format!("plain-{key}"))),
    );
    deploy(store, who, "production", 2);
    branch(store, who, 10, "production", "qa");
    deploy(store, who, "qa", 3);
    branch(store, who, 11, "qa", "qa-two");
    deploy(store, who, "qa-two", 4);
    set(
        store,
        who,
        "qa",
        &values(&|key| json!(format!("edit-{key}"))),
    );
    let web = web_id(store, who, "production");
    set(
        store,
        who,
        "production",
        &values(&|key| json!({ "secret": backend::client_sealed(&uuid(2), &web, key, 1) })),
    );
    keys
}

/// Each of `keys` as `qa-two` holds it.
fn qa_two_tokens(store: &ConfigStore, who: &Actor, keys: &[String]) -> Vec<Value> {
    let env = &web(store, who, "qa-two")["env"];
    keys.iter().map(|key| env[key].clone()).collect()
}

/// A walk below seals every row a Branch inherited, the runner confirming `web`
/// mid-run before the outcome (#3780).
#[test]
fn a_seal_of_many_values_reaches_the_branch_below_once_the_deploy_ends() {
    let (store, who) = shop();
    let keys = many_sealed_past_a_change(&store, &who, 4);

    deploy_in_batches(&store, &who, "production", 5, &[&["web"]]);

    assert_eq!(
        qa_two_tokens(&store, &who, &keys),
        vec![json!({ "secret": false }); keys.len()]
    );
}

/// A Deployment that never ends seals nothing below: confirming `web` mid-run runs
/// Follow without the walk. The next one that ends seals it (#3780).
#[test]
fn a_deploy_that_never_ends_leaves_the_branch_below_to_the_next_one() {
    let (store, who) = shop();
    let keys = many_sealed_past_a_change(&store, &who, 2);
    let id = backend::admit(&store, &who, "production", 5);
    let claimed = store
        .claim(&id, &ployz_store::RunnerId::parse("runner").unwrap())
        .unwrap();
    backend::confirm(&store, &id, &claimed, &[&["web"]], 1);

    assert_eq!(
        web(&store, &who, "qa")["env"]["TOKEN1"],
        json!("edit-TOKEN1")
    );
    assert_eq!(
        qa_two_tokens(&store, &who, &keys),
        vec![json!("plain-TOKEN1"), json!("plain-TOKEN2")]
    );

    deploy(&store, &who, "production", 6);
    assert_eq!(
        qa_two_tokens(&store, &who, &keys),
        vec![json!({ "secret": false }); keys.len()]
    );
}

/// A Branch below that another write holds costs the deploy one short wait, however
/// many rows it would seal and however many times the runner reports, then keeps
/// its values until a later deploy (#3780).
#[test]
fn a_branch_below_another_write_holds_costs_one_short_wait() {
    // SQLite writers never overlap.
    if std::env::var_os("PLOYZ_STORE_TEST_POSTGRES").is_none() {
        return;
    }
    let url = backend::fresh_url(&tempfile::tempdir().unwrap());
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    let keys = many_sealed_past_a_change(&store, &who, 4);
    let mut other = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let mut held = other.transaction().unwrap();
    held.execute(
        "UPDATE config_environment SET working_revision = working_revision WHERE id = $1",
        &[&uuid(11)],
    )
    .unwrap();

    let started = std::time::Instant::now();
    deploy_in_batches(&store, &who, "production", 5, &[&["web"]]);
    let waited = started.elapsed();
    held.commit().unwrap();

    // One 1-second wait, with room for a loaded runner; per row and per report it
    // was 4 × 10 s × 2, and any one 10-second wait fails this.
    assert!(waited < std::time::Duration::from_secs(9), "{waited:?}");
    assert_eq!(
        qa_two_tokens(&store, &who, &keys),
        keys.iter()
            .map(|key| json!(format!("plain-{key}")))
            .collect::<Vec<_>>()
    );
    deploy(&store, &who, "production", 6);
    assert_eq!(
        qa_two_tokens(&store, &who, &keys),
        vec![json!({ "secret": false }); keys.len()]
    );
}

/// production's TOKEN "plain-private-0317-first", deployed down `qa` and `qa-two`;
/// production then deploys "plain-private-0317-second", which both take by Follow
/// and deploy. `qa` changes it, staged, and production seals it, so the seal's walk
/// leaves `qa-two` Not set (#3794).
fn followed_then_sealed(store: &ConfigStore, who: &Actor) {
    let token = |value: &str| [("web.env.TOKEN", json!(value))];
    set(store, who, "production", &token("plain-private-0317-first"));
    deploy(store, who, "production", 2);
    branch(store, who, 10, "production", "qa");
    deploy(store, who, "qa", 3);
    branch(store, who, 11, "qa", "qa-two");
    deploy(store, who, "qa-two", 4);
    set(
        store,
        who,
        "production",
        &token("plain-private-0317-second"),
    );
    deploy(store, who, "production", 5);
    deploy(store, who, "qa", 6);
    deploy(store, who, "qa-two", 7);
    assert_eq!(
        web(store, who, "qa-two")["env"]["TOKEN"],
        json!("plain-private-0317-second")
    );
    set(store, who, "qa", &token("plain-edit-0317"));
    seal_token(store, who, 8);
    assert_eq!(
        web(store, who, "qa-two")["env"]["TOKEN"],
        json!({ "secret": false })
    );
}

/// A Branch of a Branch reads a value it took by Follow and an Environment above
/// sealed since as Sealed in History, and Undo of that Follow leaves it Not set
/// (#3794).
#[test]
fn a_followed_value_sealed_above_reads_sealed_in_a_branch_of_a_branchs_history() {
    let (store, who) = shop();
    followed_then_sealed(&store, &who);

    let history = history_of(&store, &who, "qa-two");
    assert!(!history.contains("plain-private-0317"), "{history}");
    let revisions = store
        .read(
            &who,
            &ployz_store::HistoryQuery {
                environment: at("qa-two"),
                limit: None,
            },
        )
        .unwrap()
        .revisions;
    // The Follow's version: the only one since the first, and it changed TOKEN.
    let [follow, _first] = revisions.as_slice() else {
        panic!("{revisions:?}")
    };
    let changed = serde_json::to_string(&follow.changes).unwrap();
    assert!(changed.contains("web.env.TOKEN"), "{changed}");
    let took = follow.revision;
    let undo = ployz_store::HistoryPreviewQuery {
        environment: at("qa-two"),
        revision: took,
        action: ployz_store::HistoryAction::Undo,
        change: None,
    };
    let preview = store.read(&who, &undo).unwrap();
    let shown = serde_json::to_string(&preview).unwrap();
    assert!(!shown.contains("plain-private-0317"), "{shown}");
    store
        .write(
            &who,
            &ployz_store::StageHistory {
                environment: at("qa-two"),
                revision: took,
                action: undo.action,
                change: None,
                version: preview.version,
                accept_overwrite: true,
            },
        )
        .unwrap();
    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!({ "secret": false })
    );
}

/// Nor does its diff hint the value back, and taking that row is refused as a hint
/// it doesn't have: its next deploy ships no plain value (#3794).
#[test]
fn a_followed_value_sealed_above_is_never_hinted_or_taken_back() {
    let (store, who) = shop();
    followed_then_sealed(&store, &who);

    let review = diff(&store, &who, "qa-two");
    assert!(hints(&store, &who, "qa-two").is_empty());
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-private-0317"), "{shown}");
    let refused = store
        .write(
            &who,
            &take("qa", "qa-two", &["variables.TOKEN"], review.version.clone()),
        )
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::NotFound);
    // Nor does taking every hint, which finds none.
    let all = Take {
        rows: None,
        ..take("qa", "qa-two", &[], review.version)
    };
    let refused = store.write(&who, &all).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    assert!(refused.message.contains("No value from qa"), "{refused:?}");
    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!({ "secret": false })
    );
    // What it deploys next is its Working State, which neither take touched.
    let shipped = deploy(&store, &who, "qa-two", 9);
    assert!(
        !shipped.to_string().contains("plain-private-0317"),
        "{shipped}"
    );
}

/// What the Branch between deploys of its own still reaches the Branch below as a
/// hint, once that one holds a value it set itself, and both stay readable (#3794).
#[test]
fn a_value_the_branch_between_deploys_itself_is_still_hinted_below() {
    let (store, who) = shop();
    followed_then_sealed(&store, &who);
    unset(&store, &who, "qa-two", "web.env.TOKEN");
    set(
        &store,
        &who,
        "qa-two",
        &[("web.env.TOKEN", json!("plain-own-0317"))],
    );
    deploy(&store, &who, "qa-two", 9);
    deploy(&store, &who, "qa", 10);

    assert!(history_of(&store, &who, "qa").contains("plain-edit-0317"));
    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!("plain-own-0317")
    );
    assert_eq!(hints(&store, &who, "qa-two").len(), 1);
    let review = diff(&store, &who, "qa-two");
    assert!(
        serde_json::to_string(&review)
            .unwrap()
            .contains("plain-edit-0317")
    );
    store
        .write(
            &who,
            &take("qa", "qa-two", &["variables.TOKEN"], review.version),
        )
        .unwrap();
    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!("plain-edit-0317")
    );
    deploy(&store, &who, "qa-two", 11);
    assert!(history_of(&store, &who, "qa-two").contains("plain-own-0317"));
}

/// An older value the Branch between still runs, once its base moved on staged, is
/// read as Sealed below once an Environment above seals the row and the Branch below
/// moves off it: what the two share still holds it (#3794).
#[test]
fn an_older_value_the_branch_between_still_runs_reads_sealed_below() {
    let (store, who) = shop();
    let token = |value: &str| [("web.env.TOKEN", json!(value))];
    set(&store, &who, "production", &token("plain-made-0317"));
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    branch(&store, &who, 11, "qa", "qa-two");
    set(&store, &who, "production", &token("plain-older-0317"));
    deploy(&store, &who, "production", 3);
    deploy(&store, &who, "qa", 4);
    deploy(&store, &who, "qa-two", 5);
    assert!(history_of(&store, &who, "qa-two").contains("plain-older-0317"));
    // qa takes the next value staged, and still runs the older one.
    set(&store, &who, "production", &token("plain-newer-0317"));
    deploy(&store, &who, "production", 6);
    assert_eq!(
        web(&store, &who, "qa")["env"]["TOKEN"],
        json!("plain-newer-0317")
    );
    seal_token(&store, &who, 7);
    // qa-two moves off it to a value of its own.
    set(&store, &who, "qa-two", &token("plain-own-0317"));
    deploy(&store, &who, "qa-two", 8);

    let history = history_of(&store, &who, "qa-two");
    assert!(!history.contains("plain-older-0317"), "{history}");
    assert!(hints(&store, &who, "qa-two").is_empty());
    let shown = serde_json::to_string(&diff(&store, &who, "qa-two")).unwrap();
    assert!(!shown.contains("plain-older-0317"), "{shown}");
}

/// A value a Branch of a Branch sets itself reads as Sealed in its History, and a
/// discard brings it back Not set, when it is text an Environment above held before
/// sealing that row, even by another Branch of its own: the mask matches the text,
/// not where it came from (#3800).
#[test]
fn a_value_an_environment_above_held_reads_sealed_even_set_below_by_hand() {
    let (store, who) = shop();
    let token = |value: &str| [("web.env.TOKEN", json!(value))];
    set(&store, &who, "production", &token("plain-made-0317"));
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    deploy(&store, &who, "qa", 3);
    branch(&store, &who, 11, "qa", "qa-two");
    deploy(&store, &who, "qa-two", 4);
    // qa keeps its own change, so only `sibling` takes the next value.
    set(&store, &who, "qa", &token("plain-qa-0317"));
    set(&store, &who, "production", &token("plain-sibling-0317"));
    deploy(&store, &who, "production", 5);
    branch(&store, &who, 12, "production", "sibling");
    seal_token(&store, &who, 6);
    unset(&store, &who, "qa-two", "web.env.TOKEN");
    set(&store, &who, "qa-two", &token("plain-sibling-0317"));
    deploy(&store, &who, "qa-two", 7);
    assert!(history_of(&store, &who, "qa-two").contains("plain-sibling-0317"));
    // Discarding a change from it brings the variable back Not set, not as it was.
    set(&store, &who, "qa-two", &token("plain-later-0317"));
    discard(&store, &who, "qa-two", "web.env.TOKEN");
    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!({ "secret": false })
    );
    // Once it moves off it, History reads the value it held as Sealed.
    unset(&store, &who, "qa-two", "web.env.TOKEN");
    set(&store, &who, "qa-two", &token("plain-later-0317"));
    deploy(&store, &who, "qa-two", 8);
    let history = history_of(&store, &who, "qa-two");
    assert!(!history.contains("plain-sibling-0317"), "{history}");
}

/// What the Branch between still runs, after it took a seal from above staged, is
/// never hinted below: it holds the row sealed now, so any plain text it runs is
/// from before the seal (#3794).
#[test]
fn what_the_branch_between_runs_after_taking_a_seal_staged_is_never_hinted_below() {
    let (store, who) = shop();
    let token = |value: &str| [("web.env.TOKEN", json!(value))];
    set(&store, &who, "production", &token("plain-made-0317"));
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    deploy(&store, &who, "qa", 3);
    branch(&store, &who, 11, "qa", "qa-two");
    deploy(&store, &who, "qa-two", 4);
    set(&store, &who, "qa-two", &token("plain-own-0317"));
    deploy(&store, &who, "qa-two", 5);
    // qa runs the next value; qa-two keeps its own, so it arrives there as a hint.
    set(&store, &who, "production", &token("plain-run-0317"));
    deploy(&store, &who, "production", 6);
    deploy(&store, &who, "qa", 7);
    // qa takes the one after staged, and still runs the one before.
    set(&store, &who, "production", &token("plain-staged-0317"));
    deploy(&store, &who, "production", 8);
    seal_token(&store, &who, 9);

    assert!(hints(&store, &who, "qa-two").is_empty());
    let review = diff(&store, &who, "qa-two");
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains("plain-run-0317"), "{shown}");
    let refused = store
        .write(
            &who,
            &take("qa", "qa-two", &["variables.TOKEN"], review.version),
        )
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::NotFound);
    assert_eq!(
        web(&store, &who, "qa-two")["env"]["TOKEN"],
        json!("plain-own-0317")
    );
}

/// Known, #3800: once the Branch between took the seal by a hint, after deleting the
/// variable, nothing it or one above still shares holds the value a Branch below
/// took by Follow. So that Branch's History shows it again, though its diff never
/// offers it, until it deploys next, which clears it.
#[test]
fn a_followed_value_shows_in_history_again_once_the_branch_between_takes_the_seal_until_the_next_deploy()
 {
    let (store, who) = shop();
    let token = |value: &str| [("web.env.TOKEN", json!(value))];
    set(
        &store,
        &who,
        "production",
        &token("plain-private-0317-first"),
    );
    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "qa");
    deploy(&store, &who, "qa", 3);
    branch(&store, &who, 11, "qa", "qa-two");
    deploy(&store, &who, "qa-two", 4);
    set(
        &store,
        &who,
        "production",
        &token("plain-private-0317-second"),
    );
    deploy(&store, &who, "production", 5);
    deploy(&store, &who, "qa", 6);
    deploy(&store, &who, "qa-two", 7);
    unset(&store, &who, "qa", "web.env.TOKEN");
    seal_token(&store, &who, 8);
    assert!(!history_of(&store, &who, "qa-two").contains("plain-private-0317"));

    let version = diff(&store, &who, "qa").version;
    store
        .write(
            &who,
            &take("production", "qa", &["variables.TOKEN"], version),
        )
        .unwrap();
    assert!(history_of(&store, &who, "qa-two").contains("plain-private-0317-second"));
    deploy(&store, &who, "qa", 9);
    let shown = serde_json::to_string(&diff(&store, &who, "qa-two")).unwrap();
    assert!(!shown.contains("plain-private-0317"), "{shown}");
    assert!(history_of(&store, &who, "qa-two").contains("plain-private-0317-second"));
    deploy(&store, &who, "qa-two", 10);
    let history = history_of(&store, &who, "qa-two");
    assert!(!history.contains("plain-private-0317"), "{history}");
}
