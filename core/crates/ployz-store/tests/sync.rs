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

use ployz_core::{RpcError, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Admit, AdmitRemoval, BranchQuery, Change, ConfigStore, CreateBranch, CreateProject,
    CreateService, DeploymentId, DeploymentStatus, DeploymentSummary, Discard, Edit, EnvironmentId,
    EnvironmentName, EnvironmentRef, EnvironmentRemoval, EnvironmentsQuery, KeepBranch,
    OrganizationId, Permission, ProjectId, ProjectName, Publish, Removal, Remove, RemoveService,
    RemovedBecause, RenameService, SealingTargetsQuery, SealingTargetsView, SecretRow, SecretValue,
    ServiceLineageId, ServiceQuery, SettingPath, Sweep, SyncChange, SyncChanges, SyncId, SyncQuery,
    SyncRow, SyncView, Synced, SystemEvent, Trusted, UndoSync, Written,
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
    shop_in(backend::open(), deployed)
}

fn shop_in(store: ConfigStore, deployed: bool) -> (ConfigStore, Actor) {
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
        close_after: false,
        version: view.version.clone(),
        picks: picks.map(|picks| picks.iter().map(|label| (*label).into()).collect()),
        skip: Vec::new(),
        values: BTreeMap::new(),
        id: None,
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
                target: Some(ployz_store::DiscardTarget::Head),
                path: Some(SettingPath::parse(path).unwrap()),
                version: None,
            },
        )
        .unwrap();
}

/// Remove the proposal `synced` included from `into`'s draft; false when gone.
fn remove(store: &ConfigStore, who: &Actor, into: &str, synced: &Synced) -> bool {
    let proposal = &synced.proposal;
    store
        .write(
            who,
            &ployz_store::RemoveProposal {
                environment: at(into),
                proposal: proposal.clone(),
                version: None,
            },
        )
        .unwrap()
        .removed
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
    assert_eq!(labels(&review), ["web.env.NEW", "web.source"]);
    let [new, image] = [row(&review, "web.env.NEW"), row(&review, "web.source")];
    assert_eq!(new.at.node().to_string(), "web");
    assert_eq!((new.ticked, new.change), (true, SyncChange::New));
    assert_eq!((&new.from, &new.into), (&json!("1"), &Value::Null));
    // A conflict starts unticked: taking it overwrites production's own change.
    assert_eq!((image.ticked, image.change), (false, SyncChange::Conflict));
    assert_eq!(
        (&image.from["image"], &image.into["image"]),
        (&json!("web:2"), &json!("web:hot"))
    );
    // The Branch's count to its Parent is the rows ticked by default.
    assert_eq!(to_parent(&store, &who), 1);

    // A stale version is refused with the fresh one.
    let stale = SyncChanges {
        version: "0:0".into(),
        ..sync(&review, None)
    };
    let stale = store.write(&who, &stale).unwrap_err();
    assert_eq!(stale.code, RpcErrorCode::Conflict);
    assert_eq!(stale.details["version"], json!(review.version));
    // A row not offered, picked or skipped, is refused with the rows there are; no
    // picks, nothing to do.
    let nope = || serde_json::from_value(json!(new.at.row().to_string().replace("NEW", "NOPE")));
    for unknown in [
        SyncChanges {
            picks: Some(vec![nope().unwrap()]),
            ..sync(&review, None)
        },
        SyncChanges {
            skip: vec![nope().unwrap()],
            ..sync(&review, None)
        },
    ] {
        let unknown = store.write(&who, &unknown).unwrap_err();
        assert_eq!(unknown.code, RpcErrorCode::NotFound);
        assert_eq!(
            unknown.details["valid_children"].as_array().unwrap().len(),
            2
        );
    }
    assert_eq!(
        store
            .write(&who, &sync(&review, Some(&[])))
            .unwrap_err()
            .code,
        RpcErrorCode::Conflict
    );

    // Only the image, by name: the variable is left out, so it is offered again.
    let synced = store
        .write(&who, &sync(&review, Some(&["web.source"])))
        .unwrap();
    assert_eq!(synced.into.name.as_str(), "production");
    let (staged, closing) = (&synced.staged, &synced.closing);
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
    // Named as it is, `site.env.X` names two rows: refused, not both.
    set(&store, &who, "fix-web", &[("web.env.X", json!("web:2"))]);
    let review = view(&store, &who);
    let refused = store
        .write(&who, &sync(&review, Some(&["site.env.X"])))
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Ambiguous, "{refused:?}");
}

#[test]
fn a_synced_mount_never_lands_where_the_receiver_mounts_another_volume() {
    let (store, who) = shop(false);
    let volume = |environment: &str, n: u8, name: &str, at: &str| {
        store
            .write(
                &who,
                &ployz_store::CreateVolume {
                    shared_writes: false,
                    storage_locked: false,
                    restored_from: None,
                    storage: ployz_core::config::VolumeKind::Docker {},
                    id: ployz_store::VolumeId::parse(uuid(n)).unwrap(),
                    environment: self::at(environment),
                    name: ployz_store::VolumeName::parse(name).unwrap(),
                    mounts: vec![ployz_store::Mount {
                        service: ServiceName::parse("web").unwrap(),
                        path: at.into(),
                    }],
                },
            )
            .unwrap();
    };
    volume("production", 20, "logs", "/logs");
    branch(&store, &who, 21, "production", "fix-logs");
    set(
        &store,
        &who,
        "fix-logs",
        &[("web.mounts.logs", json!("/x"))],
    );
    volume("production", 22, "cache", "/x/");
    let review = offered(&store, &who, "fix-logs", "production");
    let refused = store
        .write(&who, &sync(&review, Some(&["web.mounts.logs"])))
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    assert_eq!(
        refused.message,
        "web already mounts Volume cache at /x; pick another directory"
    );
}

/// Sync what `view` offers now, the rows named `only` (or else those ticked) less
/// those named `skip`.
fn skipping(view: &SyncView, only: Option<&[&str]>, skip: &[&str]) -> SyncChanges {
    SyncChanges {
        skip: skip.iter().map(|label| (*label).into()).collect(),
        ..sync(view, only)
    }
}

