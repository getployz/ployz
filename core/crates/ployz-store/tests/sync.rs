#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Sync through the Store's interface only, on SQLite and on Postgres (see
//! `backend`): a Branch's changes into its Parent, picked by row, offered again
//! when left out or discarded, never deleting, never carrying a secret's value, and
//! closing the Branch after; and between any two Environments of a Project.

use ployz_core::{DeployOutcome, DeployPreview, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Admit, BranchQuery, Change, ConfigStore, CreateBranch, CreateProject, CreateService,
    Deploy, DeploymentId, Discard, Edit, EnvironmentId, EnvironmentName, EnvironmentRef,
    KeepBranch, OrganizationId, ProjectId, ProjectName, RemoveService, RunEvidence, RunnerId,
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

/// production runs `web` (image `web:1`, `PLAIN=1`) and `db`; Branch `fix-web` has
/// its own `web` and uses `db` live.
fn shop(deployed: bool) -> (ConfigStore, Actor) {
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
        service(&store, &who, "production", n, name, image);
    }
    set(
        &store,
        &who,
        "production",
        &[
            ("web.env.PLAIN", json!("1")),
            ("web.env.DB_URL", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}")),
        ],
    );
    if deployed {
        deploy(&store, &who, "production", 1);
    }
    store
        .write(
            &who,
            &CreateBranch {
                id: EnvironmentId::parse(uuid(9)).unwrap(),
                from: at("production"),
                name: EnvironmentName::parse("fix-web").unwrap(),
                copy: vec![ployz_store::NodeName::parse("web").unwrap()],
                live: Vec::new(),
                setup: Vec::new(),
                keep: false,
                fix: None,
            },
        )
        .unwrap();
    (store, who)
}

