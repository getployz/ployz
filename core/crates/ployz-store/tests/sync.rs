#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Sync through the Store's interface only, on SQLite and on Postgres (see
//! `backend`): a Branch's changes into its Parent, picked by row, offered again
//! when left out or discarded, never deleting, and closing the Branch after.

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