#[test]
fn a_skipped_new_node_stays_behind_with_its_rows() {
    let (store, who) = shop(false);
    service(&store, &who, "fix-web", 5, "api", "api:1");
    set(
        &store,
        &who,
        "fix-web",
        &[("api.env.A", json!("a")), ("web.env.PLAIN", json!("2"))],
    );
    let review = view(&store, &who);
    let api = row(&review, "api").at.row().to_string();
    store
        .write(&who, &skipping(&review, None, &[api.as_str()]))
        .unwrap();
    assert_eq!(services(&store, &who, "production"), ["db", "web"]);
    assert_eq!(
        values(&store, &who, "production", "web")["env"]["PLAIN"],
        json!("2")
    );
    assert_eq!(labels(&view(&store, &who)), ["api", "api.env.A"]);
}

#[test]
fn a_skip_names_a_row_as_the_receiver_calls_it() {
    let (store, who) = shop(false);
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
            ("web.image", json!("web:2")),
            ("web.env.A", json!("a")),
            ("web.env.B", json!("b")),
        ],
    );
    let review = view(&store, &who);
    store
        .write(
            &who,
            &skipping(&review, Some(&["frontend.source"]), &["frontend.env.A"]),
        )
        .unwrap();
    let landed = values(&store, &who, "production", "frontend");
    assert_eq!(landed["image"], json!("web:2"));
    assert_eq!(landed["env"].get("A"), None);
    let review = view(&store, &who);
    store
        .write(&who, &skipping(&review, None, &["frontend.env.A"]))
        .unwrap();
    let landed = values(&store, &who, "production", "frontend");
    assert_eq!(
        (landed["env"].get("A"), &landed["env"]["B"]),
        (None, &json!("b"))
    );
}

#[test]
fn a_synced_change_discarded_before_it_deploys_is_offered_again() {
    let (store, who) = shop(true);
    set(&store, &who, "fix-web", &[("web.image", json!("web:2"))]);
    service(&store, &who, "fix-web", 5, "api", "api:1");
    set(&store, &who, "fix-web", &[("api.env.MODE", json!("fast"))]);
    assert_eq!(
        labels(&view(&store, &who)),
        ["api", "api.env.MODE", "web.source"]
    );
    store.write(&who, &sync(&view(&store, &who), None)).unwrap();
    assert!(view(&store, &who).rows.is_empty());

    discard(&store, &who, "web.source");
    let again = view(&store, &who);
    assert_eq!(labels(&again), ["web.source"]);
    assert_eq!(row(&again, "web.source").change, SyncChange::Changed);
    discard(&store, &who, "api");
    let again = view(&store, &who);
    assert_eq!(labels(&again), ["api", "api.env.MODE", "web.source"]);
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
    discard(&store, &who, "web.source");
    assert_eq!(
        values(&store, &who, "production", "web")["image"],
        json!("web:2")
    );
    assert!(view(&store, &who).rows.is_empty());
    set(&store, &who, "fix-web", &[("web.image", json!("web:3"))]);
    assert_eq!(
        row(&view(&store, &who), "web.source").change,
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
    // A Sync no other refreshed keeps its Undo, refused once what it landed deployed.
    set(&store, &who, "fix-web", &[("web.env.LATER", json!("1"))]);
    let later = sync_into(
        &store,
        &who,
        ("fix-web", "production"),
        Some(&["web.env.LATER"]),
    );
    deploy(&store, &who, "production", 2);
    let refused = undo(&store, &who, &later).unwrap_err();
    assert_eq!(refused.0, RpcErrorCode::Conflict);
    assert!(
        refused.1.contains("is deployed: change it back instead"),
        "{refused:?}"
    );
}

/// Edit `environment` with Cloud's evidence for the repositories `acme/web` and
/// `acme/docs`.
fn edit_git(store: &ConfigStore, who: &Actor, environment: &str, changes: Vec<Change>) {
    let edit = Edit {
        environment: at(environment),
        expect: None,
        changes,
    };
    store
        .write_trusted(who, &edit, &backend::acme_repositories())
        .unwrap();
}

/// `web` in `environment` switched from its source to `to`, one setting of the other kind.
fn switch(store: &ConfigStore, who: &Actor, environment: &str, from: &str, to: (&str, Value)) {
    let unset = Change::Unset {
        path: SettingPath::parse(from).unwrap(),
    };
    let set = Change::Set {
        path: SettingPath::parse(to.0).unwrap(),
        value: to.1,
    };
    edit_git(store, who, environment, vec![unset, set]);
}

#[test]
fn undoing_a_source_switch_rewinds_the_base_to_its_source() {
    let (store, who) = shop(false);
    switch(
        &store,
        &who,
        "production",
        "web.image",
        ("web.repository", json!("acme/web")),
    );
    branch(&store, &who, 10, "production", "fix-git");
    switch(
        &store,
        &who,
        "fix-git",
        "web.repository",
        ("web.image", json!("web:2")),
    );
    let synced = sync_into(&store, &who, ("fix-git", "production"), None);
    assert_eq!(
        values(&store, &who, "production", "web")["image"],
        json!("web:2")
    );
    assert_eq!(undo(&store, &who, &synced), Ok("production".into()));
    let web = values(&store, &who, "production", "web");
    assert_eq!(
        (&web["repository"], &web["branch"]),
        (&json!("acme/web"), &json!("main"))
    );

    // The base holds production's repository again, so only fix-git changed it.
    switch(
        &store,
        &who,
        "fix-git",
        "web.image",
        ("web.repository", json!("acme/docs")),
    );
    let offered = offered(&store, &who, "fix-git", "production");
    assert!(
        offered
            .rows
            .iter()
            .all(|row| row.change != SyncChange::Conflict),
        "{:?}",
        ticks(&offered)
    );
    assert_eq!(row(&offered, "web.source").change, SyncChange::Changed);
}

/// Undo puts a repository back and leaves the git branch the receiver chose since.
#[test]
fn undoing_a_repository_sync_keeps_a_branch_chosen_since() {
    let (store, who) = shop(false);
    switch(
        &store,
        &who,
        "production",
        "web.image",
        ("web.repository", json!("acme/web")),
    );
    branch(&store, &who, 10, "production", "fix-git");
    let docs = Change::Set {
        path: SettingPath::parse("web.repository").unwrap(),
        value: json!("acme/docs"),
    };
    edit_git(&store, &who, "fix-git", vec![docs]);
    let synced = sync_into(&store, &who, ("fix-git", "production"), None);
    let dev = Change::Set {
        path: SettingPath::parse("web.branch").unwrap(),
        value: json!("dev"),
    };
    edit_git(&store, &who, "production", vec![dev]);
    assert_eq!(undo(&store, &who, &synced), Ok("production".into()));
    let web = values(&store, &who, "production", "web");
    assert_eq!(
        (&web["repository"], &web["branch"]),
        (&json!("acme/web"), &json!("dev"))
    );
}

#[test]
fn sealing_a_value_a_sync_replaced_keeps_no_plain_text_for_its_undo() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap(), false);
    set(
        &store,
        &who,
        "production",
        &[("web.env.PAIR_X", json!("plain-own"))],
    );
    set(
        &store,
        &who,
        "fix-web",
        &[("web.env.PAIR_X", json!("plain-landed"))],
    );
    let synced = sync_into(
        &store,
        &who,
        ("fix-web", "production"),
        Some(&["web.env.PAIR_X"]),
    );
    set(
        &store,
        &who,
        "production",
        &[(
            "web.env.PAIR_X",
            backend::sealed_secret(&uuid(2), &uuid(3), "PAIR_X", 1),
        )],
    );
    assert_eq!(backend::tables_holding(&url, "plain-own"), [""; 0]);

    // Back to the value that landed and saved, so no proposal holds the Sync, Undo
    // puts back the variable sealed and Not set.
    store
        .write(
            &who,
            &Edit {
                environment: at("production"),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse("web.env.PAIR_X").unwrap(),
                }],
            },
        )
        .unwrap();
    set(
        &store,
        &who,
        "production",
        &[("web.env.PAIR_X", json!("plain-landed"))],
    );
    store
        .write(
            &who,
            &Publish {
                id: None,
                environment: at("production"),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(undo(&store, &who, &synced), Ok("production".into()));
    assert_eq!(
        values(&store, &who, "production", "web")["env"]["PAIR_X"],
        json!({ "secret": false })
    );
    assert_eq!(backend::tables_holding(&url, "plain-own"), [""; 0]);
}

/// Sealing what a Sync landed keeps only its fingerprint in the arrival, owned by its
/// proposal or deployed, and Undo is refused: sealing changed it.
#[test]
fn sealing_a_value_a_sync_landed_keeps_only_its_fingerprint() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap(), false);
    set(
        &store,
        &who,
        "fix-web",
        &[
            ("web.env.PAIR_X", json!("plain-landed")),
            ("web.env.PAIR_Y", json!("plain-deployed")),
        ],
    );
    let deployed = sync_into(
        &store,
        &who,
        ("fix-web", "production"),
        Some(&["web.env.PAIR_Y"]),
    );
    deploy(&store, &who, "production", 1);
    let pending = sync_into(
        &store,
        &who,
        ("fix-web", "production"),
        Some(&["web.env.PAIR_X"]),
    );
    // The Branch moves on, so only production's arrivals hold what landed.
    set(
        &store,
        &who,
        "fix-web",
        &[
            ("web.env.PAIR_X", json!("branch-again")),
            ("web.env.PAIR_Y", json!("branch-again")),
        ],
    );
    for marker in ["plain-landed", "plain-deployed"] {
        assert!(backend::tables_holding(&url, marker).contains(&"config_sync_arrival".to_owned()));
    }
    set(
        &store,
        &who,
        "production",
        &[
            (
                "web.env.PAIR_X",
                backend::sealed_secret(&uuid(2), &uuid(3), "PAIR_X", 1),
            ),
            (
                "web.env.PAIR_Y",
                backend::sealed_secret(&uuid(2), &uuid(3), "PAIR_Y", 2),
            ),
        ],
    );
    for marker in ["plain-landed", "plain-deployed"] {
        assert!(!backend::tables_holding(&url, marker).contains(&"config_sync_arrival".to_owned()));
    }
    assert_eq!(
        undo(&store, &who, &pending),
        Err((
            RpcErrorCode::Conflict,
            "web.env.PAIR_X is sealed since it synced: set it as you want it instead".into()
        ))
    );
    assert_eq!(
        undo(&store, &who, &deployed),
        Err((
            RpcErrorCode::Conflict,
            "web.env.PAIR_Y is deployed: change it back instead".into()
        ))
    );
}