fn service(store: &ConfigStore, who: &Actor, environment: &str, n: u8, name: &str, image: &str) {
    store
        .write(
            who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(n)).unwrap(),
                environment: at(environment),
                name: ServiceName::parse(name).unwrap(),
                image: Some(image.into()),
                template: None,
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

fn values(store: &ConfigStore, who: &Actor, environment: &str, service: &str) -> Value {
    Value::Object(
        store
            .read(
                who,
                &ServiceQuery {
                    environment: at(environment),
                    service: ServiceName::parse(service).unwrap(),
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

/// The rows offered, by label, sorted.
fn labels(view: &SyncView) -> Vec<&str> {
    let mut labels: Vec<&str> = view.rows.iter().map(|row| row.label.as_str()).collect();
    labels.sort_unstable();
    labels
}

fn row<'view>(view: &'view SyncView, label: &str) -> &'view ployz_store::SyncRow {
    view.rows.iter().find(|row| row.label == label).unwrap()
}

fn sync(picks: Option<Vec<String>>, version: Option<String>) -> SyncChanges {
    SyncChanges {
        from: at("fix-web"),
        picks,
        version,
        ..SyncChanges::default()
    }
}

fn to_parent(store: &ConfigStore, who: &Actor) -> usize {
    store
        .read(
            who,
            &BranchQuery {
                environment: at("fix-web"),
            },
        )
        .unwrap()
        .to_parent
}

fn discard(store: &ConfigStore, who: &Actor, path: &str) {
    store
        .write(
            who,
            &Discard {
                environment: at("production"),
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

#[test]
fn a_branch_syncs_its_picked_changes_into_its_parent_and_leaves_the_rest_for_next_time() {
    let (store, who) = shop(false);
    assert!(view(&store, &who).rows.is_empty());
    let nothing = store.write(&who, &sync(None, None)).unwrap_err();
    assert_eq!(
        (nothing.code, nothing.message.as_str()),
        (RpcErrorCode::Conflict, "Nothing to sync into production")
    );
    assert_eq!(
        nothing.details["version"],
        json!(view(&store, &who).version)
    );

    set(
        &store,
        &who,
        "fix-web",
        &[("web.image", json!("web:2")), ("web.env.NEW", json!("1"))],
    );
    // production changed the image too since the two last shared.
    set(
        &store,
        &who,
        "production",
        &[("web.image", json!("web:hot"))],
    );
    let review = view(&store, &who);
    assert_eq!(review.into.name.as_str(), "production");
    assert_eq!(labels(&review), ["web.env.NEW", "web.image"]);
    let [new, image] = [row(&review, "web.env.NEW"), row(&review, "web.image")];
    assert_eq!(new.node.to_string(), "web");
    assert_eq!((new.ticked, new.changed, new.new), (true, false, true));
    assert_eq!((&new.from, &new.into), (&json!("1"), &Value::Null));
    assert_eq!(
        (image.ticked, image.changed, image.new),
        (true, true, false)
    );
    assert_eq!(
        (&image.from, &image.into),
        (&json!("web:2"), &json!("web:hot"))
    );
    // The Branch's count to its Parent is the rows ticked by default.
    assert_eq!(to_parent(&store, &who), 2);

    let stale = store
        .write(&who, &sync(None, Some("0:0".into())))
        .unwrap_err();
    assert_eq!(stale.code, RpcErrorCode::Conflict);
    assert_eq!(stale.details["version"], json!(review.version));
    let unknown = store
        .write(&who, &sync(Some(vec!["nope".into()]), None))
        .unwrap_err();
    assert_eq!(unknown.code, RpcErrorCode::NotFound);

    // Only the image: the variable is left out, so it is offered again.
    let image_key = row(&review, "web.image").key.clone();
    let synced = store
        .write(
            &who,
            &sync(Some(vec![image_key]), Some(review.version.clone())),
        )
        .unwrap();
    assert_eq!(synced.into.name.as_str(), "production");
    assert_eq!(
        synced
            .staged
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["web"]
    );
    assert!(!synced.closing);
    let web = values(&store, &who, "production", "web");
    assert_eq!(web["image"], json!("web:2"));
    assert_eq!(web["env"].get("NEW"), None);
    assert_eq!(labels(&view(&store, &who)), ["web.env.NEW"]);
    assert_eq!(to_parent(&store, &who), 1);

    // A repeat Sync offers only what changed since the last one.
    set(&store, &who, "fix-web", &[("web.env.OTHER", json!("2"))]);
    assert_eq!(
        labels(&view(&store, &who)),
        ["web.env.NEW", "web.env.OTHER"]
    );
    store.write(&who, &sync(None, None)).unwrap();
    let web = values(&store, &who, "production", "web");
    assert_eq!(
        (&web["env"]["NEW"], &web["env"]["OTHER"]),
        (&json!("1"), &json!("2"))
    );
    assert!(view(&store, &who).rows.is_empty());
    assert_eq!(to_parent(&store, &who), 0);

    // Sync never deletes: not db, which the Branch uses live, nor web once the
    // Branch removes its own.
    store
        .write(
            &who,
            &RemoveService {
                environment: at("fix-web"),
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap();
    assert!(view(&store, &who).rows.is_empty());
    let services: Vec<String> = store
        .read(
            &who,
            &ployz_store::ServicesQuery {
                environment: at("production"),
            },
        )
        .unwrap()
        .services
        .into_iter()
        .map(|listing| listing.service.name.to_string())
        .collect();
    assert_eq!(services, ["db", "web"]);
}

#[test]
fn a_synced_change_discarded_before_it_deploys_is_offered_again() {
    let (store, who) = shop(true);
    set(&store, &who, "fix-web", &[("web.image", json!("web:2"))]);
    service(&store, &who, "fix-web", 5, "api", "api:1");
    set(&store, &who, "fix-web", &[("api.env.MODE", json!("fast"))]);
    assert_eq!(
        labels(&view(&store, &who)),
        ["api", "api.env.MODE", "web.image"]
    );
    store.write(&who, &sync(None, None)).unwrap();
    assert!(view(&store, &who).rows.is_empty());

    discard(&store, &who, "web.image");
    let again = view(&store, &who);
    assert_eq!(labels(&again), ["web.image"]);
    assert!(!row(&again, "web.image").changed);
    discard(&store, &who, "api");
    let again = view(&store, &who);
    assert_eq!(labels(&again), ["api", "api.env.MODE", "web.image"]);
    assert!(row(&again, "api").new);

    // Once production deploys them, a Discard there no longer gives them back.
    store.write(&who, &sync(None, None)).unwrap();
    deploy(&store, &who, "production", 2);
    set(
        &store,
        &who,
        "production",
        &[("web.image", json!("web:hot"))],
    );
    discard(&store, &who, "web.image");
    assert_eq!(
        values(&store, &who, "production", "web")["image"],
        json!("web:2")
    );
    assert!(view(&store, &who).rows.is_empty());
    set(&store, &who, "fix-web", &[("web.image", json!("web:3"))]);
    assert!(!row(&view(&store, &who), "web.image").changed);
}

#[test]
fn a_sync_closes_a_branch_that_isnt_kept_when_asked() {
    let (store, who) = shop(false);
    set(&store, &who, "fix-web", &[("web.image", json!("web:2"))]);
    let keep = |kept| KeepBranch {
        environment: at("fix-web"),
        kept,
    };
    let closing = SyncChanges {
        close_after: true,
        ..sync(None, None)
    };
    store.write(&who, &keep(true)).unwrap();
    let refused = store.write(&who, &closing).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        refused.details["next"],
        json!("ployz env keep --off --project shop --env fix-web")
    );
    // Refused before anything landed.
    assert_eq!(
        values(&store, &who, "production", "web")["image"],
        json!("web:1")
    );

    store.write(&who, &keep(false)).unwrap();
    let synced = store.write(&who, &closing).unwrap();
    assert!(synced.closing);
    assert_eq!(
        values(&store, &who, "production", "web")["image"],
        json!("web:2")
    );
    // It never ran on the Servers, so it is gone at once.
    let gone = store
        .read(
            &who,
            &BranchQuery {
                environment: at("fix-web"),
            },
        )
        .unwrap_err();
    assert_eq!(gone.code, RpcErrorCode::NotFound);
}

/// What a Sync from `from` into `into` would stage.
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

/// Sync `from` into `into`, the rows labelled `picks` or else those ticked.
fn sync_into(store: &ConfigStore, who: &Actor, (from, into): (&str, &str), picks: Option<&[&str]>) {
    let view = offered(store, who, from, into);
    let picks = picks.map(|labels| {
        labels
            .iter()
            .map(|label| row(&view, label).key.clone())
            .collect()
    });
    store
        .write(
            who,
            &SyncChanges {
                from: at(from),
                into: Some(at(into)),
                picks,
                ..SyncChanges::default()
            },
        )
        .unwrap();
}

/// Each row offered by label, with whether it is ticked by default.
fn ticks(view: &SyncView) -> Vec<(&str, bool)> {
    let mut ticks: Vec<(&str, bool)> = view
        .rows
        .iter()
        .map(|row| (row.label.as_str(), row.ticked))
        .collect();
    ticks.sort_unstable();
    ticks
}

/// A Branch `name` of `from` with its own `web`.
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

#[test]
fn a_branch_syncs_skipping_a_level_and_sideways_ticking_only_its_own_changes() {
    // production → fix-web → fix-a and fix-b.
    let (store, who) = shop(false);
    branch(&store, &who, 10, "fix-web", "fix-a");
    branch(&store, &who, 11, "fix-web", "fix-b");
    set(&store, &who, "production", &[("web.env.ROOT", json!("1"))]);
    // A root has no Parent: its every row is its own.
    let root = offered(&store, &who, "production", "fix-web");
    assert_eq!(ticks(&root), [("web.env.ROOT", true)]);
    sync_into(&store, &who, ("production", "fix-web"), None);

    // Into its own Branch, fix-web ticks its own change and not what it got from
    // production.
    set(&store, &who, "fix-web", &[("web.env.MID", json!("1"))]);
    let down = offered(&store, &who, "fix-web", "fix-a");
    assert_eq!(
        ticks(&down),
        [("web.env.MID", true), ("web.env.ROOT", false)]
    );
    sync_into(&store, &who, ("fix-web", "fix-a"), None);
    let web = values(&store, &who, "fix-a", "web");
    assert_eq!(
        (&web["env"]["MID"], web["env"].get("ROOT")),
        (&json!("1"), None)
    );
    sync_into(&store, &who, ("fix-web", "fix-a"), Some(&["web.env.ROOT"]));
    assert_eq!(
        values(&store, &who, "fix-a", "web")["env"]["ROOT"],
        json!("1")
    );

    // Skipping a level: fix-a's first Sync into production compares over where
    // fix-a was made, so production's own later change isn't offered back.
    set(&store, &who, "fix-a", &[("web.image", json!("web:2"))]);
    set(
        &store,
        &who,
        "production",
        &[("web.env.PLAIN", json!("hot"))],
    );
    let skip = offered(&store, &who, "fix-a", "production");
    assert_eq!(ticks(&skip), [("web.env.MID", false), ("web.image", true)]);
    assert!(row(&skip, "web.env.MID").new);
    sync_into(&store, &who, ("fix-a", "production"), None);
    let web = values(&store, &who, "production", "web");
    assert_eq!(
        (&web["image"], &web["env"]["PLAIN"], web["env"].get("MID")),
        (&json!("web:2"), &json!("hot"), None)
    );
    // The pair shares a base now: the change left out is offered again, alone.
    assert_eq!(
        ticks(&offered(&store, &who, "fix-a", "production")),
        [("web.env.MID", false)]
    );

    // Sideways: fix-a into its sibling, over where fix-a was made.
    let sideways = offered(&store, &who, "fix-a", "fix-b");
    assert_eq!(
        ticks(&sideways),
        [
            ("web.env.MID", false),
            ("web.env.ROOT", false),
            ("web.image", true)
        ]
    );
    sync_into(&store, &who, ("fix-a", "fix-b"), None);
    let web = values(&store, &who, "fix-b", "web");
    assert_eq!(
        (&web["image"], web["env"].get("MID")),
        (&json!("web:2"), None)
    );

    // Into its own Parent, every row is ticked.
    assert_eq!(
        ticks(&offered(&store, &who, "fix-a", "fix-web")),
        [("web.image", true)]
    );
}

#[test]
fn roots_sync_into_a_branch_they_didnt_make_and_into_each_other() {
    let (store, who) = shop(false);
    branch(&store, &who, 10, "fix-web", "fix-a");
    set(&store, &who, "fix-a", &[("web.image", json!("web:2"))]);
    set(&store, &who, "production", &[("web.env.ROOT", json!("1"))]);
    // Into a Branch of its Branch: over where that Branch was made, so the
    // Branch's own image isn't offered back.
    let skip = offered(&store, &who, "production", "fix-a");
    assert_eq!(ticks(&skip), [("web.env.ROOT", true)]);
    sync_into(&store, &who, ("production", "fix-a"), None);
    let web = values(&store, &who, "fix-a", "web");
    assert_eq!(
        (&web["image"], &web["env"]["ROOT"]),
        (&json!("web:2"), &json!("1"))
    );

    // Two roots that never synced: over the receiver itself, so staging gets all
    // of production.
    store
        .write(
            &who,
            &ployz_store::CreateEnvironment {
                id: EnvironmentId::parse(uuid(20)).unwrap(),
                project: None,
                name: EnvironmentName::parse("staging").unwrap(),
            },
        )
        .unwrap();
    let first = offered(&store, &who, "production", "staging");
    assert_eq!(
        ticks(&first),
        [
            ("db", true),
            ("web", true),
            ("web.env.DB_URL", true),
            ("web.env.PLAIN", true),
            ("web.env.ROOT", true)
        ]
    );
    sync_into(&store, &who, ("production", "staging"), None);
    assert_eq!(
        values(&store, &who, "staging", "web")["image"],
        json!("web:1")
    );
    // Then only what changed since: not staging's own image.
    set(&store, &who, "staging", &[("web.image", json!("web:s"))]);
    set(&store, &who, "production", &[("web.env.PLAIN", json!("2"))]);
    assert_eq!(
        ticks(&offered(&store, &who, "production", "staging")),
        [("web.env.PLAIN", true)]
    );
    sync_into(&store, &who, ("production", "staging"), None);
    let web = values(&store, &who, "staging", "web");
    assert_eq!(
        (&web["image"], &web["env"]["PLAIN"]),
        (&json!("web:s"), &json!("2"))
    );
}

#[test]
fn a_sync_stays_within_its_project() {
    let (store, who) = shop(false);
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(30)).unwrap(),
                name: ProjectName::parse("blog").unwrap(),
                default_environment: EnvironmentId::parse(uuid(31)).unwrap(),
            },
        )
        .unwrap();
    let in_project = |project: &str, environment: &str| EnvironmentRef {
        project: Some(ProjectName::parse(project).unwrap()),
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    };
    let across = SyncQuery {
        from: in_project("shop", "fix-web"),
        into: Some(in_project("blog", "production")),
    };
    let refused = store.read(&who, &across).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        refused.details["next"],
        json!("ployz env sync --to ENV --project shop --env fix-web")
    );
    let refused = store
        .write(
            &who,
            &SyncChanges {
                from: across.from,
                into: across.into,
                ..SyncChanges::default()
            },
        )
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);

    // A root names where it syncs.
    let rootless = store
        .read(
            &who,
            &SyncQuery {
                from: in_project("shop", "production"),
                into: None,
            },
        )
        .unwrap_err();
    assert_eq!(
        rootless.details["next"],
        json!("ployz env sync --to ENV --project shop --env production")
    );
}

#[test]
fn a_secret_syncs_without_its_value_and_the_receiver_deploys_only_with_its_own() {
    let (store, who) = shop(false);
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!({ "secret": "prod-token" }))],
    );
    set(
        &store,
        &who,
        "fix-web",
        &[
            ("web.env.TOKEN", json!({ "secret": "test-token" })),
            ("web.env.API_KEY", json!({ "secret": "test-key" })),
        ],
    );
    service(&store, &who, "fix-web", 5, "api", "api:1");
    set(
        &store,
        &who,
        "fix-web",
        &[("api.env.KEY", json!({ "secret": "test-api-key" }))],
    );

    // production has TOKEN: it is never offered. The secrets it lacks are, flagged.
    let review = view(&store, &who);
    assert_eq!(labels(&review), ["api", "api.env.KEY", "web.env.API_KEY"]);
    for label in ["api.env.KEY", "web.env.API_KEY"] {
        let secret = row(&review, label);
        assert_eq!(
            (secret.secret, secret.new, secret.ticked),
            (true, true, true)
        );
        assert_eq!(secret.from, json!({ "secret": true }));
    }
    assert!(!row(&review, "api").secret);
    store.write(&who, &sync(None, None)).unwrap();
    assert!(view(&store, &who).rows.is_empty());
    assert_eq!(
        values(&store, &who, "production", "web")["env"]["API_KEY"],
        json!({ "secret": true })
    );

    // They landed without a value: Deploy refuses, naming each, until production has its own.
    let admit = |n: u8| {
        store.write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: DeploymentId::parse(format!("00000000-0000-4000-8000-0000000002{n:02}"))
                    .unwrap(),
                environment: at("production"),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &Trusted::default(),
        )
    };
    let refused = admit(1).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(
        refused.message,
        "production has secrets without a value: set api.env.KEY, web.env.API_KEY before deploying"
    );
    assert_eq!(
        refused.details["next"],
        json!("ployz set api.env.KEY --secret --project shop --env production")
    );
    set(
        &store,
        &who,
        "production",
        &[("api.env.KEY", json!({ "secret": "prod-api-key" }))],
    );
    assert_eq!(
        admit(2).unwrap_err().details["secrets"],
        json!(["web.env.API_KEY"])
    );
    set(
        &store,
        &who,
        "production",
        &[("web.env.API_KEY", json!({ "secret": "prod-key" }))],
    );
    let input = deploy(&store, &who, "production", 3);
    let env = |id: &str| {
        input["snapshots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|snapshot| snapshot["serviceId"] == id)
            .unwrap()["resolvedEnv"]
            .clone()
    };
    let web = env(&uuid(3));
    assert_eq!(
        (&web["TOKEN"], &web["API_KEY"]),
        (&json!("prod-token"), &json!("prod-key"))
    );
    let api_id = store
        .read(
            &who,
            &ployz_store::ServicesQuery {
                environment: at("production"),
            },
        )
        .unwrap()
        .services
        .into_iter()
        .find(|listing| listing.service.name.as_str() == "api")
        .unwrap()
        .service
        .id
        .to_string();
    assert_eq!(env(&api_id)["KEY"], json!("prod-api-key"));
}
