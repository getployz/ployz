#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! A Service's authored lifecycle: empty Services, rename, staged removal, and the
//! `service ls`/`service inspect` views, through the Store's interface only, on
//! SQLite and on Postgres (see `backend`).

use ployz_core::config::ReviewLifecycleKind;
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Change, Command, ConfigStore, CreateProject, CreateService, DiffQuery, Edit,
    EnvironmentId, EnvironmentRef, OrganizationId, ProjectId, ProjectName, RemoveService,
    RenameService, ServiceLineageId, ServiceQuery, ServicesQuery, SettingPath, SourceKind, Written,
};
use serde_json::json;

mod backend;

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn name(name: &str) -> ServiceName {
    ServiceName::parse(name).unwrap()
}

/// A store with Project `shop`, an image Service `web` and an empty Service `worker`.
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
    for (n, service, image) in [(3, "web", Some("nginx:1")), (4, "worker", None)] {
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(uuid(n)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: name(service),
                    image: image.map(Into::into),
                    template: None,
                },
            )
            .unwrap();
    }
    (store, who)
}

fn rename(store: &ConfigStore, who: &Actor, from: &str, to: &str) -> Result<Written, RpcErrorCode> {
    store
        .write(
            who,
            &Command::RenameService(RenameService {
                environment: EnvironmentRef::default(),
                service: name(from),
                name: name(to),
            }),
        )
        .map_err(|error| error.code)
}

fn listed(store: &ConfigStore, who: &Actor) -> Vec<(String, String, SourceKind)> {
    store
        .read(who, &ServicesQuery::default())
        .unwrap()
        .services
        .into_iter()
        .map(|listing| {
            (
                listing.service.name.to_string(),
                listing.service.private_dns.to_string(),
                listing.source,
            )
        })
        .collect()
}

fn inspect(store: &ConfigStore, who: &Actor, service: &str) -> ployz_store::ServiceView {
    store
        .read(
            who,
            &ServiceQuery {
                environment: EnvironmentRef::default(),
                service: name(service),
            },
        )
        .unwrap()
}

#[test]
fn an_empty_service_lists_without_a_source_until_it_gets_an_image() {
    let (store, who) = shop();
    assert_eq!(
        listed(&store, &who),
        [
            ("web".into(), "web".into(), SourceKind::Image),
            ("worker".into(), "worker".into(), SourceKind::Empty),
        ]
    );
    let worker = inspect(&store, &who, "worker");
    assert_eq!(worker.service.change, Some(ReviewLifecycleKind::Create));
    assert_eq!(worker.lineage.as_str(), uuid(4));
    assert!(!worker.values.contains_key("image"));

    store
        .write(
            &who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse("worker.image").unwrap(),
                    value: json!("busybox:1"),
                }],
            },
        )
        .unwrap();
    let worker = inspect(&store, &who, "worker");
    assert_eq!(worker.service.source, SourceKind::Image);
    assert_eq!(worker.values["image"], json!("busybox:1"));
}

#[test]
fn a_rename_keeps_the_private_dns_name() {
    let (store, who) = shop();
    let Ok(Written::ServiceRenamed(renamed)) = rename(&store, &who, "web", "frontend") else {
        panic!("a rename writes the renamed Service")
    };
    assert_eq!(
        (
            renamed.service.name.as_str(),
            renamed.service.private_dns.as_str()
        ),
        ("frontend", "web")
    );
    assert_eq!(renamed.staged, [SettingPath::parse("frontend").unwrap()]);
    assert_eq!(
        inspect(&store, &who, "frontend").values["image"],
        json!("nginx:1")
    );
    assert_eq!(
        store
            .read(
                &who,
                &ServiceQuery {
                    environment: EnvironmentRef::default(),
                    service: name("web"),
                }
            )
            .unwrap_err()
            .code,
        RpcErrorCode::NotFound
    );

    // `web` still names the Service's Private DNS, and `worker` is taken outright.
    assert_eq!(
        rename(&store, &who, "worker", "web").unwrap_err(),
        RpcErrorCode::Conflict
    );
    assert_eq!(
        rename(&store, &who, "frontend", "worker").unwrap_err(),
        RpcErrorCode::Conflict
    );
    let web = CreateService {
        id: ServiceLineageId::parse(uuid(5)).unwrap(),
        environment: EnvironmentRef::default(),
        name: name("web"),
        image: None,
        template: None,
    };
    assert_eq!(
        store.write(&who, &web).unwrap_err().code,
        RpcErrorCode::Conflict
    );

    // Renaming back to its Private DNS name is allowed; renaming to itself changes nothing.
    let revision = store
        .read(&who, &ServicesQuery::default())
        .unwrap()
        .environment
        .revision;
    let Ok(Written::ServiceRenamed(same)) = rename(&store, &who, "frontend", "frontend") else {
        panic!("a rename writes the renamed Service")
    };
    assert!(same.staged.is_empty());
    assert_eq!(same.environment.revision, revision);
    rename(&store, &who, "frontend", "web").unwrap();
    assert_eq!(listed(&store, &who)[0].0, "web");
}

