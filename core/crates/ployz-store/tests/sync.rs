#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Sync through the Store's interface only, on SQLite and on Postgres (see
//! `backend`): a Branch's changes into its Parent, picked by row or by name, offered
//! again when left out, discarded or undone, never deleting, a secret the receiver
//! lacks arriving with the value given or without one, and closing the Branch after;
//! and between any two Environments of a Project.

use std::collections::BTreeMap;

use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Admit, BranchQuery, Change, ConfigStore, CreateBranch, CreateProject, CreateService,
    Deploy, DeploymentId, Discard, Edit, EnvironmentId, EnvironmentName, EnvironmentRef,
    KeepBranch, OrganizationId, ProjectId, ProjectName, RemoveService, RenameService, SecretRow,
    ServiceLineageId, ServiceQuery, SettingPath, SyncChange, SyncChanges, SyncId, SyncQuery,
    SyncRow, SyncView, Synced, SyncedWhen, Trusted, UndoSync, When,
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
    branch(&store, &who, 9, "production", "fix-web");
    (store, who)
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

fn services(store: &ConfigStore, who: &Actor, environment: &str) -> Vec<String> {
    store
        .read(
            who,
            &ployz_store::ServicesQuery {
                environment: at(environment),
            },
        )
        .unwrap()
        .services
        .into_iter()
        .map(|listing| listing.service.name.to_string())
        .collect()
}

/// What a Sync from `from` into `into` would stage.
fn offered(store: &ConfigStore, who: &Actor, from: &str, into: &str) -> SyncView {
    store
        .read(
            who,
            &SyncQuery {
                from: at(from),
                into: Some(at(into)),
                when: None,
            },
        )
        .unwrap()
}

/// What a Sync from fix-web into its Parent would stage.
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

/// The rows offered, by label, sorted.
fn labels(view: &SyncView) -> Vec<String> {
    let mut labels: Vec<String> = view.rows.iter().map(|row| row.at.to_string()).collect();
    labels.sort_unstable();
    labels
}

/// The rows offered, by label, sorted; one not ticked by default reads `-label`.
fn ticks(view: &SyncView) -> Vec<String> {
    let mut ticks: Vec<(String, bool)> = view
        .rows
        .iter()
        .map(|row| (row.at.to_string(), row.ticked))
        .collect();
    ticks.sort_unstable();
    ticks
        .into_iter()
        .map(|(label, ticked)| if ticked { label } else { format!("-{label}") })
        .collect()
}

fn row<'view>(view: &'view SyncView, label: &str) -> &'view SyncRow {
    view.rows
        .iter()
        .find(|row| row.at.to_string() == label)
        .unwrap()
}

/// Sync what `view` offers now, at its version: `picks` by name, or else the rows
/// ticked.
fn sync(view: &SyncView, picks: Option<&[&str]>) -> SyncChanges {
    SyncChanges {
        from: at(view.from.name.as_str()),
        into: Some(at(view.into.name.as_str())),
        when: None,
        version: view.version.clone(),
        picks: picks.map(|picks| picks.iter().map(|label| (*label).into()).collect()),
        values: BTreeMap::new(),
    }
}