/// A secret added since to a node a Sync brought was never plain there: Undo says it
/// changed, not that it was sealed.
#[test]
fn a_secret_added_to_a_synced_node_is_a_change_not_a_seal() {
    let (store, who) = shop(false);
    service(&store, &who, "fix-web", 5, "api", "api:1");
    let synced = sync_into(&store, &who, ("fix-web", "production"), None);
    let api = store
        .read(
            &who,
            &ServiceQuery {
                environment: at("production"),
                service: ServiceName::parse("api").unwrap(),
            },
        )
        .unwrap()
        .service
        .service
        .id;
    set(
        &store,
        &who,
        "production",
        &[(
            "api.env.NEW",
            backend::sealed_secret(&uuid(2), api.as_str(), "NEW", 1),
        )],
    );
    // Saved, so no proposal holds the Sync.
    store
        .write(
            &who,
            &Publish {
                id: None,
                environment: at("production"),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(
        undo(&store, &who, &synced),
        Err((
            RpcErrorCode::Conflict,
            "api.env.NEW changed since it synced: change it back instead".into()
        ))
    );
}

/// A key sealed once and added again plain keeps its plain value for Undo, as History
/// keeps it.
#[test]
fn a_key_sealed_then_added_again_plain_keeps_its_value_for_undo() {
    let (store, who) = shop(false);
    set(
        &store,
        &who,
        "production",
        &[(
            "web.env.PAIR_X",
            backend::sealed_secret(&uuid(2), &uuid(3), "PAIR_X", 1),
        )],
    );
    store
        .write(
            &who,
            &Edit {
                environment: at("production"),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse("web.env.PAIR_X").unwrap(),
                }],
            },
        )
        .unwrap();
    set(
        &store,
        &who,
        "production",
        &[("web.env.PAIR_X", json!("plain-again"))],
    );
    set(
        &store,
        &who,
        "fix-web",
        &[("web.env.PAIR_X", json!("plain-landed"))],
    );
    let synced = sync_into(
        &store,
        &who,
        ("fix-web", "production"),
        Some(&["web.env.PAIR_X"]),
    );
    store
        .write(
            &who,
            &Publish {
                id: None,
                environment: at("production"),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(undo(&store, &who, &synced), Ok("production".into()));
    assert_eq!(
        values(&store, &who, "production", "web")["env"]["PAIR_X"],
        json!("plain-again")
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
    // That Sync refreshed the one before it, so once deployed neither is undone.
    assert_eq!(
        undo(&store, &who, &image).unwrap_err().0,
        RpcErrorCode::NotFound
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
        close_after: true,
        ..sync(&view(&store, &who), None)
    };
    store.write(&who, &keep(true)).unwrap();
    let refused = store.write(&who, &closing).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        refused.details["next"],
        json!({"tool": "keep_branch", "input": {
            "environment": {"project": "shop", "environment": "fix-web"}, "kept": false
        }})
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

#[test]
fn a_branch_a_sync_closed_lists_its_removal_before_and_after_the_sweep_starts_it() {
    let (store, who) = shop(true);
    deploy(&store, &who, "fix-web", 30);
    set(&store, &who, "fix-web", &[("web.image", json!("web:2"))]);
    let closing = SyncChanges {
        close_after: true,
        ..sync(&view(&store, &who), None)
    };
    assert!(store.write(&who, &closing).unwrap().closing);
    let removal = || {
        store
            .read(&who, &EnvironmentsQuery::default())
            .unwrap()
            .environments
            .into_iter()
            .find(|listing| listing.name.as_str() == "fix-web")
            .unwrap()
            .removal
    };
    // Before the sweep starts its Deployment, the close is pending.
    assert!(
        matches!(
            removal(),
            Some(EnvironmentRemoval::Pending {
                because: RemovedBecause::Synced,
                since,
            }) if since > 0
        ),
        "{:?}",
        removal()
    );

    let id = DeploymentId::parse(uuid::Uuid::new_v4().to_string()).unwrap();
    store
        .write_trusted(
            &who,
            &Admit::Remove(AdmitRemoval {
                id: id.clone(),
                environment: at("fix-web"),
            }),
            &Trusted::default(),
        )
        .unwrap();
    assert!(
        matches!(
            removal(),
            Some(EnvironmentRemoval::Deployment(running)) if running.id == id && running.in_flight
        ),
        "{:?}",
        removal()
    );
}

/// Run `fix-web`, change its `web` image to `image` and close it by a Sync.
fn close_by_sync(store: &ConfigStore, who: &Actor, n: u8, image: &str) {
    deploy(store, who, "fix-web", n);
    set(store, who, "fix-web", &[("web.image", json!(image))]);
    let closing = SyncChanges {
        close_after: true,
        ..sync(&view(store, who), Some(&["web.source"]))
    };
    assert!(store.write(who, &closing).unwrap().closing);
}

/// The Environments one sweep finds closing, by name.
fn swept(store: &ConfigStore, who: &Actor) -> Vec<String> {
    let Written::Automated(automated) = store
        .system(
            &who.organization,
            &SystemEvent::Sweep(Sweep { now: 0 }),
            &Trusted::default(),
        )
        .unwrap()
    else {
        panic!("a sweep writes Automated");
    };
    automated
        .closing
        .into_iter()
        .map(|environment| environment.name.to_string())
        .collect()
}

/// Admit `fix-web`'s removal as Cloud does for each Environment a sweep found closing.
fn admit_removal(
    store: &ConfigStore,
    who: &Actor,
    trusted: &Trusted,
) -> (DeploymentId, Result<DeploymentSummary, RpcError>) {
    let id = DeploymentId::parse(uuid::Uuid::new_v4().to_string()).unwrap();
    let admitted = store.write_trusted(
        who,
        &Admit::Remove(AdmitRemoval {
            id: id.clone(),
            environment: at("fix-web"),
        }),
        trusted,
    );
    (id, admitted)
}

fn listed_removal(store: &ConfigStore, who: &Actor) -> Option<EnvironmentRemoval> {
    store
        .read(who, &EnvironmentsQuery::default())
        .unwrap()
        .environments
        .into_iter()
        .find(|listing| listing.name.as_str() == "fix-web")
        .and_then(|listing| listing.removal)
}

#[test]
fn two_sweeps_that_both_find_a_close_due_admit_one_removal() {
    let (store, who) = shop(true);
    close_by_sync(&store, &who, 30, "web:2");
    // The hourly sweep and one a close asked for both read the mark before either admits.
    let fix_web = vec!["fix-web".to_owned()];
    assert_eq!(
        (swept(&store, &who), swept(&store, &who)),
        (fix_web.clone(), fix_web)
    );
    let (first, admitted) = admit_removal(&store, &who, &Trusted::default());
    admitted.unwrap();
    let (_, again) = admit_removal(&store, &who, &Trusted::default());
    assert_eq!(again.unwrap_err().code, RpcErrorCode::Conflict);
    let removal = listed_removal(&store, &who);
    assert!(
        matches!(
            &removal,
            Some(EnvironmentRemoval::Deployment(running)) if running.id == first && running.in_flight
        ),
        "{removal:?}"
    );
    assert_eq!(swept(&store, &who), Vec::<String>::new());
}

#[test]
fn two_sweeps_admit_one_removal_when_it_applies_at_once() {
    let (store, who) = shop(true);
    close_by_sync(&store, &who, 30, "web:2");
    let fix_web = vec!["fix-web".to_owned()];
    assert_eq!(
        (swept(&store, &who), swept(&store, &who)),
        (fix_web.clone(), fix_web)
    );
    // With no Servers counted, the removal applies as it is admitted.
    let none = Trusted {
        servers: Some(0),
        ..Trusted::default()
    };
    let (first, admitted) = admit_removal(&store, &who, &none);
    assert_eq!(admitted.unwrap().status, DeploymentStatus::Applied);
    let (_, again) = admit_removal(&store, &who, &none);
    assert_eq!(again.unwrap_err().code, RpcErrorCode::Conflict);
    let removal = listed_removal(&store, &who);
    assert!(
        matches!(
            &removal,
            Some(EnvironmentRemoval::Deployment(applied)) if applied.id == first
        ),
        "{removal:?}"
    );
    // The next sweep sees nothing of it runs, and deletes it.
    assert_eq!(swept(&store, &who), Vec::<String>::new());
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
fn a_new_branch_closing_under_a_removed_branchs_name_admits_its_own_removal() {
    let (store, who) = shop(true);
    let none = Trusted {
        servers: Some(0),
        ..Trusted::default()
    };
    close_by_sync(&store, &who, 30, "web:2");
    assert_eq!(swept(&store, &who), vec!["fix-web".to_owned()]);
    admit_removal(&store, &who, &none).1.unwrap();
    assert_eq!(swept(&store, &who), Vec::<String>::new());

    deploy(&store, &who, "production", 2);
    branch(&store, &who, 10, "production", "fix-web");
    close_by_sync(&store, &who, 31, "web:3");
    assert_eq!(swept(&store, &who), vec!["fix-web".to_owned()]);
    let (id, admitted) = admit_removal(&store, &who, &none);
    let admitted = admitted.unwrap();
    assert_eq!(
        (admitted.id, admitted.status),
        (id, DeploymentStatus::Applied)
    );
}

#[test]
fn a_close_whose_removal_row_is_gone_admits_its_removal_again() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap(), true);
    close_by_sync(&store, &who, 30, "web:2");
    assert_eq!(swept(&store, &who), vec!["fix-web".to_owned()]);
    let (first, admitted) = admit_removal(&store, &who, &Trusted::default());
    admitted.unwrap();
    // The mark names its removal in JSON, so the row can go while the mark stays.
    backend::execute(
        &url,
        &format!(
            "DELETE FROM config_deployment WHERE id = '{}'",
            first.as_str()
        ),
    );
    assert_eq!(swept(&store, &who), vec!["fix-web".to_owned()]);
    let (id, admitted) = admit_removal(&store, &who, &Trusted::default());
    let admitted = admitted.unwrap();
    assert_eq!(
        (admitted.id, admitted.status),
        (id, DeploymentStatus::Queued)
    );
}

#[test]
fn removing_a_branch_whose_close_waits_for_the_sweep_needs_no_new_confirmation() {
    let (store, who) = shop(true);
    deploy(&store, &who, "fix-web", 30);
    set(&store, &who, "fix-web", &[("web.image", json!("web:2"))]);
    let closing = SyncChanges {
        close_after: true,
        ..sync(&view(&store, &who), None)
    };
    assert!(store.write(&who, &closing).unwrap().closing);

    let again = Remove {
        project: ProjectName::parse("shop").unwrap(),
        environment: Some(EnvironmentName::parse("fix-web").unwrap()),
        version: None,
        accept_volume_loss: BTreeMap::new(),
        every_copy: BTreeMap::new(),
    };
    assert!(matches!(
        store.write_trusted(&who, &again, &Trusted::default()),
        Ok(Removal::Removing { .. })
    ));
}

#[test]
fn a_token_for_the_parent_alone_cannot_close_the_branch_it_syncs_from() {
    let (store, who) = shop(false);
    set(&store, &who, "fix-web", &[("web.image", json!("web:2"))]);
    let closing = SyncChanges {
        close_after: true,
        ..sync(&view(&store, &who), None)
    };
    let production_only = Trusted {
        permissions: Some(
            [
                Permission::ChangesStage,
                Permission::ChangesDeploy,
                Permission::EnvironmentsProtect,
            ]
            .into(),
        ),
        environments: Some([EnvironmentName::parse("production").unwrap()].into()),
        ..Trusted::default()
    };
    let refused = store
        .write_trusted(&who, &closing, &production_only)
        .unwrap_err();
    assert_eq!(refused.code.as_str(), "forbidden", "{refused:?}");
    assert_eq!(refused.message, "This token can deploy only to production.");
    assert_eq!(
        values(&store, &who, "production", "web")["image"],
        json!("web:1")
    );
    assert_eq!(
        values(&store, &who, "fix-web", "web")["image"],
        json!("web:2")
    );
    store
        .read(
            &who,
            &BranchQuery {
                environment: at("fix-web"),
            },
        )
        .unwrap();
    // Without closing it, the same token syncs into production.
    let staying = SyncChanges {
        close_after: false,
        ..closing
    };
    store
        .write_trusted(&who, &staying, &production_only)
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
    assert_eq!(ticks(&skip), ["-web.env.MID", "web.source"]);
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
        ["-web.env.MID", "-web.env.ROOT", "web.source"]
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
        ["web.source"]
    );

    // A change synced into its Parent stays fix-a's own until the Parent saves it:
    // then it is unticked sideways, and a Sync reviewed while it was ticked is stale.
    set(&store, &who, "fix-a", &[("web.env.SIDE", json!("1"))]);
    let before = offered(&store, &who, "fix-a", "fix-b");
    assert!(row(&before, "web.env.SIDE").ticked);
    sync_into(&store, &who, ("fix-a", "fix-web"), Some(&["web.env.SIDE"]));
    assert!(row(&offered(&store, &who, "fix-a", "fix-b"), "web.env.SIDE").ticked);
    store
        .write(
            &who,
            &Publish {
                id: None,
                environment: at("fix-web"),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
        )
        .unwrap();
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
    let copy = service_id(&store, &who, "fix-web", "web");
    set(
        &store,
        &who,
        "production",
        &[(
            "web.env.TOKEN",
            backend::sealed_secret(&uuid(2), &uuid(3), "TOKEN", 1),
        )],
    );
    set(
        &store,
        &who,
        "fix-web",
        &[
            (
                "web.env.TOKEN",
                backend::sealed_secret(&uuid(9), copy.as_str(), "TOKEN", 2),
            ),
            (
                "web.env.API_KEY",
                backend::sealed_secret(&uuid(9), copy.as_str(), "API_KEY", 3),
            ),
        ],
    );
    service(&store, &who, "fix-web", 5, "api", "api:1");
    set(
        &store,
        &who,
        "fix-web",
        &[(
            "api.env.KEY",
            backend::sealed_secret(&uuid(9), &uuid(5), "KEY", 4),
        )],
    );

    // production has TOKEN: it is never offered. The secrets it lacks are, flagged.
    let review = view(&store, &who);
    assert_eq!(labels(&review), ["api", "api.env.KEY", "web.env.API_KEY"]);
    let lacking = Some(SecretRow {});
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
        values: BTreeMap::from([(row.at.row().clone().into(), sealed(2, 3, "API_KEY", 5))]),
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

    // Not set never blocks: Deploy ships without it, and without what reads it, and
    // warns.
    set(
        &store,
        &who,
        "production",
        &[("web.env.KEY_URL", json!("https://${{ api.KEY }}@api"))],
    );
    let id = backend::admit(&store, &who, "production", 3);
    let input = backend::run(&store, &id);
    let env = |input: &Value, n: u8| {
        let id = &input["lineages"][uuid(n)];
        input["snapshots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|snapshot| snapshot["serviceId"] == *id)
            .unwrap()["resolvedEnv"]
            .clone()
    };
    let given = backend::client_sealed(&uuid(2), &uuid(3), "API_KEY", 5)["value"].clone();
    assert_eq!(env(&input, 5).get("KEY"), None);
    assert_eq!(env(&input, 3).get("KEY_URL"), None);
    assert_eq!(env(&input, 3)["API_KEY"], given);
    let shipped = store
        .read(&who, &ployz_store::DeploymentQuery { id })
        .unwrap();
    assert_eq!(
        json!(shipped.deployment.warnings),
        json!([
            { "service": "web", "variable": "KEY_URL", "reads": "api.KEY" },
            { "service": "api", "variable": "KEY" },
        ])
    );
    let api = service_id(&store, &who, "production", "api");
    let key = backend::client_sealed(&uuid(2), api.as_str(), "KEY", 6);
    set(
        &store,
        &who,
        "production",
        &[("api.env.KEY", json!({ "secret": key }))],
    );
    let input = deploy(&store, &who, "production", 4);
    // Each Service by its lineage: web's is 3, api's 5. Every secret reaches its
    // Service as the browser sealed it, a template around one unopened too.
    let web = env(&input, 3);
    assert_eq!(
        (&web["TOKEN"], &web["API_KEY"]),
        (
            &backend::client_sealed(&uuid(2), &uuid(3), "TOKEN", 1)["value"],
            &given
        )
    );
    assert_eq!(env(&input, 5)["KEY"], key["value"]);
    assert_eq!(
        web["KEY_URL"],
        json!({ "template": ["https://", key["value"], "@api"] })
    );
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
    // Discarding api kept fix-web included, so this Sync refreshed it: Undo is
    // refused, and Remove takes the whole proposal out.
    let synced = sync_into(&store, &who, ("fix-web", "production"), None);
    assert_eq!(
        undo(&store, &who, &synced),
        Err((
            RpcErrorCode::Conflict,
            "fix-web was refreshed since: Remove it in Details".into()
        ))
    );
    assert!(remove(&store, &who, "production", &synced));
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
        close_after: true,
        ..sync(&view(&store, &who), None)
    };
    let synced = store.write(&who, &closing).unwrap();
    assert!(synced.closing);
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
    assert_eq!(labels(&view(&store, &who)), ["web.env.B", "web.source"]);
    store
        .write(&who, &sync(&view(&store, &who), Some(&["web.env"])))
        .unwrap();
    assert_eq!(labels(&view(&store, &who)), ["web.source"]);
    let unknown = store
        .write(&who, &sync(&view(&store, &who), Some(&["web.nope"])))
        .unwrap_err();
    assert_eq!(unknown.code, RpcErrorCode::NotFound);
    assert_eq!(
        unknown.details["valid_children"],
        json!(["frontend.source", "web.source"])
    );
}

/// A synced source that turns credentials on brings the Branch's credential; one
/// already on keeps the receiver's own.
#[test]
fn a_synced_source_brings_its_credential_only_where_it_turns_credentials_on() {
    let (store, who) = shop(false);
    let pulls_with = |n: u8| {
        let id = backend::admit(&store, &who, "production", n);
        let runner = ployz_store::RunnerId::parse("runner").unwrap();
        let claimed = store.claim(&id, &runner).unwrap().intent.registry_auth;
        backend::run(&store, &id);
        claimed
            .into_values()
            .map(|auth| auth.password)
            .collect::<Vec<_>>()
    };
    let credential = |environment: &str, secret: &str| {
        set(
            &store,
            &who,
            environment,
            &[("web.registryCredential", json!({ "secret": secret }))],
        );
    };
    credential("fix-web", "branch-token");
    sync_into(
        &store,
        &who,
        ("fix-web", "production"),
        Some(&["web.source"]),
    );
    assert_eq!(pulls_with(1), ["branch-token"]);

    credential("production", "own-token");
    set(&store, &who, "fix-web", &[("web.image", json!("web:2"))]);
    sync_into(
        &store,
        &who,
        ("fix-web", "production"),
        Some(&["web.source"]),
    );
    assert_eq!(values(&store, &who, "production", "web")["image"], "web:2");
    assert_eq!(pulls_with(2), ["own-token"]);
}

#[test]
fn a_config_file_syncs_as_one_row_and_a_conflict_replaces_the_whole_file() {
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
    service(&store, &who, "production", 3, "web", "web:1");
    let config = |name: &str| ployz_core::ConfigName::parse(name).unwrap();
    let file = |name: &str| ployz_core::ConfigFileName::parse(name).unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse(uuid(10)).unwrap(),
                environment: at("production"),
                name: config("sentry"),
                mounts: vec![ployz_store::ConfigMountAt {
                    service: ServiceName::parse("web").unwrap(),
                    dir: "/etc/sentry".into(),
                }],
            },
        )
        .unwrap();
    let put = |environment: &str, path: &str, content: &str| {
        store
            .write(
                &who,
                &ployz_store::PutConfigFile {
                    environment: at(environment),
                    config: config("sentry"),
                    file: file(path),
                    content: content.into(),
                    mode: None,
                    uid: None,
                    gid: None,
                },
            )
            .unwrap();
    };
    put("production", "a.yml", "one\ntwo\nthree\n");
    branch(&store, &who, 9, "production", "fix-web");
    assert!(view(&store, &who).rows.is_empty());

    put("fix-web", "a.yml", "one\ntwo\nthree\nfour\n");
    put("fix-web", "conf.d/b.yml", "b\n");
    put("production", "a.yml", "zero\ntwo\nthree\n");
    let review = view(&store, &who);
    let mut rows: Vec<(String, SyncChange)> = review
        .rows
        .iter()
        .map(|row| (row.at.row().to_string(), row.change))
        .collect();
    rows.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        rows,
        [
            (format!("{}:files.a.yml", uuid(10)), SyncChange::Conflict),
            (format!("{}:files.conf.d/b.yml", uuid(10)), SyncChange::New),
        ]
    );

    // The conflict starts unticked, so both are picked.
    let picks: Vec<String> = review.rows.iter().map(|row| row.at.to_string()).collect();
    let picks: Vec<&str> = picks.iter().map(String::as_str).collect();
    store.write(&who, &sync(&review, Some(&picks))).unwrap();
    let contents = store
        .read(
            &who,
            &ployz_store::ConfigItemQuery {
                environment: at("production"),
                config: config("sentry").into(),
            },
        )
        .unwrap()
        .contents;
    assert_eq!(contents[&file("a.yml")], "one\ntwo\nthree\nfour\n");
    assert_eq!(contents[&file("conf.d/b.yml")], "b\n");
    assert!(view(&store, &who).rows.is_empty());
}

