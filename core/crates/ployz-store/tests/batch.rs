#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! A Batch: creates and edits in one transaction, all or none, through the Store's
//! interface only, on SQLite and on Postgres (see `backend`).

use ployz_core::config::VolumeKind;
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Batch, BatchCommand, Batched, Change, ConfigStore, CreateProject, CreateService,
    CreateVolume, Edit, EnvironmentId, EnvironmentQuery, EnvironmentRef, Mount, OrganizationId,
    ProjectId, ProjectName, ServiceLineageId, ServicesQuery, SettingPath, VolumeId, VolumeName,
    VolumesQuery, Written,
};
use serde_json::{Value, json};

mod backend;

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

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
    (store, who)
}

/// Service `db`, Volume `db-data` mounted into it, and `env` patched onto it.
fn database(env: Value) -> Batch {
    Batch {
        environment: EnvironmentRef::default(),
        commands: vec![
            BatchCommand::CreateService(CreateService {
                id: ServiceLineageId::parse(uuid(3)).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("db").unwrap(),
                image: Some("postgres:18".into()),
                template: None,
            }),
            BatchCommand::CreateVolume(CreateVolume {
                shared_writes: false,
                id: VolumeId::parse(uuid(4)).unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("db-data").unwrap(),
                storage: VolumeKind::Docker {},
                mounts: vec![Mount {
                    service: ServiceName::parse("db").unwrap(),
                    path: "/var/lib/postgresql/data".into(),
                }],
            }),
            BatchCommand::Edit(Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Patch {
                    path: SettingPath::parse("db").unwrap(),
                    value: json!({ "env": env }),
                }],
            }),
        ],
        expect: None,
    }
}

fn services(store: &ConfigStore, who: &Actor) -> Vec<String> {
    let listed = store.read(who, &ServicesQuery::default()).unwrap();
    listed
        .services
        .into_iter()
        .map(|listing| listing.service.name.to_string())
        .collect()
}

fn volumes(store: &ConfigStore, who: &Actor) -> usize {
    store
        .read(who, &VolumesQuery::default())
        .unwrap()
        .volumes
        .len()
}

#[test]
fn a_batch_creates_a_mounted_configured_service_at_once() {
    let (store, who) = shop();
    let batch = database(json!({ "PGDATA": "/var/lib/postgresql/data/pgdata" }));
    let written = store.write(&who, &batch).unwrap().results;
    let wire = serde_json::to_value(Written::Batch(Batched {
        results: written.clone(),
    }))
    .unwrap();
    assert_eq!(wire["written"], "batch");
    assert!(matches!(
        written.as_slice(),
        [Written::Service(_), Written::Volume(_), Written::Edited(_)]
    ));

    let values = store
        .read(
            &who,
            &EnvironmentQuery {
                environment: EnvironmentRef::default(),
                path: Some(SettingPath::parse("db").unwrap()),
                all: false,
            },
        )
        .unwrap()
        .values
        .unwrap();
    assert_eq!(
        values["mounts"],
        json!({ "db-data": "/var/lib/postgresql/data" })
    );
    assert_eq!(
        values["env"]["PGDATA"],
        json!("/var/lib/postgresql/data/pgdata")
    );

    // A retry replays the creates instead of refusing them as duplicates, and its
    // edit changes nothing.
    let retried = store.write(&who, &batch).unwrap().results;
    assert_eq!(retried[..2], written[..2]);
    assert_eq!(services(&store, &who), ["db"]);
}

#[test]
fn a_failing_command_leaves_nothing_of_the_batch() {
    let (store, who) = shop();
    let error = store
        .write(&who, &database(json!({ "PGDATA": { "not": "a value" } })))
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert!(services(&store, &who).is_empty());
    assert_eq!(volumes(&store, &who), 0);
}

#[test]
fn a_batch_writes_one_environment_and_refuses_a_command_naming_another() {
    let (store, who) = shop();
    let mut batch = database(json!({}));
    if let BatchCommand::Edit(edit) = &mut batch.commands[2] {
        edit.environment.environment =
            Some(ployz_store::EnvironmentName::parse("staging").unwrap());
    }
    let error = store.write(&who, &batch).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert!(services(&store, &who).is_empty());
}

#[test]
fn a_batch_is_refused_whole_unless_working_state_is_where_it_expects() {
    let (store, who) = shop();
    let mut batch = database(json!({}));
    batch.expect = Some(ployz_store::Revision(9));
    let error = store.write(&who, &batch).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
    assert!(services(&store, &who).is_empty());
}
