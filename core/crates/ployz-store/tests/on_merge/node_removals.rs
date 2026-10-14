use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Automated, ConfigStore, CreateGitService, CreateProject, DiffQuery, EnvironmentId,
    EnvironmentName, EnvironmentQuery, EnvironmentRef, HistoryAction, HistoryPreviewQuery,
    NodeName, OnMergeCheck, OnMergeConclusion, OrganizationId, ProjectId, ProjectName, Ride,
    ServiceLineageId, SetPrPlan, StageHistory, Trusted,
};
use serde_json::{Value, json};

use crate::backend;
use crate::support::{
    HEAD, LATER, MERGE, at, check, config, exclude, history, labels, merge_value, merged, on_merge,
    passed, production_diff, pull, push, row, runner, save, set, shop_copying, system, unset, uuid,
    value, vouched, watching,
};

fn node(name: &str) -> NodeName {
    NodeName::parse(name).unwrap()
}

fn image_service(store: &ConfigStore, who: &Actor, environment: &str, n: u8, name: &str) {
    store
        .write(
            who,
            &ployz_store::CreateService {
                id: ServiceLineageId::parse(uuid(n)).unwrap(),
                environment: at(environment),
                name: ServiceName::parse(name).unwrap(),
                image: Some(format!("{name}:1")),
                template: None,
            },
        )
        .unwrap();
}

fn volume(store: &ConfigStore, who: &Actor, n: u8, name: &str, on: &str) {
    store
        .write(
            who,
            &ployz_store::CreateVolume {
                shared_writes: false,
                storage_locked: false,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: ployz_store::VolumeId::parse(uuid(n)).unwrap(),
                environment: at("production"),
                name: ployz_store::VolumeName::parse(name).unwrap(),
                mounts: vec![ployz_store::Mount {
                    service: ServiceName::parse(on).unwrap(),
                    path: format!("/var/lib/{name}"),
                }],
            },
        )
        .unwrap();
}

/// production's `db`, its Volume `data` and the Config `sentry` that `web` mounts,
/// beside `worker`, its Volume `cache` and an unmounted Config `extra`, all saved.
/// `web.env.DB_URL` references `db`.
fn production_nodes(store: &ConfigStore, who: &Actor) {
    image_service(store, who, "production", 20, "db");
    image_service(store, who, "production", 21, "worker");
    volume(store, who, 22, "data", "db");
    volume(store, who, 23, "cache", "worker");
    config(store, who, 24, "sentry", &["web"]);
    config(store, who, 25, "extra", &[]);
    store
        .write(
            who,
            &ployz_store::PutConfigFile {
                environment: at("production"),
                config: ployz_core::ConfigName::parse("sentry").unwrap(),
                file: ployz_core::ConfigFileName::parse("sentry.yml").unwrap(),
                content: "dsn: ${{ web.PLOYZ_PRIVATE_DOMAIN }}".into(),
                mode: Some(ployz_core::config::FileMode::parse("0640").unwrap()),
                uid: Some(1000),
                gid: None,
            },
        )
        .unwrap();
    set(
        store,
        who,
        "production",
        &[
            ("web.env.DB_URL", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}")),
            ("db.env.PASSWORD", json!({ "secret": "hunter2" })),
            ("db.env.USER", json!("app")),
            (
                "db.registryCredential",
                json!({ "username": "bot", "secret": "pull-token" }),
            ),
        ],
    );
    save(store, who, "production");
}

/// [`shop`] over [`production_nodes`]: pr-5 copies `web`, `sentry`, `db` and
/// `data`, and leaves `worker`, `cache` and `extra` in production.
fn shop_with_nodes() -> (ConfigStore, Actor) {
    shop_copying(
        production_nodes,
        Some(vec![
            node("db"),
            node("volumes.data"),
            node("configs.sentry"),
        ]),
    )
}

fn remove_service(store: &ConfigStore, who: &Actor, environment: &str, service: &str) {
    store
        .write(
            who,
            &ployz_store::RemoveService {
                environment: at(environment),
                service: ServiceName::parse(service).unwrap(),
            },
        )
        .unwrap();
}

/// pr-5 stops referencing `db` and removes it.
fn remove_db(store: &ConfigStore, who: &Actor) {
    set(
        store,
        who,
        "pr-5",
        &[("web.env.DB_URL", json!("db.internal"))],
    );
    remove_service(store, who, "pr-5", "db");
}

/// Whether `environment`'s latest Saved revision has `name`, by what Working
/// changes against it.
fn saved_has(store: &ConfigStore, who: &Actor, environment: &str, name: &str) -> bool {
    let working = value(store, who, environment, &format!("{name}.image")).is_some();
    let diff = store
        .read(
            who,
            &DiffQuery {
                environment: at(environment),
            },
        )
        .unwrap();
    let drafted = diff.draft_changes.iter().find(|change| change.name == name);
    match drafted.map(|change| change.lifecycle) {
        Some(ployz_core::config::ReviewLifecycleKind::Create) => false,
        Some(ployz_core::config::ReviewLifecycleKind::Delete) => true,
        _ => working,
    }
}