#[test]
fn picking_a_config_file_by_name_picks_that_file_and_not_one_its_name_prefixes() {
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
    service(&store, &who, "production", 3, "web", "web:1");
    let config = || ployz_core::ConfigName::parse("sentry").unwrap();
    let file = |name: &str| ployz_core::ConfigFileName::parse(name).unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse(uuid(10)).unwrap(),
                environment: at("production"),
                name: config(),
                mounts: vec![ployz_store::ConfigMountAt {
                    service: ServiceName::parse("web").unwrap(),
                    dir: "/etc/sentry".into(),
                }],
            },
        )
        .unwrap();
    let put = |environment: &str, path: &str, content: &str| {
        store
            .write(
                &who,
                &ployz_store::PutConfigFile {
                    environment: at(environment),
                    config: config(),
                    file: file(path),
                    content: content.into(),
                    mode: None,
                    uid: None,
                    gid: None,
                },
            )
            .unwrap();
    };
    put("production", "app.conf", "old\n");
    branch(&store, &who, 9, "production", "fix-web");
    put("fix-web", "app.conf", "new\n");
    put("fix-web", "app.conf.bak", "backup\n");

    sync_into(
        &store,
        &who,
        ("fix-web", "production"),
        Some(&["configs.sentry.files.app.conf"]),
    );
    let contents = store
        .read(
            &who,
            &ployz_store::ConfigItemQuery {
                environment: at("production"),
                config: config().into(),
            },
        )
        .unwrap()
        .contents;
    assert_eq!(
        contents.into_iter().collect::<Vec<_>>(),
        [(file("app.conf"), "new\n".to_owned())]
    );
}

