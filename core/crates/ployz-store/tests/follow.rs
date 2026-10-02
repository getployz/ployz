#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Follow through the Store's interface only, on SQLite and on Postgres (see
//! `backend`): what a Parent deploys arrives staged in each of its Branches, once,
//! tagged with where it came from; a Branch's own change wins and the Parent's
//! value is a Use hint, as is one the Branch discards; it cascades one level per
//! deploy; and a secret follows only into a Branch that never set its own.

use ployz_core::{DeployOutcome, DeployPreview, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Admit, Change, ConfigStore, CreateBranch, CreateProject, CreateService, Deploy,
    DeploymentId, DiffQuery, DiffView, Discard, Edit, EnvironmentId, EnvironmentName,
    EnvironmentRef, HintSource, OrganizationId, ProjectId, ProjectName, RunEvidence, RunnerId,
    ServiceLineageId, ServiceQuery, ServicesQuery, SettingPath, Take, Trusted,
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
        .map(|change| format!("{} {}", change.row, change.from))
        .collect();
    incoming.sort();
    incoming
}

/// The Follow hints in `environment`, as `row = value from`.
fn hints(store: &ConfigStore, who: &Actor, environment: &str) -> Vec<String> {
    diff(store, who, environment)
        .follow_hints
        .iter()
        .map(|hint| format!("{} = {} from {}", hint.row, hint.value, hint.from))
        .collect()
}

fn take(parent: &str, into: &str, rows: &[&str], version: Option<String>) -> Take {
    Take {
        from: HintSource::Parent(EnvironmentName::parse(parent).unwrap()),
        into: Some(at(into)),
        rows: Some(rows.iter().map(|row| (*row).to_owned()).collect()),
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

/// Deploy `environment` in full and record every Service applied; what the runner
/// was handed.
fn deploy(store: &ConfigStore, who: &Actor, environment: &str, n: u8) -> Value {
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
    claimed.input
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

    // Delivered once: deploying again stages nothing more.
    let before = diff(&store, &who, "fix-web").version;
    deploy(&store, &who, "production", 3);
    assert_eq!(diff(&store, &who, "fix-web").version, before);

    // Deployed in the Branch, it is the Branch's own: no longer incoming.
    deploy(&store, &who, "fix-web", 4);
    assert!(incoming(&store, &who, "fix-web").is_empty());
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
        .write(&who, &take("fix-web", "fix-web", &["web.image"], None))
        .unwrap_err();
    assert_eq!(wrong.code, RpcErrorCode::InvalidArgument);
    let unknown = store
        .write(&who, &take("production", "fix-web", &["web.env"], None))
        .unwrap_err();
    assert_eq!(unknown.code, RpcErrorCode::NotFound);
    let stale = store
        .write(
            &who,
            &take(
                "production",
                "fix-web",
                &["web.image"],
                Some("0:0:0".into()),
            ),
        )
        .unwrap_err();
    assert_eq!(stale.code, RpcErrorCode::Conflict);

    let version = diff(&store, &who, "fix-web").version;
    let took = store
        .write(
            &who,
            &take("production", "fix-web", &["web.image"], Some(version)),
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
