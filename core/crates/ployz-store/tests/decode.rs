//! Refusals for commands and queries that do not decode: each names where the input
//! went wrong and what fits there, and never echoes a value the caller sent.

use ployz_core::RpcErrorCode;
use ployz_store::{
    Actor, Change, Command, CreateProject, CreateService, Edit, EnvironmentId, EnvironmentRef,
    OrganizationId, ProjectId, ProjectName, Query, ServiceLineageId, SettingPath,
};
use serde_json::{Value, json};

mod backend;
use backend::open;

const MARKER: &str = "MARKER_9f3c";
const VOLUME: &str = "0b9e7c4e-1f2a-4c3b-9d8e-7f6a5b4c3d2e";

#[test]
fn a_malformed_command_names_its_path_never_its_values() {
    let cases = [
        (
            json!({ "command": "create_volume", "id": VOLUME, "name": "data", "shared_writes": MARKER }),
            "shared_writes: invalid type, expected a boolean",
        ),
        (
            json!({ "command": "create_volume", "id": VOLUME, "name": MARKER }),
            "name: ",
        ),
        (
            json!({ "command": "create_volume", "id": MARKER, "name": "data" }),
            "id: ",
        ),
        (
            json!({ "command": "create_volume", "id": VOLUME, "name": "data", "storage": MARKER }),
            "storage: invalid type",
        ),
        (
            json!({ "command": MARKER, "id": VOLUME }),
            "command: unknown variant, expected one of `create_project`",
        ),
        (
            json!({ "command": "create_volume", "name": "data" }),
            "missing field `id`",
        ),
        (
            json!({ "command": "create_volume", "id": VOLUME, "name": "data",
                    "mounts": [{ "service": "web", "path": "/data", MARKER: MARKER }] }),
            "mounts[0]: unknown field, expected `service` or `path`",
        ),
        (
            json!({ "command": "create_volume", "id": VOLUME, "name": "data",
                    "mounts": [{ "service": MARKER, "path": "/data" }] }),
            "mounts[0].service: invalid Service Name, expected",
        ),
        (
            json!({ "command": "create_volume", "id": VOLUME, "name": "data",
                    "mounts": [{ "service": "web", "path": [MARKER] }] }),
            "mounts[0].path: invalid type, expected a string",
        ),
        (
            json!({ "command": "edit", "changes": [{ "op": MARKER, "path": MARKER }] }),
            "changes[0].op: unknown variant, expected one of `set`, `unset`, `patch`",
        ),
        (
            json!({ "command": "rename_service", MARKER: MARKER }),
            "unknown field, expected one of `environment`, `service`, `name`",
        ),
    ];
    for (command, reason) in cases {
        let error = Command::decode(command.clone()).unwrap_err().to_string();
        assert!(error.starts_with(reason), "{command}: {error}");
        assert!(!error.contains(MARKER), "{command} echoed: {error}");
    }
}

#[test]
fn a_value_that_mimics_serdes_wording_is_not_echoed() {
    for text in [
        format!("a, expected {MARKER}"),
        format!("a`, expected {MARKER}"),
        format!("a\": {MARKER}"),
    ] {
        for command in [
            json!({ "command": "create_volume", "id": VOLUME, "name": "data", "shared_writes": text }),
            json!({ "command": "create_volume", "id": VOLUME, "name": text }),
            json!({ "command": text }),
            json!({ "command": "edit", "changes": [{ "op": text }] }),
        ] {
            let error = Command::decode(command.clone()).unwrap_err().to_string();
            assert!(!error.contains(MARKER), "{command} echoed: {error}");
        }
    }
}

#[test]
fn a_well_formed_command_decodes_as_serde_reads_it() {
    let command = json!({ "command": "create_volume", "id": VOLUME, "name": "data",
                          "mounts": [{ "service": "web", "path": "/data" }] });
    assert_eq!(
        Command::decode(command.clone()).unwrap(),
        serde_json::from_value::<Command>(command).unwrap()
    );
}

#[test]
fn a_malformed_query_names_its_path_never_its_values() {
    let error = Query::decode(json!({ "query": "environment", "all": MARKER }))
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with("all: invalid type, expected a boolean"),
        "{error}"
    );
    assert!(!error.contains(MARKER), "{error}");
}

#[test]
fn a_setting_of_the_wrong_shape_names_its_reason_never_its_value() {
    let store = open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid("project")).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(uuid("env")).unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(uuid("web")).unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_core::ServiceName::parse("web").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    for (path, value, reason) in [
        (
            "web.image",
            json!([MARKER]),
            "image: invalid type, expected a string",
        ),
        (
            "web.image",
            json!({ MARKER: MARKER }),
            "image: invalid type, expected a string",
        ),
        (
            "web.buildMethod",
            json!(MARKER),
            "buildMethod: unknown variant, expected one of `dockerfile`",
        ),
    ] {
        let error = store
            .write(
                &who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: vec![Change::Set {
                        path: SettingPath::parse(path).unwrap(),
                        value: value.clone(),
                    }],
                },
            )
            .unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument);
        assert!(error.message.starts_with(reason), "{value}: {error:?}");
        let refusal: Value = serde_json::to_value(&error).unwrap();
        assert!(
            !refusal.to_string().contains(MARKER),
            "{value} echoed: {refusal}"
        );
    }
}

fn uuid(label: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, label.as_bytes()).to_string()
}