/// `web.env.API_KEY` of `production` sealed in the browser, holding ciphertext `n`.
fn api_key(n: u8) -> SecretValue {
    sealed(2, 3, "API_KEY", n)
}

/// A value sealed in the browser for variable `key` of Service `service` in
/// Environment `environment`, holding ciphertext `n`.
fn sealed(environment: u8, service: u8, key: &str, n: u8) -> SecretValue {
    let value = backend::client_sealed(&uuid(environment), &uuid(service), key, n);
    serde_json::from_value(value).unwrap()
}

#[test]
fn a_value_sealed_in_the_browser_fills_a_secret_only_sealed_for_it() {
    let (store, who) = shop(false);
    let copy = service_id(&store, &who, "fix-web", "web");
    set(
        &store,
        &who,
        "fix-web",
        &[(
            "web.env.API_KEY",
            backend::sealed_secret(&uuid(9), copy.as_str(), "API_KEY", 1),
        )],
    );
    service(&store, &who, "fix-web", 5, "api", "api:1");
    set(
        &store,
        &who,
        "fix-web",
        &[(
            "api.env.KEY",
            backend::sealed_secret(&uuid(9), &uuid(5), "KEY", 1),
        )],
    );
    let review = view(&store, &who);
    let with = |picks: Option<&[&str]>, label: &str, value: SecretValue| SyncChanges {
        values: BTreeMap::from([(row(&review, label).at.row().clone().into(), value)]),
        ..sync(&review, picks)
    };
    let refused = |changes: SyncChanges, why: &str| {
        let error = store.write(&who, &changes).unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{error:?}");
        assert!(error.message.contains(why), "{error:?}");
        assert!(!format!("{error:?}").contains("AAAA"), "{error:?}");
    };
    for (value, why) in [
        (sealed(9, 3, "API_KEY", 1), "sealed for another Environment"),
        (sealed(2, 3, "TOKEN", 1), "sealed for another variable"),
        (sealed(2, 4, "API_KEY", 1), "sealed for another Service"),
    ] {
        refused(
            with(Some(&["web.env.API_KEY"]), "web.env.API_KEY", value),
            why,
        );
    }
    // A new Service has no ID in the receiver yet to seal for.
    refused(
        with(
            Some(&["api", "api.env.KEY"]),
            "api.env.KEY",
            sealed(2, 5, "KEY", 1),
        ),
        "isn't there yet",
    );
    let synced = store
        .write(
            &who,
            &with(Some(&["web.env.API_KEY"]), "web.env.API_KEY", api_key(1)),
        )
        .unwrap();
    assert!(!format!("{synced:?}").contains("AAAA"), "{synced:?}");
    assert_eq!(
        values(&store, &who, "production", "web")["env"]["API_KEY"],
        json!({ "secret": true })
    );
    assert_eq!(labels(&view(&store, &who)), ["api", "api.env.KEY"]);
}

