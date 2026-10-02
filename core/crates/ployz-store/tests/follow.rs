#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Follow through the Store's interface only, on SQLite and on Postgres (see
//! `backend`): what a Parent deploys arrives staged in each of its Branches, once,
//! tagged with where it came from; a Branch's own change wins and the Parent's
//! value is a Use hint, as is one the Branch discards; it cascades one level per
//! deploy; and a secret follows only into a Branch that never set its own.

use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::RowId;
use ployz_store::{
    Actor, Change, ConfigStore, CreateBranch, CreateProject, CreateService, DiffQuery, DiffView,
    Discard, Edit, EnvironmentId, EnvironmentName, EnvironmentRef, HintSource, OrganizationId,
    ProjectId, ProjectName, ServiceLineageId, ServiceQuery, ServicesQuery, SettingPath, Take,
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

/// production runs `web` (image `web:1`, `PLAIN=1`) and `db`; Branch `fix-web` has
/// its own `web` and uses `db` live.
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
        .map(|change| format!("{} {}", change.at.label(), change.from))
        .collect();
    incoming.sort();
    incoming
}

/// The Follow hints in `environment`, as `row = value from`.
fn hints(store: &ConfigStore, who: &Actor, environment: &str) -> Vec<String> {
    diff(store, who, environment)
        .follow_hints
        .iter()
        .map(|hint| format!("{} = {} from {}", hint.at.label(), hint.value, hint.from))
        .collect()
}

/// `web`'s row `at`, as `variables.KEY` or `source.image`.
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
                path: Some(SettingPath::parse(path).unwrap()),
                version: None,
            },
        )
        .unwrap();
}

/// The environment `web` deploys with in `environment`, deploying it as `n`.
fn deployed_env(store: &ConfigStore, who: &Actor, environment: &str, n: u8) -> Value {
    let input = deploy(store, who, environment, n);
    let id = store
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
        .to_string();
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
            ["web.env.NEW production", "web.image production"]
        );
        assert!(hints(&store, &who, branch).is_empty());
    }

    // Edited after it arrived, it is the Branch's own.
    set(&store, &who, "qa", &[("web.env.NEW", json!("2"))]);
    assert_eq!(incoming(&store, &who, "qa"), ["web.image production"]);

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
    let arrived: Vec<_> = view.incoming.iter().map(|change| &change.at.row).collect();
    let web = &view.changes[0];
    assert_eq!(web.row.at(), "node");
    let rows: Vec<(&str, Option<String>)> = web
        .settings
        .iter()
        .map(|row| (row.path.as_str(), row.row.as_ref().map(RowId::at)))
        .collect();
    assert_eq!(
        rows,
        [
            ("web.image", Some("source.image".to_owned())),
            ("web.healthcheck.path", Some("healthcheck".to_owned())),
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
        [r#"web.image = "web:2" from production"#]
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
                &["source.image"],
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
            &take("production", "fix-web", &["source.image"], "0:0:0".into()),
        )
        .unwrap_err();
    assert_eq!(stale.code, RpcErrorCode::Conflict);

    let version = diff(&store, &who, "fix-web").version;
    let took = store
        .write(
            &who,
            &take("production", "fix-web", &["source.image"], version),
        )
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

    discard(&store, &who, "fix-web", "web.image");
    assert_eq!(web(&store, &who, "fix-web")["image"], json!("web:1"));
    let hint = [r#"web.image = "web:2" from production"#];
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
fn a_discarded_secret_rotation_is_a_hint_and_the_next_one_still_follows() {
    let (store, who) = shop();
    let rotate = |secret: &str, n: u8| {
        set(
            &store,
            &who,
            "production",
            &[("web.env.TOKEN", json!({ "secret": secret }))],
        );
        deploy(&store, &who, "production", n);
    };
    rotate("prod-1", 2);
    deploy(&store, &who, "fix-web", 3);
    rotate("prod-2", 4);
    discard(&store, &who, "fix-web", "web.env.TOKEN");
    assert_eq!(
        hints(&store, &who, "fix-web"),
        [r#"web.env.TOKEN = {"secret":true} from production"#]
    );
    assert_eq!(
        deployed_env(&store, &who, "fix-web", 5)["TOKEN"],
        json!("prod-1")
    );
    rotate("prod-3", 6);
    assert_eq!(
        deployed_env(&store, &who, "fix-web", 7)["TOKEN"],
        json!("prod-3")
    );
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
    assert_eq!(incoming(&store, &who, "child"), ["web.image fix-web"]);
}

#[test]
fn a_secret_follows_into_a_branch_that_never_set_its_own_and_not_one_that_did() {
    let (store, who) = shop();
    branch(&store, &who, 10, "production", "qa");
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!({ "secret": "prod-1" }))],
    );
    deploy(&store, &who, "production", 2);
    set(
        &store,
        &who,
        "qa",
        &[("web.env.TOKEN", json!({ "secret": "qa-own" }))],
    );

    // production rotates it: fix-web follows, qa keeps its own and gets no hint.
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!({ "secret": "prod-2" }))],
    );
    deploy(&store, &who, "production", 3);
    assert!(hints(&store, &who, "qa").is_empty());
    assert_eq!(
        deployed_env(&store, &who, "fix-web", 4)["TOKEN"],
        json!("prod-2")
    );
    assert_eq!(
        deployed_env(&store, &who, "qa", 5)["TOKEN"],
        json!("qa-own")
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
    assert_eq!(incoming(&store, &who, "fix-web"), ["web.image production"]);
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
    assert_eq!(incoming(&store, &who, "fix-web"), ["web.image production"]);
    let hints = hints(&store, &who, "fix-web");
    assert!(
        hints.iter().any(|hint| hint.starts_with("api ")),
        "{hints:?}"
    );
}