#[test]
fn production_nodes_the_pr_environment_never_copied_make_no_rows() {
    let (store, who) = shop_with_nodes();
    assert_eq!(labels(&store, &who), []);
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    assert_eq!(
        labels(&store, &who),
        [("web.env.A".to_owned(), Ride::Rides)]
    );

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    for name in ["db", "worker"] {
        assert!(saved_has(&store, &who, "production", name), "{name}");
    }
}

#[test]
fn a_removed_service_rides_and_the_merge_takes_it_out_of_production() {
    let (store, who) = shop_with_nodes();
    remove_db(&store, &who);
    let db = row(&store, &who, "db");
    assert_eq!(
        (db.lands, db.preview, db.ride, db.conflict, db.refused_by),
        (Value::Null, Value::Null, Ride::Rides, false, vec![])
    );
    assert_ne!(db.production, Value::Null);
    assert_eq!(row(&store, &who, "web.env.DB_URL").ride, Ride::Rides);

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(!saved_has(&store, &who, "production", "db"));
    assert_eq!(value(&store, &who, "production", "db.image"), None);
    assert_eq!(
        value(&store, &who, "production", "web.env.DB_URL"),
        Some(json!("db.internal"))
    );
    assert!(production_diff(&store, &who).published);
    // The rest of production stays.
    assert!(saved_has(&store, &who, "production", "worker"));
}

#[test]
fn an_excluded_removal_keeps_the_node_until_included_again() {
    let (store, who) = shop_with_nodes();
    remove_db(&store, &who);
    exclude(&store, &who, "db", false);
    assert_eq!(row(&store, &who, "db").ride, Ride::Excluded);
    exclude(&store, &who, "db", true);
    assert_eq!(row(&store, &who, "db").ride, Ride::Rides);
    exclude(&store, &who, "db", false);

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(saved_has(&store, &who, "production", "db"));
    assert_eq!(
        value(&store, &who, "production", "web.env.DB_URL"),
        Some(json!("db.internal"))
    );
}

#[test]
fn removing_a_node_production_changed_since_is_a_conflict() {
    let (store, who) = shop_with_nodes();
    set(
        &store,
        &who,
        "production",
        &[("db.env.USER", json!("admin"))],
    );
    save(&store, &who, "production");
    remove_db(&store, &who);
    let db = row(&store, &who, "db");
    assert_eq!((db.ride, db.conflict), (Ride::Rides, true));
}