/// Sync `from` into `into`, the rows named `picks` or else those ticked.
fn sync_into(
    store: &ConfigStore,
    who: &Actor,
    (from, into): (&str, &str),
    picks: Option<&[&str]>,
) -> Synced {
    let view = offered(store, who, from, into);
    store.write(who, &sync(&view, picks)).unwrap()
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

fn undo(
    store: &ConfigStore,
    who: &Actor,
    synced: &Synced,
) -> Result<String, (RpcErrorCode, String)> {
    store
        .write(
            who,
            &UndoSync {
                sync: synced.sync.clone(),
            },
        )
        .map(|undone| undone.into.name.to_string())
        .map_err(|error| (error.code, error.message))
}

#[test]
fn a_branch_syncs_its_picked_changes_into_its_parent_and_leaves_the_rest_for_next_time() {
    let (store, who) = shop(false);
    let empty = view(&store, &who);
    assert!(empty.rows.is_empty());
    let nothing = store.write(&who, &sync(&empty, None)).unwrap_err();
    assert_eq!(
        (nothing.code, nothing.message.as_str()),
        (RpcErrorCode::Conflict, "Nothing to sync into production")
    );
    assert_eq!(nothing.details["version"], json!(empty.version));

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
    assert_eq!(new.at.node.to_string(), "web");
    assert_eq!((new.ticked, new.change), (true, SyncChange::New));
    assert_eq!((&new.from, &new.into), (&json!("1"), &Value::Null));
    assert_eq!((image.ticked, image.change), (true, SyncChange::Conflict));
    assert_eq!(
        (&image.from, &image.into),
        (&json!("web:2"), &json!("web:hot"))
    );
    // The Branch's count to its Parent is the rows ticked by default.
    assert_eq!(to_parent(&store, &who), 2);

    // A stale version is refused with the fresh one.
    let stale = SyncChanges {
        version: "0:0".into(),
        ..sync(&review, None)
    };
    let stale = store.write(&who, &stale).unwrap_err();
    assert_eq!(stale.code, RpcErrorCode::Conflict);
    assert_eq!(stale.details["version"], json!(review.version));
    // A row not offered is refused with the rows there are; no picks, nothing to do.
    let nope = new.at.row.to_string().replace("NEW", "NOPE");
    let unknown = SyncChanges {
        picks: Some(vec![serde_json::from_value(json!(nope)).unwrap()]),
        ..sync(&review, None)
    };
    let unknown = store.write(&who, &unknown).unwrap_err();
    assert_eq!(unknown.code, RpcErrorCode::NotFound);
    assert_eq!(
        unknown.details["valid_children"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        store
            .write(&who, &sync(&review, Some(&[])))
            .unwrap_err()
            .code,
        RpcErrorCode::Conflict
    );

    // Only the image, by name: the variable is left out, so it is offered again.
    let synced = store
        .write(&who, &sync(&review, Some(&["web.image"])))
        .unwrap();
    assert_eq!(synced.into.name.as_str(), "production");
    let SyncedWhen::Now { staged, closing } = &synced.when else {
        panic!("a Sync between Branches stages now")
    };
    assert_eq!(
        staged.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["web"]
    );
    assert!(!closing);
    // What a Sync brings is the receiver's own change, not one from a deploy.
    let diff = store
        .read(
            &who,
            &ployz_store::DiffQuery {
                environment: at("production"),
            },
        )
        .unwrap();
    assert!(diff.incoming.is_empty(), "{:?}", diff.incoming);
    let web = values(&store, &who, "production", "web");
    assert_eq!(web["image"], json!("web:2"));
    assert_eq!(web["env"].get("NEW"), None);
    assert_eq!(labels(&view(&store, &who)), ["web.env.NEW"]);
    assert_eq!(to_parent(&store, &who), 1);

    // A repeat Sync offers only what changed since the last one; one skipped stays.
    set(&store, &who, "fix-web", &[("web.env.OTHER", json!("2"))]);
    let review = view(&store, &who);
    assert_eq!(labels(&review), ["web.env.NEW", "web.env.OTHER"]);
    store
        .write(&who, &sync(&review, Some(&["web.env.NEW"])))
        .unwrap();
    assert_eq!(labels(&view(&store, &who)), ["web.env.OTHER"]);
    store.write(&who, &sync(&view(&store, &who), None)).unwrap();
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
    assert_eq!(services(&store, &who, "production"), ["db", "web"]);
}

#[test]
fn a_row_lands_in_what_the_receiver_calls_its_node() {
    let (store, who) = shop(false);
    // production renames web to site; fix-web makes a site of its own:
    // `site.env.X` names web's X in production and the new site's in fix-web.
    store
        .write(
            &who,
            &RenameService {
                environment: at("production"),
                service: ServiceName::parse("web").unwrap(),
                name: ServiceName::parse("site").unwrap(),
            },
        )
        .unwrap();
    service(&store, &who, "fix-web", 5, "site", "site:1");
    set(
        &store,
        &who,
        "fix-web",
        &[("web.env.X", json!("web")), ("site.env.X", json!("site"))],
    );
    let review = view(&store, &who);
    // Picked by its row, only web's X lands, in what production calls site.
    store
        .write(&who, &sync(&review, Some(&["web.env.X"])))
        .unwrap();
    assert_eq!(
        values(&store, &who, "production", "site")["env"]["X"],
        json!("web")
    );
    assert_eq!(services(&store, &who, "production"), ["db", "site"]);
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
    store.write(&who, &sync(&view(&store, &who), None)).unwrap();
    assert!(view(&store, &who).rows.is_empty());

    discard(&store, &who, "web.image");
    let again = view(&store, &who);
    assert_eq!(labels(&again), ["web.image"]);
    assert_eq!(row(&again, "web.image").change, SyncChange::Changed);
    discard(&store, &who, "api");
    let again = view(&store, &who);
    assert_eq!(labels(&again), ["api", "api.env.MODE", "web.image"]);
    assert_eq!(row(&again, "api").change, SyncChange::New);

    // Once production deploys them, a Discard there no longer gives them back.
    store.write(&who, &sync(&again, None)).unwrap();
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
    assert_eq!(
        row(&view(&store, &who), "web.image").change,
        SyncChange::Changed
    );
}

#[test]
fn a_synced_setting_production_lacked_is_offered_again_once_discarded() {
    let (store, who) = shop(true);
    set(
        &store,
        &who,
        "fix-web",
        &[
            ("web.startCommand", json!("serve")),
            ("web.env.NEW", json!("1")),
        ],
    );
    store.write(&who, &sync(&view(&store, &who), None)).unwrap();
    assert!(view(&store, &who).rows.is_empty());

    discard(&store, &who, "web.startCommand");
    discard(&store, &who, "web.env.NEW");
    let again = view(&store, &who);
    assert_eq!(labels(&again), ["web.env.NEW", "web.startCommand"]);
    assert_eq!(row(&again, "web.startCommand").change, SyncChange::New);
}

#[test]
fn undoing_a_sync_puts_back_only_what_it_changed_and_offers_it_again() {
    let (store, who) = shop(false);
    set(
        &store,
        &who,
        "fix-web",
        &[("web.image", json!("web:2")), ("web.env.NEW", json!("1"))],
    );
    service(&store, &who, "fix-web", 5, "api", "api:1");
    set(
        &store,
        &who,
        "production",
        &[("web.image", json!("web:hot"))],
    );
    let review = view(&store, &who);
    let synced = store.write(&who, &sync(&review, None)).unwrap();
    assert_eq!(services(&store, &who, "production"), ["api", "db", "web"]);
    // production's own change since stays.
    set(
        &store,
        &who,
        "production",
        &[("web.env.PLAIN", json!("own"))],
    );

    assert_eq!(undo(&store, &who, &synced), Ok("production".into()));
    let web = values(&store, &who, "production", "web");
    assert_eq!(
        (&web["image"], web["env"].get("NEW"), &web["env"]["PLAIN"]),
        (&json!("web:hot"), None, &json!("own"))
    );
    assert_eq!(services(&store, &who, "production"), ["db", "web"]);
    assert_eq!(labels(&view(&store, &who)), labels(&review));

    // Undone once, it is gone; so is a Sync that never was.
    let gone = Err((
        RpcErrorCode::NotFound,
        format!("No Sync {} to undo", synced.sync),
    ));
    assert_eq!(undo(&store, &who, &synced), gone);
    let never = Synced {
        sync: SyncId::parse(uuid(99)).unwrap(),
        ..synced
    };
    assert_eq!(
        undo(&store, &who, &never).unwrap_err().0,
        RpcErrorCode::NotFound
    );
}

#[test]
fn a_sync_is_not_undone_once_a_row_it_landed_changed_or_deployed() {
    let (store, who) = shop(false);
    set(
        &store,
        &who,
        "fix-web",
        &[("web.image", json!("web:2")), ("web.env.NEW", json!("1"))],
    );
    let new = sync_into(
        &store,
        &who,
        ("fix-web", "production"),
        Some(&["web.env.NEW"]),
    );
    set(&store, &who, "production", &[("web.env.NEW", json!("2"))]);
    assert_eq!(
        undo(&store, &who, &new),
        Err((
            RpcErrorCode::Conflict,
            "web.env.NEW changed since it synced: change it back instead".into()
        ))
    );

    let image = sync_into(&store, &who, ("fix-web", "production"), None);
    deploy(&store, &who, "production", 1);
    assert_eq!(
        undo(&store, &who, &image),
        Err((
            RpcErrorCode::Conflict,
            "web.image is deployed: change it back instead".into()
        ))
    );
    assert_eq!(
        values(&store, &who, "production", "web")["image"],
        json!("web:2")
    );
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
        when: Some(When::Now { close_after: true }),
        ..sync(&view(&store, &who), None)
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
    // Only a Sync into its Parent closes it.
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
    let sideways = SyncChanges {
        into: Some(at("staging")),
        ..closing.clone()
    };
    assert_eq!(
        store.write(&who, &sideways).unwrap_err().code,
        RpcErrorCode::InvalidArgument
    );
    let closing = SyncChanges {
        version: view(&store, &who).version,
        ..closing
    };
    let synced = store.write(&who, &closing).unwrap();
    assert!(matches!(synced.when, SyncedWhen::Now { closing: true, .. }));
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

#[test]
fn a_branch_syncs_skipping_a_level_and_sideways_ticking_only_its_own_changes() {
    // production → fix-web → fix-a and fix-b.
    let (store, who) = shop(false);
    branch(&store, &who, 10, "fix-web", "fix-a");
    branch(&store, &who, 11, "fix-web", "fix-b");
    set(&store, &who, "production", &[("web.env.ROOT", json!("1"))]);
    // A root has no Parent: its every row is its own.
    let root = offered(&store, &who, "production", "fix-web");
    assert_eq!(ticks(&root), ["web.env.ROOT"]);
    sync_into(&store, &who, ("production", "fix-web"), None);

    // Into its own Branch, fix-web ticks its own change and not what it got from
    // production.
    set(&store, &who, "fix-web", &[("web.env.MID", json!("1"))]);
    let down = offered(&store, &who, "fix-web", "fix-a");
    assert_eq!(ticks(&down), ["web.env.MID", "-web.env.ROOT"]);
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
    assert_eq!(ticks(&skip), ["-web.env.MID", "web.image"]);
    assert_eq!(row(&skip, "web.env.MID").change, SyncChange::New);
    sync_into(&store, &who, ("fix-a", "production"), None);
    let web = values(&store, &who, "production", "web");
    assert_eq!(
        (&web["image"], &web["env"]["PLAIN"], web["env"].get("MID")),
        (&json!("web:2"), &json!("hot"), None)
    );
    // The pair shares a base now: the change left out is offered again, alone.
    assert_eq!(
        ticks(&offered(&store, &who, "fix-a", "production")),
        ["-web.env.MID"]
    );

    // Sideways: fix-a into its sibling, over where fix-a was made.
    let sideways = offered(&store, &who, "fix-a", "fix-b");
    assert_eq!(
        ticks(&sideways),
        ["-web.env.MID", "-web.env.ROOT", "web.image"]
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
        ["web.image"]
    );

    // Once synced into its Parent, a change is no longer fix-a's own: unticked
    // sideways, a Sync reviewed while it was ticked is stale.
    set(&store, &who, "fix-a", &[("web.env.SIDE", json!("1"))]);
    let before = offered(&store, &who, "fix-a", "fix-b");
    assert!(row(&before, "web.env.SIDE").ticked);
    sync_into(&store, &who, ("fix-a", "fix-web"), Some(&["web.env.SIDE"]));
    let after = offered(&store, &who, "fix-a", "fix-b");
    assert!(!row(&after, "web.env.SIDE").ticked);
    assert_eq!(
        store.write(&who, &sync(&before, None)).unwrap_err().code,
        RpcErrorCode::Conflict
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
    assert_eq!(ticks(&skip), ["web.env.ROOT"]);
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
            "db",
            "web",
            "web.env.DB_URL",
            "web.env.PLAIN",
            "web.env.ROOT"
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
        ["web.env.PLAIN"]
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
    let review = view(&store, &who);
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
        when: None,
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
                ..sync(&review, None)
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
                when: None,
            },
        )
        .unwrap_err();
    assert_eq!(
        rootless.details["next"],
        json!("ployz env sync --to ENV --project shop --env production")
    );
}

#[test]
fn a_secret_the_receiver_lacks_syncs_with_the_value_given_or_without_one() {
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
    let lacking = Some(SecretRow { held: false });
    for label in ["api.env.KEY", "web.env.API_KEY"] {
        let secret = row(&review, label);
        assert_eq!(
            (&secret.secret, secret.change, secret.ticked),
            (&lacking, SyncChange::New, true)
        );
        assert_eq!(secret.from, json!({ "secret": true }));
    }
    assert_eq!(row(&review, "api").secret, None);

    // A value only fills a picked secret the receiver lacks; a pick needs its new node.
    let with = |picks: Option<&[&str]>, row: &SyncRow| SyncChanges {
        values: BTreeMap::from([(row.at.row.clone().into(), "given-key".into())]),
        ..sync(&review, picks)
    };
    for refused in [
        with(None, row(&review, "api")),
        with(
            Some(&["api", "api.env.KEY"]),
            row(&review, "web.env.API_KEY"),
        ),
        sync(&review, Some(&["api.env.KEY"])),
    ] {
        assert_eq!(
            store.write(&who, &refused).unwrap_err().code,
            RpcErrorCode::InvalidArgument,
            "{refused:?}"
        );
    }
    store
        .write(&who, &with(None, row(&review, "web.env.API_KEY")))
        .unwrap();
    assert!(view(&store, &who).rows.is_empty());
    // The value given is set, never shown; the other lands without one, and sending
    // that back keeps it.
    assert_eq!(
        values(&store, &who, "production", "web")["env"]["API_KEY"],
        json!({ "secret": true })
    );
    assert_eq!(
        values(&store, &who, "production", "api")["env"]["KEY"],
        json!({ "secret": false })
    );
    set(
        &store,
        &who,
        "production",
        &[("api.env.KEY", json!({ "secret": false }))],
    );

    // Deploy refuses, naming it, until production has its own.
    let refused = store
        .write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: DeploymentId::parse(uuid(50)).unwrap(),
                environment: at("production"),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &Trusted::default(),
        )
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(
        refused.message,
        "production has secrets without a value: set api.env.KEY before deploying"
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
    let input = deploy(&store, &who, "production", 3);
    // Each Service by its lineage: web's is 3, api's 5.
    let env = |n: u8| {
        let id = &input["lineages"][uuid(n)];
        input["snapshots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|snapshot| snapshot["serviceId"] == *id)
            .unwrap()["resolvedEnv"]
            .clone()
    };
    let web = env(3);
    assert_eq!(
        (&web["TOKEN"], &web["API_KEY"]),
        (&json!("prod-token"), &json!("given-key"))
    );
    assert_eq!(env(5)["KEY"], json!("prod-api-key"));
}

#[test]
fn a_new_service_arrives_without_its_sizing() {
    let (store, who) = shop(false);
    service(&store, &who, "fix-web", 5, "api", "api:1");
    set(&store, &who, "fix-web", &[("api.replicas", json!(3))]);
    let review = view(&store, &who);
    assert_eq!(labels(&review), ["api"]);
    store.write(&who, &sync(&review, None)).unwrap();
    assert_eq!(
        values(&store, &who, "production", "api")["replicas"],
        json!(1)
    );
}

#[test]
fn a_new_service_is_reviewed_and_undone_whole() {
    let (store, who) = shop(false);
    service(&store, &who, "fix-web", 5, "api", "api:1");
    // What the new node carries is part of what was reviewed.
    let review = view(&store, &who);
    set(&store, &who, "fix-web", &[("api.image", json!("api:2"))]);
    assert_eq!(
        store.write(&who, &sync(&review, None)).unwrap_err().code,
        RpcErrorCode::Conflict
    );

    // Edited after it synced, in what it carries or in a row that didn't land with
    // it, it isn't undone.
    for (path, value) in [
        ("api.image", json!("api:3")),
        ("api.startCommand", json!("go")),
    ] {
        let synced = sync_into(&store, &who, ("fix-web", "production"), None);
        set(&store, &who, "production", &[(path, value)]);
        assert_eq!(
            undo(&store, &who, &synced).unwrap_err().0,
            RpcErrorCode::Conflict,
            "{path}"
        );
        assert_eq!(services(&store, &who, "production"), ["api", "db", "web"]);
        discard(&store, &who, "api");
    }
    let synced = sync_into(&store, &who, ("fix-web", "production"), None);
    assert_eq!(undo(&store, &who, &synced), Ok("production".into()));
    assert_eq!(services(&store, &who, "production"), ["db", "web"]);
}

#[test]
fn a_deploy_settles_only_what_it_shipped() {
    let (store, who) = shop(true);
    set(&store, &who, "fix-web", &[("web.image", json!("web:2"))]);
    set(&store, &who, "production", &[("web.env.PLAIN", json!("2"))]);
    // Deployment 2 publishes before the Sync lands, so it ships web without it.
    let id = backend::admit(&store, &who, "production", 2);
    let synced = sync_into(&store, &who, ("fix-web", "production"), None);
    backend::run(&store, &id);
    assert_eq!(undo(&store, &who, &synced), Ok("production".into()));
}

#[test]
fn a_sync_that_closed_its_branch_can_still_be_undone() {
    let (store, who) = shop(false);
    set(&store, &who, "fix-web", &[("web.image", json!("web:2"))]);
    let closing = SyncChanges {
        when: Some(When::Now { close_after: true }),
        ..sync(&view(&store, &who), None)
    };
    let synced = store.write(&who, &closing).unwrap();
    assert!(matches!(synced.when, SyncedWhen::Now { closing: true, .. }));
    assert_eq!(undo(&store, &who, &synced), Ok("production".into()));
    assert_eq!(
        values(&store, &who, "production", "web")["image"],
        json!("web:1")
    );
}

/// A pick names its row by either side's name, or by a prefix for every row under it.
#[test]
fn picks_name_rows_by_either_sides_name_or_by_a_prefix() {
    let (store, who) = shop(true);
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
    set(
        &store,
        &who,
        "fix-web",
        &[
            ("web.env.A", json!("1")),
            ("web.env.B", json!("2")),
            ("web.image", json!("web:2")),
        ],
    );
    // As the receiver names it.
    store
        .write(&who, &sync(&view(&store, &who), Some(&["frontend.env.A"])))
        .unwrap();
    assert_eq!(labels(&view(&store, &who)), ["web.env.B", "web.image"]);
    store
        .write(&who, &sync(&view(&store, &who), Some(&["web.env"])))
        .unwrap();
    assert_eq!(labels(&view(&store, &who)), ["web.image"]);
    let unknown = store
        .write(&who, &sync(&view(&store, &who), Some(&["web.nope"])))
        .unwrap_err();
    assert_eq!(unknown.code, RpcErrorCode::NotFound);
    assert_eq!(
        unknown.details["valid_children"],
        json!(["frontend.image", "web.image"])
    );
}