#[test]
fn removing_a_new_service_drops_it_from_working_state() {
    let (store, who) = shop();
    let removed = store
        .write(
            &who,
            &RemoveService {
                environment: EnvironmentRef::default(),
                service: name("worker"),
            },
        )
        .unwrap();
    assert_eq!(removed.staged, [SettingPath::parse("worker").unwrap()]);
    assert_eq!(
        listed(&store, &who),
        [("web".into(), "web".into(), SourceKind::Image)]
    );
    // Nothing was ever deployed, so no removal is staged either.
    let changes = store.read(&who, &DiffQuery::default()).unwrap().changes;
    assert_eq!(
        changes
            .iter()
            .map(|change| &change.name)
            .collect::<Vec<_>>(),
        ["web"]
    );
    // Another Service may take the freed name.
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(6)).unwrap(),
                environment: EnvironmentRef::default(),
                name: name("worker"),
                image: None,
                template: None,
            },
        )
        .unwrap();
    let missing = store
        .write(
            &who,
            &RemoveService {
                environment: EnvironmentRef::default(),
                service: name("wbe"),
            },
        )
        .unwrap_err();
    assert_eq!(missing.code, RpcErrorCode::NotFound);
    assert_eq!(missing.details["did_you_mean"], json!("web"));
}

#[test]
fn a_service_keeps_the_template_it_was_created_from_until_unset() {
    let (store, who) = shop();
    let postgres = json!({ "id": "postgres", "version": 1 });
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(5)).unwrap(),
                environment: EnvironmentRef::default(),
                name: name("db"),
                image: Some("postgres:18".into()),
                template: serde_json::from_value(postgres.clone()).unwrap(),
            },
        )
        .unwrap();
    let edit = |change: Change| {
        store.write(
            &who,
            &Command::Edit(Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![change],
            }),
        )
    };
    let template = || {
        let listed = store.read(&who, &ServicesQuery::default()).unwrap();
        let db = listed
            .services
            .into_iter()
            .find(|listing| listing.service.name.as_str() == "db")
            .unwrap();
        serde_json::to_value(db.template).unwrap()
    };
    assert_eq!(template(), postgres);
    // Other edits leave it be.
    edit(Change::Set {
        path: SettingPath::parse("db.replicas").unwrap(),
        value: json!(2),
    })
    .unwrap();
    assert_eq!(template(), postgres);
    store
        .write(
            &who,
            &Command::Publish(ployz_store::Publish {
                environment: EnvironmentRef::default(),
                version: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
    // A template is a tag: changed, it takes effect at once and is no diff row.
    let changed = edit(Change::Set {
        path: SettingPath::parse("db.template").unwrap(),
        value: json!({ "id": "postgres", "version": 2 }),
    })
    .unwrap();
    let Written::Edited(changed) = changed else {
        panic!("an edit writes Edited")
    };
    assert!(changed.staged.is_empty());
    assert_eq!(template(), json!({ "id": "postgres", "version": 2 }));
    edit(Change::Set {
        path: SettingPath::parse("db.env.MODE").unwrap(),
        value: json!("fast"),
    })
    .unwrap();
    // Every row says whether `discard` takes its path.
    let diff = store.read(&who, &DiffQuery::default()).unwrap();
    let rows: Vec<(&str, bool)> = diff
        .changes
        .iter()
        .flat_map(|change| &change.settings)
        .map(|row| (row.path.as_str(), row.can_restore))
        .collect();
    assert!(rows.contains(&("db.env.MODE", true)), "{rows:?}");
    assert!(
        !rows.iter().any(|(path, _)| *path == "db.template"),
        "{rows:?}"
    );
    let refused = edit(Change::Set {
        path: SettingPath::parse("db.template").unwrap(),
        value: json!({ "id": "Not A Label", "version": 1 }),
    })
    .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    edit(Change::Unset {
        path: SettingPath::parse("db.template").unwrap(),
    })
    .unwrap();
    assert_eq!(template(), json!(null));
}