#[test]
fn a_removal_production_still_references_stays_in_use_and_the_rest_lands() {
    let (store, who) = shop_with_nodes();
    remove_db(&store, &who);
    // production's worker references db since pr-5 started.
    set(
        &store,
        &who,
        "production",
        &[("worker.env.DB", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}"))],
    );
    save(&store, &who, "production");
    let db = row(&store, &who, "db");
    assert_eq!(
        (
            db.ride,
            db.refused_by
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        ),
        (Ride::Rides, vec!["worker.env.DB".to_owned()])
    );

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(saved_has(&store, &who, "production", "db"));
    assert_eq!(
        value(&store, &who, "production", "web.env.DB_URL"),
        Some(json!("db.internal"))
    );
    assert!(production_diff(&store, &who).published);
}

#[test]
fn a_removal_is_refused_by_the_value_that_lands_not_the_previews() {
    let (store, who) = shop_with_nodes();
    remove_db(&store, &who);
    // Excluding the edit that stopped referencing db keeps production's reference.
    exclude(&store, &who, "web.env.DB_URL", false);
    let refused: Vec<String> = row(&store, &who, "db")
        .refused_by
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(refused, ["web.env.DB_URL"]);
    exclude(&store, &who, "web.env.DB_URL", true);
    assert_eq!(row(&store, &who, "db").refused_by, []);

    // An Edit landing the reference refuses it too.
    store
        .write(
            &who,
            &merge_value("web.env.DB_URL", Some("${{ db.PLOYZ_PRIVATE_DOMAIN }}")),
        )
        .unwrap();
    assert_eq!(row(&store, &who, "db").refused_by.len(), 1);
}

#[test]
fn a_removed_volume_is_kept_with_its_data() {
    let (store, who) = shop_with_nodes();
    store
        .write(
            &who,
            &ployz_store::RemoveVolume {
                environment: at("pr-5"),
                volume: ployz_store::VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    let data = row(&store, &who, "volumes.data");
    assert_eq!((data.ride, &data.lands), (Ride::Kept, &data.production));
    assert_ne!(data.production, Value::Null);
    // db keeps its mount on the Volume it keeps.
    assert_eq!(
        labels(&store, &who),
        [
            ("db.mounts.data".to_owned(), Ride::Kept),
            ("volumes.data".to_owned(), Ride::Kept)
        ]
    );

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(
        value(&store, &who, "production", "db.mounts.data"),
        Some(json!("/var/lib/data"))
    );
    let kept = store.read(
        &who,
        &ployz_store::VolumeQuery {
            environment: at("production"),
            volume: ployz_store::VolumeName::parse("data").unwrap(),
        },
    );
    assert!(kept.is_ok(), "{kept:?}");
    assert!(production_diff(&store, &who).published);
}

#[test]
fn a_draft_on_a_removed_service_stays_in_working_while_saved_takes_the_removal() {
    let (store, who) = shop_with_nodes();
    remove_db(&store, &who);
    // Only whether USER is exported changes: its value, and so its cell, doesn't.
    set(
        &store,
        &who,
        "production",
        &[("db.env.USER.exported", json!("true"))],
    );

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(!saved_has(&store, &who, "production", "db"));
    assert_eq!(
        value(&store, &who, "production", "db.env.USER.exported"),
        Some(json!(true))
    );
}

#[test]
fn undo_brings_a_removed_service_and_config_back_whole() {
    let (store, who) = shop_with_nodes();
    let snapshot = |store: &ConfigStore| {
        let settings = store
            .read(
                &who,
                &EnvironmentQuery {
                    environment: at("production"),
                    path: None,
                    all: true,
                },
            )
            .unwrap()
            .settings;
        let db = store
            .read(
                &who,
                &ployz_store::ServiceQuery {
                    environment: at("production"),
                    service: ServiceName::parse("db").unwrap(),
                },
            )
            .unwrap();
        let configs = store
            .read(
                &who,
                &ployz_store::ConfigsQuery {
                    environment: at("production"),
                },
            )
            .unwrap()
            .configs;
        json!({
            "settings": settings,
            "db": [db.service, db.lineage, db.values],
            "configs": configs,
        })
    };
    let before = snapshot(&store);
    remove_db(&store, &who);
    store
        .write(
            &who,
            &ployz_store::DeleteConfig {
                environment: at("pr-5"),
                config: ployz_core::ConfigName::parse("sentry").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(row(&store, &who, "configs.sentry").ride, Ride::Rides);

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(!saved_has(&store, &who, "production", "db"));
    let landed = history(&store, &who)
        .into_iter()
        .find(|revision| revision.message.as_deref() == Some("Merged #5"))
        .unwrap();
    let query = HistoryPreviewQuery {
        environment: at("production"),
        revision: landed.revision,
        action: HistoryAction::Undo,
        change: None,
    };
    let preview = store.read(&who, &query).unwrap();
    let staged = store
        .write(
            &who,
            &StageHistory {
                environment: at("production"),
                revision: query.revision,
                action: query.action,
                change: None,
                version: preview.version,
                accept_overwrite: false,
            },
        )
        .unwrap();
    assert_eq!(
        staged
            .not_set
            .iter()
            .map(|variable| format!("{}.{}", variable.service, variable.key))
            .collect::<Vec<_>>(),
        ["db.PASSWORD"]
    );
    // IDs, mounts, files, and the registry credential kept by ID come back as they were. History
    // holds no sealed value, so the sealed PASSWORD of the removed Service comes back Not set.
    let mut expected = before;
    expected["db"][2]["env"]["PASSWORD"] = json!({ "secret": false });
    for setting in expected["settings"].as_array_mut().unwrap() {
        if setting["path"] == "db.env.PASSWORD" {
            setting["value"] = json!({ "secret": false });
        }
    }
    assert_eq!(snapshot(&store), expected);
}

#[test]
fn each_destination_decides_its_own_removal() {
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
    // production deploys `release`; its Branches staging and qa deploy `main`.
    store
        .write_trusted(
            &who,
            &CreateGitService {
                id: ServiceLineageId::parse(uuid(3)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                repository: backend::repo_name("acme/web"),
                branch: Some(backend::git_branch("release")),
            },
            &vouched(),
        )
        .unwrap();
    image_service(&store, &who, "production", 20, "db");
    save(&store, &who, "production");
    for (n, name) in [(30, "staging"), (31, "qa")] {
        store
            .write(
                &who,
                &ployz_store::CreateBranch {
                    id: EnvironmentId::parse(uuid(n)).unwrap(),
                    from: at("production"),
                    name: EnvironmentName::parse(name).unwrap(),
                    copy: vec![node("web"), node("db")],
                    live: Vec::new(),
                    setup: Vec::new(),
                    keep: true,
                    fix: None,
                },
            )
            .unwrap();
        set(&store, &who, name, &[("web.branch", json!("main"))]);
        save(&store, &who, name);
    }
    // qa references db since.
    set(
        &store,
        &who,
        "qa",
        &[("web.env.DB_URL", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}"))],
    );
    save(&store, &who, "qa");
    store
        .write(
            &who,
            &SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: Some(true),
                start_from: Some(EnvironmentName::parse("production").unwrap()),
                copy: Some(vec![node("db")]),
                setup: None,
                remove_on_close: None,
                include_bots: None,
            },
        )
        .unwrap();
    pull(&store, &who, "main", "2026-09-29T10:00:00Z");
    remove_service(&store, &who, "pr-5", "db");

    let destinations: Vec<_> = on_merge(&store, &who)
        .destinations
        .into_iter()
        .map(|destination| {
            let rows: Vec<_> = destination
                .rows
                .into_iter()
                .map(|row| (row.at.to_string(), row.ride, row.refused_by.len()))
                .collect();
            (destination.name.to_string(), rows)
        })
        .collect();
    assert_eq!(
        destinations,
        [
            (
                "staging".to_owned(),
                vec![("db".to_owned(), Ride::Rides, 0)]
            ),
            ("qa".to_owned(), vec![("db".to_owned(), Ride::Rides, 1)]),
        ]
    );

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(!saved_has(&store, &who, "staging", "db"));
    assert!(saved_has(&store, &who, "qa", "db"));
    assert!(saved_has(&store, &who, "production", "db"));
}

#[test]
fn the_check_names_a_removal_apart_from_values_and_reads_neutral_while_it_is_in_use() {
    let (store, who) = shop_with_nodes();
    remove_db(&store, &who);
    store
        .write(
            &who,
            &ployz_store::RemoveVolume {
                environment: at("pr-5"),
                volume: ployz_store::VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    let removes = check(&store, &who).unwrap();
    assert_eq!(
        removes,
        OnMergeCheck {
            conclusion: OnMergeConclusion::Success,
            title: "2 config changes".into(),
            summary: "- `web · DB_URL`: ${{ db.PLOYZ_PRIVATE_DOMAIN }} → db.internal\n\
                      - Removes `db`\n\n\
                      - `volumes.data`: Kept"
                .into(),
            merged_into: None,
        }
    );

    // production's worker references db since pr-5 started: the removal waits.
    set(
        &store,
        &who,
        "production",
        &[("worker.env.DB", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}"))],
    );
    save(&store, &who, "production");
    let in_use = check(&store, &who).unwrap();
    assert_eq!(
        in_use,
        OnMergeCheck {
            conclusion: OnMergeConclusion::Neutral,
            title: "1 config change".into(),
            summary: "- `web · DB_URL`: ${{ db.PLOYZ_PRIVATE_DOMAIN }} → db.internal\n\n\
                      - `db`: In use · worker.env.DB\n\n\
                      - `volumes.data`: Kept"
                .into(),
            merged_into: None,
        }
    );
}

#[test]
fn the_check_says_a_removal_was_changed_there() {
    let (store, who) = shop_with_nodes();
    remove_db(&store, &who);
    set(&store, &who, "production", &[("db.env.POOL", json!("10"))]);
    save(&store, &who, "production");
    assert_eq!(
        check(&store, &who).unwrap().summary,
        "- `web · DB_URL`: ${{ db.PLOYZ_PRIVATE_DOMAIN }} → db.internal\n\n\
         ### Changed in production\n\n\
         - Removes `db` · Changed there\n\n\
         The merge sets these. Undo in Ployz History reverts it."
    );
}

/// Run Deployment `id` to its end, touching `services`.
fn run(store: &ConfigStore, id: &ployz_store::DeploymentId, services: &[&str]) {
    run_seeing(store, id, services, true);
}

/// Run Deployment `id` to its end, touching `services`, with a snapshot of every
/// Server when `complete`.
fn run_seeing(
    store: &ConfigStore,
    id: &ployz_store::DeploymentId,
    services: &[&str],
    complete: bool,
) {
    let claimed = store.claim(id, &runner()).unwrap();
    let operation = |index: usize| {
        json!({"type": "remove_container", "machine_id": "a".repeat(32),
               "container_id": format!("{index:x}").repeat(64)})
    };
    let preview: ployz_core::DeployPreview = serde_json::from_value(json!({
        "namespace": claimed.intent.namespace,
        "operations": services.iter().enumerate().map(|(index, name)| json!({
            "index": index, "machine_id": "a".repeat(32), "service_name": name,
            "operation": operation(index), "status": {"type": "pending"}
        })).collect::<Vec<_>>(),
        "warnings": [], "would_remove": [], "preserved_volumes": [],
        "prune_refusal": (!complete).then_some("incomplete_snapshot")
    }))
    .unwrap();
    store
        .record(id, &runner(), ployz_store::RunEvidence::Prepared(preview))
        .unwrap();
    let outcome: ployz_core::DeployOutcome<ployz_core::ExecutionError> =
        serde_json::from_value(json!({
            "type": "success",
            "completed": (0..services.len()).map(operation).collect::<Vec<_>>()
        }))
        .unwrap();
    store
        .record(
            id,
            &runner(),
            ployz_store::RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
}

/// Deploy `services` of production by hand.
fn deploy_by_hand(
    store: &ConfigStore,
    who: &Actor,
    services: &[&str],
) -> ployz_store::DeploymentId {
    let id = ployz_store::DeploymentId::parse(::uuid::Uuid::new_v4().to_string()).unwrap();
    store
        .write_trusted(
            who,
            &ployz_store::Admit::Deploy(ployz_store::Deploy {
                id: id.clone(),
                environment: at("production"),
                services: services
                    .iter()
                    .map(|service| ServiceName::parse(*service).unwrap())
                    .collect(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &Trusted::default(),
        )
        .unwrap();
    id
}

/// production's `db` and `worker` run.
fn running(store: &ConfigStore, who: &Actor) {
    let id = deploy_by_hand(store, who, &["db", "worker"]);
    run(store, &id, &["db", "worker"]);
}

/// What the one Deployment `automated` admitted tells its runner: the Services it
/// selects, the ones it tears down, and the Volumes it targets.
fn teardown(store: &ConfigStore, who: &Actor, automated: &Automated) -> [Vec<String>; 3] {
    let [admitted] = automated.admitted.as_slice() else {
        panic!("one Deployment: {automated:?}")
    };
    told(store, who, &admitted.deployment.id)
}

fn told(store: &ConfigStore, who: &Actor, id: &ployz_store::DeploymentId) -> [Vec<String>; 3] {
    let claimed = store.claim(id, &runner()).unwrap();
    let volumes = store
        .read(who, &ployz_store::DeploymentQuery { id: id.clone() })
        .unwrap()
        .nodes
        .into_iter()
        .filter(|node| matches!(node.node, ployz_store::DeployedNode::Volume { .. }))
        .map(|node| node.node.name().to_owned())
        .collect();
    [
        claimed
            .intent
            .options
            .selected
            .iter()
            .map(|attempt| attempt.name.to_string())
            .collect(),
        claimed
            .intent
            .options
            .removals
            .iter()
            .map(ToString::to_string)
            .collect(),
        volumes,
    ]
}

/// [`shop_with_nodes`] where nothing references `db`.
fn shop_with_lone_db() -> (ConfigStore, Actor) {
    shop_copying(
        |store, who| {
            production_nodes(store, who);
            unset(store, who, "production", "web.env.DB_URL");
            save(store, who, "production");
        },
        Some(vec![
            node("db"),
            node("volumes.data"),
            node("configs.sentry"),
        ]),
    )
}

/// Undo what pull request #5's merge landed in production, and save it.
fn undo_merge(store: &ConfigStore, who: &Actor) {
    let landed = history(store, who)
        .into_iter()
        .find(|revision| revision.message.as_deref() == Some("Merged #5"))
        .unwrap();
    let query = HistoryPreviewQuery {
        environment: at("production"),
        revision: landed.revision,
        action: HistoryAction::Undo,
        change: None,
    };
    let preview = store.read(who, &query).unwrap();
    store
        .write(
            who,
            &StageHistory {
                environment: at("production"),
                revision: query.revision,
                action: query.action,
                change: None,
                version: preview.version,
                accept_overwrite: false,
            },
        )
        .unwrap();
    save(store, who, "production");
}

const NOTHING: Vec<String> = Vec::new();

#[test]
fn a_merge_that_removes_a_running_service_tears_it_down_with_its_push() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    watching(&store, &who);
    remove_db(&store, &who);

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(landed.admitted.is_empty(), "{landed:?}");
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    // web for the DB_URL that landed, and db's teardown; never Volume data.
    assert_eq!(
        teardown(&store, &who, &pushed),
        [
            vec!["web".to_owned(), "db".to_owned()],
            vec!["db".to_owned()],
            NOTHING
        ]
    );
    // Once.
    let next = system(
        &store,
        &who,
        &push(Some(MERGE), LATER, Some(&["README.md"])),
    );
    assert!(next.admitted.is_empty(), "{next:?}");
}

#[test]
fn a_merge_that_only_removes_a_service_tears_it_down_alone_after_its_push() {
    let (store, who) = shop_with_lone_db();
    running(&store, &who);
    watching(&store, &who);
    remove_service(&store, &who, "pr-5", "db");
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(pushed.admitted.is_empty(), "{pushed:?}");

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    // A teardown of db alone, never a full Deploy pruning whatever else runs.
    assert_eq!(
        teardown(&store, &who, &landed),
        [vec!["db".to_owned()], vec!["db".to_owned()], NOTHING]
    );
}

#[test]
fn a_removal_waits_for_ci_with_what_the_merge_changed() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    set(
        &store,
        &who,
        "production",
        &[
            ("web.watchPaths", json!(["web/**"])),
            ("web.waitForCi", json!(true)),
        ],
    );
    system(&store, &who, &push(None, HEAD, None));
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(
        (pushed.waiting.len(), pushed.admitted.len()),
        (1, 0),
        "{pushed:?}"
    );

    let checked = passed(&store, &who, MERGE);
    assert_eq!(
        teardown(&store, &who, &checked),
        [
            vec!["web".to_owned(), "db".to_owned()],
            vec!["db".to_owned()],
            NOTHING
        ]
    );
}

#[test]
fn a_removed_service_that_never_ran_deploys_nothing() {
    let (store, who) = shop_with_lone_db();
    watching(&store, &who);
    remove_service(&store, &who, "pr-5", "db");

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(landed.admitted.is_empty(), "{landed:?}");
    assert!(pushed.admitted.is_empty(), "{pushed:?}");
    assert!(pushed.skipped.is_empty(), "{pushed:?}");
}

#[test]
fn undo_then_deploy_brings_a_torn_down_service_back() {
    let (store, who) = shop_with_lone_db();
    running(&store, &who);
    watching(&store, &who);
    remove_service(&store, &who, "pr-5", "db");
    system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    run(&store, &landed.admitted[0].deployment.id, &["db"]);

    undo_merge(&store, &who);
    let id = deploy_by_hand(&store, &who, &["db"]);
    assert_eq!(
        told(&store, &who, &id),
        [vec!["db".to_owned()], NOTHING, vec!["data".to_owned()]]
    );
}

#[test]
fn undo_before_the_push_keeps_the_service_running() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    watching(&store, &who);
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);

    undo_merge(&store, &who);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    // web still deploys for the merge; db is saved again, so nothing tears it down.
    assert_eq!(
        teardown(&store, &who, &pushed),
        [vec!["web".to_owned()], NOTHING, NOTHING]
    );
}

#[test]
fn a_manual_deploy_of_other_services_leaves_the_teardown_to_the_push() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    watching(&store, &who);
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let web = deploy_by_hand(&store, &who, &["web"]);
    run(&store, &web, &["web"]);

    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(
        teardown(&store, &who, &pushed),
        [vec!["db".to_owned()], vec!["db".to_owned()], NOTHING]
    );
}

#[test]
fn a_full_deploy_that_cant_see_every_server_leaves_the_teardown_to_the_push() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    watching(&store, &who);
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let full = deploy_by_hand(&store, &who, &[]);
    run_seeing(&store, &full, &["web", "worker"], false);

    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(
        teardown(&store, &who, &pushed),
        [vec!["db".to_owned()], vec!["db".to_owned()], NOTHING]
    );
}

#[test]
fn a_full_deploy_over_a_queued_teardown_that_cant_see_every_server_is_not_applied() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    watching(&store, &who);
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(pushed.admitted.len(), 1, "{pushed:?}");
    let full = deploy_by_hand(&store, &who, &[]);
    run_seeing(&store, &full, &["web", "worker"], false);

    assert_ne!(
        status(&store, &who, &full),
        ployz_store::DeploymentStatus::Applied
    );
    retry(&store, &who, &full).unwrap();
}

/// The Services production's Applied State runs.
fn applied(store: &ConfigStore, who: &Actor) -> Vec<String> {
    store
        .read(
            who,
            &ployz_store::NamespaceQuery {
                environment: at("production"),
            },
        )
        .unwrap()
        .services
        .into_keys()
        .map(|name| name.to_string())
        .collect()
}

#[test]
fn a_targeted_deploy_over_a_full_one_over_a_teardown_keeps_it() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    watching(&store, &who);
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let [queued] = pushed.admitted.as_slice() else {
        panic!("one Deployment: {pushed:?}")
    };
    let full = deploy_by_hand(&store, &who, &[]);
    let worker = deploy_by_hand(&store, &who, &["worker"]);

    for superseded in [&queued.deployment.id, &full] {
        assert_eq!(
            status(&store, &who, superseded),
            ployz_store::DeploymentStatus::Superseded
        );
    }
    let [_, removals, _] = told(&store, &who, &worker);
    assert_eq!(removals, vec!["db".to_owned()]);
    run(&store, &worker, &["worker", "db"]);
    assert_eq!(
        status(&store, &who, &worker),
        ployz_store::DeploymentStatus::Applied
    );
    assert!(
        !applied(&store, &who).contains(&"db".to_owned()),
        "{:?}",
        applied(&store, &who)
    );
}

#[test]
fn a_targeted_deploy_over_a_full_retry_over_a_teardown_keeps_it() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    watching(&store, &who);
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let full = deploy_by_hand(&store, &who, &[]);
    cancel(&store, &who, &full);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let [queued] = pushed.admitted.as_slice() else {
        panic!("one Deployment: {pushed:?}")
    };
    let retried = retry(&store, &who, &full).unwrap();
    let worker = deploy_by_hand(&store, &who, &["worker"]);

    for superseded in [&queued.deployment.id, &retried] {
        assert_eq!(
            status(&store, &who, superseded),
            ployz_store::DeploymentStatus::Superseded
        );
    }
    let [_, removals, _] = told(&store, &who, &worker);
    assert_eq!(removals, vec!["db".to_owned()]);
    run(&store, &worker, &["worker", "db"]);
    assert_eq!(
        status(&store, &who, &worker),
        ployz_store::DeploymentStatus::Applied
    );
    assert!(
        !applied(&store, &who).contains(&"db".to_owned()),
        "{:?}",
        applied(&store, &who)
    );
}

/// Run Deployment `id` of `service` to a failure on Server `a…`.
fn fail(store: &ConfigStore, id: &ployz_store::DeploymentId, service: &str) {
    let claimed = store.claim(id, &runner()).unwrap();
    let operation = json!({"type": "remove_container", "machine_id": "a".repeat(32),
                           "container_id": "0".repeat(64)});
    let error = json!({
        "type": "machine", "action": "RemoveContainer",
        "error": {"code": "internal", "message": "busy", "details": {}}
    });
    let preview: ployz_core::DeployPreview = serde_json::from_value(json!({
        "namespace": claimed.intent.namespace,
        "operations": [{
            "index": 0, "machine_id": "a".repeat(32), "service_name": service,
            "operation": operation, "status": {"type": "pending"}
        }],
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(id, &runner(), ployz_store::RunEvidence::Prepared(preview))
        .unwrap();
    let outcome: ployz_core::DeployOutcome<ployz_core::ExecutionError> =
        serde_json::from_value(json!({
            "type": "failed", "completed": [], "unexecuted": [],
            "failed": {"type": "operation", "operation": operation, "error": error}
        }))
        .unwrap();
    store
        .record(
            id,
            &runner(),
            ployz_store::RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
}

#[test]
fn a_full_deploy_over_a_teardown_of_a_service_that_never_applied_cant_apply_blind() {
    let (store, who) = shop_with_nodes();
    let worker = deploy_by_hand(&store, &who, &["worker"]);
    run(&store, &worker, &["worker"]);
    let db = deploy_by_hand(&store, &who, &["db"]);
    fail(&store, &db, "db");
    assert_eq!(
        status(&store, &who, &db),
        ployz_store::DeploymentStatus::Failed
    );
    assert!(!applied(&store, &who).contains(&"db".to_owned()));
    watching(&store, &who);
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(pushed.admitted.len(), 1, "{pushed:?}");
    let full = deploy_by_hand(&store, &who, &[]);
    run_seeing(&store, &full, &["web", "worker"], false);

    assert_eq!(
        status(&store, &who, &full),
        ployz_store::DeploymentStatus::Failed
    );
    let retried = retry(&store, &who, &full).unwrap();
    let nodes = store
        .read(
            &who,
            &ployz_store::DeploymentQuery {
                id: retried.clone(),
            },
        )
        .unwrap()
        .nodes;
    assert!(
        nodes.iter().any(|node| node.node.name() == "db"),
        "{nodes:?}"
    );
    run(&store, &retried, &["web", "worker", "db"]);
    assert_eq!(
        status(&store, &who, &retried),
        ployz_store::DeploymentStatus::Applied
    );
}

#[test]
fn a_full_deploy_that_tore_the_service_down_leaves_the_push_nothing() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    watching(&store, &who);
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let full = deploy_by_hand(&store, &who, &[]);
    run(&store, &full, &["web", "worker", "db"]);

    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(pushed.admitted.is_empty(), "{pushed:?}");
}

#[test]
fn a_removal_supersedes_a_queued_deploy_that_would_run_the_service() {
    let (store, who) = shop_with_lone_db();
    watching(&store, &who);
    let queued = deploy_by_hand(&store, &who, &["db"]);
    remove_service(&store, &who, "pr-5", "db");
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);

    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(
        teardown(&store, &who, &pushed),
        [vec!["db".to_owned()], vec!["db".to_owned()], NOTHING]
    );
    assert!(store.claim(&queued, &runner()).is_err());
}

#[test]
fn a_removal_tears_down_what_a_deploy_claimed_before_its_push() {
    let (store, who) = shop_with_lone_db();
    watching(&store, &who);
    let queued = deploy_by_hand(&store, &who, &["db"]);
    remove_service(&store, &who, "pr-5", "db");
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    store.claim(&queued, &runner()).unwrap();

    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(
        teardown(&store, &who, &pushed),
        [vec!["db".to_owned()], vec!["db".to_owned()], NOTHING]
    );
}

fn cancel(store: &ConfigStore, who: &Actor, id: &ployz_store::DeploymentId) {
    store
        .write(
            who,
            &ployz_store::Cancel {
                deployment: id.clone(),
            },
        )
        .unwrap();
}

fn retry(
    store: &ConfigStore,
    who: &Actor,
    source: &ployz_store::DeploymentId,
) -> Result<ployz_store::DeploymentId, ployz_core::RpcError> {
    let id = ployz_store::DeploymentId::parse(::uuid::Uuid::new_v4().to_string()).unwrap();
    store.write_trusted(
        who,
        &ployz_store::Admit::Retry(ployz_store::Retry {
            id: id.clone(),
            deployment: source.clone(),
        }),
        &Trusted::default(),
    )?;
    Ok(id)
}

#[test]
fn a_retry_takes_over_the_teardown_it_supersedes() {
    let (store, who) = shop_with_nodes();
    running(&store, &who);
    watching(&store, &who);
    remove_db(&store, &who);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let worker = deploy_by_hand(&store, &who, &["worker"]);
    cancel(&store, &who, &worker);
    system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));

    let retried = retry(&store, &who, &worker).unwrap();
    let [_, removals, _] = told(&store, &who, &retried);
    assert_eq!(removals, vec!["db".to_owned()]);
}

fn status(
    store: &ConfigStore,
    who: &Actor,
    id: &ployz_store::DeploymentId,
) -> ployz_store::DeploymentStatus {
    store
        .read(who, &ployz_store::DeploymentQuery { id: id.clone() })
        .unwrap()
        .deployment
        .status
}

#[test]
fn a_retry_from_before_the_merge_leaves_the_queued_teardown() {
    for target in ["db", "worker"] {
        let (store, who) = shop_with_nodes();
        running(&store, &who);
        watching(&store, &who);
        let earlier = deploy_by_hand(&store, &who, &[target]);
        cancel(&store, &who, &earlier);
        remove_db(&store, &who);
        merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
        let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
        let [queued] = pushed.admitted.as_slice() else {
            panic!("one Deployment: {pushed:?}")
        };

        let refused = retry(&store, &who, &earlier).unwrap_err();
        assert_eq!(
            refused.code,
            RpcErrorCode::Conflict,
            "{target}: {refused:?}"
        );
        assert_eq!(
            status(&store, &who, &queued.deployment.id),
            ployz_store::DeploymentStatus::Queued,
            "{target}"
        );
    }
}

#[test]
fn a_retry_after_undo_never_tears_down_what_saved_holds_again() {
    let (store, who) = shop_with_lone_db();
    running(&store, &who);
    watching(&store, &who);
    remove_service(&store, &who, "pr-5", "db");
    system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let teardown = landed.admitted[0].deployment.id.clone();
    cancel(&store, &who, &teardown);

    undo_merge(&store, &who);
    let refused = retry(&store, &who, &teardown).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
}

/// Production runs `db` with Config `dbconf` that only it mounts, and pull request
/// #5, which removes both, merged.
fn shop_merging_db_with_its_config() -> (ConfigStore, Actor) {
    let (store, who) = shop_copying(
        |store, who| {
            production_nodes(store, who);
            config(store, who, 26, "dbconf", &["db"]);
            save(store, who, "production");
        },
        Some(vec![
            node("db"),
            node("volumes.data"),
            node("configs.sentry"),
            node("configs.dbconf"),
        ]),
    );
    let full = deploy_by_hand(&store, &who, &[]);
    run(&store, &full, &["web", "worker", "db"]);
    watching(&store, &who);
    remove_db(&store, &who);
    store
        .write(
            &who,
            &ployz_store::DeleteConfig {
                environment: at("pr-5"),
                config: ployz_core::ConfigName::parse("dbconf").unwrap(),
            },
        )
        .unwrap();
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    (store, who)
}

/// Each node of Deployment `id` by name, with its outcome.
fn outcomes(
    store: &ConfigStore,
    who: &Actor,
    id: &ployz_store::DeploymentId,
) -> Vec<(String, ployz_store::NodeStatus)> {
    store
        .read(who, &ployz_store::DeploymentQuery { id: id.clone() })
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| (node.node.name().to_owned(), node.outcome))
        .collect()
}

#[test]
fn a_removed_config_reads_removed_only_once_the_service_that_mounted_it_is_gone() {
    for (touched, complete, outcome) in [
        (&["web"][..], false, ployz_store::NodeStatus::NotAttempted),
        (&["web", "db"][..], true, ployz_store::NodeStatus::Removed),
    ] {
        let (store, who) = shop_merging_db_with_its_config();
        let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
        let [admitted] = pushed.admitted.as_slice() else {
            panic!("one Deployment: {pushed:?}")
        };
        let id = &admitted.deployment.id;
        run_seeing(&store, id, touched, complete);

        let ended = outcomes(&store, &who, id);
        assert!(ended.contains(&("db".to_owned(), outcome)), "{ended:?}");
        assert!(ended.contains(&("dbconf".to_owned(), outcome)), "{ended:?}");
    }
}

#[test]
fn a_deploy_that_takes_over_a_teardown_takes_the_config_it_removes_too() {
    type Successor = fn(&ConfigStore, &Actor) -> ployz_store::DeploymentId;
    let cases: [(&str, Successor); 3] = [
        ("worker over the teardown", |store, who| {
            system(store, who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
            deploy_by_hand(store, who, &["worker"])
        }),
        (
            "worker over a full Deploy over the teardown",
            |store, who| {
                system(store, who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
                deploy_by_hand(store, who, &[]);
                deploy_by_hand(store, who, &["worker"])
            },
        ),
        ("a worker Retry over the teardown", |store, who| {
            let worker = deploy_by_hand(store, who, &["worker"]);
            cancel(store, who, &worker);
            system(store, who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
            retry(store, who, &worker).unwrap()
        }),
    ];
    for (case, successor) in cases {
        let (store, who) = shop_merging_db_with_its_config();
        let id = successor(&store, &who);
        let names: Vec<String> = outcomes(&store, &who, &id)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert!(names.contains(&"dbconf".to_owned()), "{case}: {names:?}");

        run(&store, &id, &["worker", "db"]);
        let ended = outcomes(&store, &who, &id);
        for removed in ["db", "dbconf"] {
            assert!(
                ended.contains(&(removed.to_owned(), ployz_store::NodeStatus::Removed)),
                "{case}: {ended:?}"
            );
        }
        assert!(!applied(&store, &who).contains(&"db".to_owned()), "{case}");
        let changes: Vec<String> = store
            .read(
                &who,
                &ployz_store::PlanQuery {
                    environment: at("production"),
                    services: Vec::new(),
                },
            )
            .unwrap()
            .changes
            .into_iter()
            .map(|change| change.name)
            .collect();
        assert!(
            !changes.contains(&"dbconf".to_owned()),
            "{case}: {changes:?}"
        );
    }
}

#[test]
fn a_merge_that_removes_a_mounted_config_takes_it_out_with_the_service() {
    let (store, who) = shop_with_nodes();
    let full = deploy_by_hand(&store, &who, &[]);
    run(&store, &full, &["web", "worker", "db"]);
    watching(&store, &who);
    store
        .write(
            &who,
            &ployz_store::DeleteConfig {
                environment: at("pr-5"),
                config: ployz_core::ConfigName::parse("sentry").unwrap(),
            },
        )
        .unwrap();
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);

    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    let [admitted] = pushed.admitted.as_slice() else {
        panic!("one Deployment: {pushed:?}")
    };
    let nodes: Vec<String> = store
        .read(
            &who,
            &ployz_store::DeploymentQuery {
                id: admitted.deployment.id.clone(),
            },
        )
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| node.node.name().to_owned())
        .collect();
    assert_eq!(nodes, vec!["web".to_owned(), "sentry".to_owned()]);
}