#[test]
fn a_value_sealed_for_the_parent_arrives_not_set_in_a_branch_created_from_it() {
    let (store, who) = shop(false);
    let value = backend::client_sealed(&uuid(2), &uuid(3), "API_KEY", 1);
    set(
        &store,
        &who,
        "production",
        &[("web.env.API_KEY", json!({ "secret": value }))],
    );
    assert_eq!(
        values(&store, &who, "production", "web")["env"]["API_KEY"],
        json!({ "secret": true })
    );
    branch(&store, &who, 10, "production", "fix-two");
    assert_eq!(
        values(&store, &who, "fix-two", "web")["env"]["API_KEY"],
        json!({ "secret": false })
    );
    // Follow carries it to fix-web the same way once production deploys it.
    deploy(&store, &who, "production", 2);
    assert_eq!(
        values(&store, &who, "fix-web", "web")["env"]["API_KEY"],
        json!({ "secret": false })
    );
    // A value the Branch sealed for its own copy of web stays through its
    // Parent's next Deploy.
    let copy = store
        .read(
            &who,
            &ServiceQuery {
                environment: at("fix-two"),
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap()
        .service
        .service
        .id;
    let own = backend::client_sealed(&uuid(10), copy.as_str(), "API_KEY", 2);
    set(
        &store,
        &who,
        "fix-two",
        &[("web.env.API_KEY", json!({ "secret": own }))],
    );
    set(&store, &who, "production", &[("web.env.PLAIN", json!("2"))]);
    deploy(&store, &who, "production", 3);
    assert_eq!(
        values(&store, &who, "fix-two", "web")["env"]["API_KEY"],
        json!({ "secret": true })
    );
}

/// The ID of Service `name` in `environment`: its own, not its lineage.
fn service_id(store: &ConfigStore, who: &Actor, environment: &str, name: &str) -> ServiceLineageId {
    store
        .read(
            who,
            &ServiceQuery {
                environment: at(environment),
                service: ServiceName::parse(name).unwrap(),
            },
        )
        .unwrap()
        .service
        .service
        .id
}

/// Where a Sync from fix-web into its Parent at `version` lands `rows`.
fn sync_targets(
    store: &ConfigStore,
    who: &Actor,
    version: &str,
    rows: &[&str],
) -> Result<SealingTargetsView, RpcError> {
    store.read(
        who,
        &SealingTargetsQuery::Sync {
            from: at("fix-web"),
            into: None,
            version: version.into(),
            rows: rows.iter().map(|row| (*row).into()).collect(),
        },
    )
}

#[test]
fn sealing_targets_resolve_each_row_as_the_sync_does_to_the_receiver_s_own_service() {
    let (store, who) = shop(false);
    // fix-web's web is an Own Copy: its own ID, web's lineage.
    let copy = service_id(&store, &who, "fix-web", "web");
    assert_ne!(copy.as_str(), uuid(3));
    let edit = store
        .read(
            &who,
            &SealingTargetsQuery::Edit {
                environment: at("fix-web"),
            },
        )
        .unwrap();
    let SealingTargetsView::Edit { services, .. } = &edit else {
        panic!("{edit:?}");
    };
    let web = services
        .iter()
        .find(|service| service.name.as_str() == "web")
        .unwrap();
    assert_eq!(web.service.unwrap().to_string(), copy.as_str());
    // production calls web site now: the row is web.env.API_KEY in fix-web and
    // site.env.API_KEY in production, and lands in production's site.
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
    set(
        &store,
        &who,
        "fix-web",
        &[
            (
                "web.env.API_KEY",
                backend::sealed_secret(&uuid(9), copy.as_str(), "API_KEY", 1),
            ),
            ("web.image", json!("web:2")),
        ],
    );
    let review = view(&store, &who);
    let api_key = row(&review, "web.env.API_KEY").at.row().clone();
    for asked in ["web.env.API_KEY", "site.env.API_KEY", &api_key.to_string()] {
        let targets = sync_targets(&store, &who, &review.version, &[asked]).unwrap();
        let SealingTargetsView::Sync {
            organization,
            into,
            values,
        } = targets
        else {
            panic!("{targets:?}");
        };
        assert_eq!(organization, "org");
        assert_eq!(into.name.as_str(), "production");
        assert_eq!(values.len(), 1, "{values:?}");
        assert_eq!(values[0].asked, asked.into());
        assert_eq!(values[0].row, api_key);
        assert_eq!(values[0].service.to_string(), uuid(3));
        assert_eq!(values[0].key, "API_KEY");
    }
    let refused = |rows: &[&str], code: RpcErrorCode, why: &str| {
        let error = sync_targets(&store, &who, &review.version, rows).unwrap_err();
        assert_eq!(error.code, code, "{error:?}");
        assert!(error.message.contains(why), "{error:?}");
    };
    refused(
        &["web.source"],
        RpcErrorCode::InvalidArgument,
        "isn't a secret",
    );
    refused(&["web.env.NOPE"], RpcErrorCode::NotFound, "No row named");
    let stale = sync_targets(&store, &who, "0:stale", &["web.env.API_KEY"]).unwrap_err();
    assert_eq!(stale.code, RpcErrorCode::Conflict, "{stale:?}");
    // A new Service's secret has no Service in the receiver to seal for yet.
    service(&store, &who, "fix-web", 5, "api", "api:1");
    set(
        &store,
        &who,
        "fix-web",
        &[(
            "api.env.KEY",
            backend::sealed_secret(&uuid(9), &uuid(5), "KEY", 1),
        )],
    );
    let review = view(&store, &who);
    let error = sync_targets(&store, &who, &review.version, &["api.env.KEY"]).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{error:?}");
    assert_eq!(
        error.message,
        "api.env.KEY: its Service isn't there yet, so nothing can be sealed for it"
    );
    assert_eq!(
        error.details["next"],
        json!(
            "sync without --value first, then ployz set api.env.KEY --secret --env production --project shop"
        )
    );
}

#[test]
fn plain_text_for_a_synced_secret_is_refused_and_an_old_cli_is_told_to_update() {
    let (store, who) = shop(false);
    let copy = service_id(&store, &who, "fix-web", "web");
    let with_value = |label: &str| {
        let review = view(&store, &who);
        SyncChanges {
            values: BTreeMap::from([(label.into(), "plain-value".into())]),
            ..sync(&review, Some(&[label]))
        }
    };
    set(
        &store,
        &who,
        "fix-web",
        &[(
            "web.env.TOKEN",
            backend::sealed_secret(&uuid(9), copy.as_str(), "TOKEN", 1),
        )],
    );
    let cli = Trusted {
        from_cli: true,
        ..Trusted::default()
    };
    let error = store
        .write_trusted(&who, &with_value("web.env.TOKEN"), &cli)
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Unsupported, "{error:?}");
    assert_eq!(error.message, "TOKEN: update ployz to set a Sealed value.");
    assert_eq!(error.details["next"], "curl -fsSL https://ployz.sh | sh");
    assert!(!format!("{error:?}").contains("plain-value"), "{error:?}");
    // From anyone else, plain text is refused too: a value arrives only sealed.
    let error = store.write(&who, &with_value("web.env.TOKEN")).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{error:?}");
    assert!(error.message.contains("sealed on your device"), "{error:?}");
    assert!(!format!("{error:?}").contains("plain-value"), "{error:?}");
    assert_eq!(
        values(&store, &who, "production", "web")["env"]["TOKEN"],
        Value::Null
    );
}
