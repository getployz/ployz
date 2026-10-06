//! Current SDK evidence admission and complete per-Service operation outcomes.

#![expect(
    clippy::indexing_slicing,
    reason = "Test JSON fixtures assert public response fields."
)]

use ployz_core::config::{parse_runtime_preview, project_runtime_outcome};
use ployz_core::{ConfigSpec, DeployOperation, DeployPreview, ResolvedServiceSpec};
use serde_json::{Value, json};

fn operation(id: &str) -> Value {
    json!({"type":"remove_container", "machine_id":"a".repeat(32), "container_id":id.repeat(64)})
}

#[test]
fn current_outcome_confirms_only_services_with_all_operations_completed() {
    let api = operation("a");
    let worker = operation("b");
    let replica = operation("c");
    let preview = |operations: Vec<(&str, Value)>| {
        json!({
            "namespace":"production", "operations":operations.into_iter().enumerate().map(|(index, (service, operation))|
                json!({"index":index,"machine_id":"a".repeat(32),"service_name":service,"operation":operation,"status":{"type":"pending"}})
            ).collect::<Vec<_>>(), "warnings":[]
        })
    };
    let project = |preview: Value, outcome: Value| {
        project_runtime_outcome(
            &parse_runtime_preview(preview).unwrap(),
            json!({"version":1,"outcome":outcome}),
        )
        .map(|projection| serde_json::to_value(projection).unwrap())
    };
    let partial = json!({"type":"failed","completed":[api.clone()],
        "failed":{"type":"operation","operation":worker.clone(),"error":{"type":"cancelled"}},"unexecuted":[]});
    let result = project(
        preview(vec![("api", api.clone()), ("worker", worker.clone())]),
        partial.clone(),
    )
    .unwrap();
    assert_eq!(result["confirmedServices"], json!(["api"]));
    assert_eq!(result["failedServices"], json!(["worker"]));
    assert_eq!(result["unattemptedServices"], json!([]));
    let mut incomplete = partial;
    incomplete["unexecuted"] = json!([replica.clone()]);
    assert_eq!(
        project(
            preview(vec![
                ("api", api.clone()),
                ("worker", worker),
                ("api", replica)
            ]),
            incomplete
        )
        .unwrap()["confirmedServices"],
        json!([])
    );
    let success = json!({"type":"success","completed":[api.clone()]});
    assert_eq!(
        project(preview(vec![("api", api)]), success).unwrap()["summary"],
        json!({"type":"success","completed":1})
    );
    let mut unknown_field = json!({"type":"success","completed":[operation("a")]});
    unknown_field["completed"][0]["unexpected"] = json!("must-not-be-echoed");
    let error = project(preview(vec![("api", operation("a"))]), unknown_field).unwrap_err();
    assert!(!error.to_string().contains("must-not-be-echoed"));
    assert!(
        project(
            preview(vec![("api", operation("a"))]),
            json!({"type":"success","completed":[]})
        )
        .is_err()
    );
    let preflight = json!({"type":"failed","completed":[],
        "failed":{"type":"operation","operation":operation("b"),"error":{"type":"cancelled"}},"unexecuted":[operation("a")]});
    let stopped = project(
        preview(vec![("api", operation("a")), ("worker", operation("b"))]),
        preflight,
    )
    .unwrap();
    assert_eq!(stopped["confirmedServices"], json!([]));
    assert_eq!(stopped["failedServices"], json!(["worker"]));
    assert_eq!(stopped["unattemptedServices"], json!(["api"]));
    let invalid = project_runtime_outcome(
        &parse_runtime_preview(preview(vec![])).unwrap(),
        json!({"version":2,"outcome":{"type":"success","completed":[]}}),
    );
    assert!(invalid.is_err());
}

fn secret_spec() -> Value {
    json!({
        "service_id":"c".repeat(32),
        "name":"api",
        "mode":{"mode":"replicated","replicas":1},
        "container":{
            "image":"api:1",
            "pull_policy":"missing",
            "environment":{"TOKEN":"service-secret"},
            "config_mounts":[{"config_name":"settings","target":"/etc/settings.conf"}]
        },
        "configs":[{"name":"settings","content":b"token=config-secret".to_vec()}],
        "pre_deploy":{"command":["migrate"],"environment":{"DATABASE_URL":"hook-secret"}}
    })
}

fn spec_carrying_operations(spec: &Value) -> [Value; 3] {
    let machine_id = "a".repeat(32);
    [
        json!({"type":"run_container","machine_id":machine_id,"spec":spec,"skip_health_monitor":false}),
        json!({"type":"run_hook","machine_id":machine_id,"spec":spec,"old_hook_containers":[]}),
        json!({"type":"replace_container","machine_id":machine_id,"old_container_id":"b".repeat(64),"spec":spec,"skip_health_monitor":false}),
    ]
}

fn single_operation_preview(operation: &Value) -> Value {
    json!({
        "namespace":"production",
        "operations":[{"index":0,"machine_id":"a".repeat(32),"service_name":"api","operation":operation,"status":{"type":"pending"}}],
        "warnings":[]
    })
}

fn operation_spec(operation: &DeployOperation) -> &ResolvedServiceSpec {
    match operation {
        DeployOperation::RunContainer { spec, .. } | DeployOperation::RunHook { spec, .. } => spec,
        DeployOperation::ReplaceContainer(replacement) => &replacement.spec,
        other => panic!("{other:?} carries no spec"),
    }
}

fn project(preview: &DeployPreview, outcome: Value) -> Value {
    let projection =
        project_runtime_outcome(preview, json!({"version":1,"outcome":outcome})).unwrap();
    serde_json::to_value(projection).unwrap()
}

#[test]
fn runtime_previews_and_outcomes_redact_every_spec_carrying_operation() {
    let submitted: ResolvedServiceSpec = serde_json::from_value(secret_spec()).unwrap();
    for operation in spec_carrying_operations(&secret_spec()) {
        let preview = parse_runtime_preview(single_operation_preview(&operation)).unwrap();

        let redacted = operation_spec(&preview.operations[0].operation);
        assert_eq!(
            redacted.configs(),
            [ConfigSpec {
                name: "settings".into(),
                content: Vec::new(),
            }],
            "{operation}"
        );
        assert_eq!(redacted.config_mounts(), submitted.config_mounts());
        assert!(redacted.container.environment.is_empty(), "{operation}");
        let hook = redacted.pre_deploy.as_ref().unwrap();
        assert!(hook.environment.is_empty(), "{operation}");
        assert_eq!(hook.command, submitted.pre_deploy.as_ref().unwrap().command);
        assert_eq!(
            project(&preview, json!({"type":"success","completed":[operation]}))["confirmedServices"],
            json!(["api"])
        );
    }

    let [_, _, replace] = spec_carrying_operations(&secret_spec());
    let preview = parse_runtime_preview(single_operation_preview(&replace)).unwrap();
    let mut replacement = replace;
    replacement.as_object_mut().unwrap().remove("type");
    let failed = json!({
        "type":"failed",
        "completed":[],
        "failed":{
            "type":"replacement",
            "operation":replacement,
            "error":{"type":"cancelled"},
            "compensation":{"type":"old_untouched","stop_new_container":{"type":"stopped"}}
        },
        "unexecuted":[]
    });
    assert_eq!(project(&preview, failed)["failedServices"], json!(["api"]));
}

#[test]
fn outcome_matches_a_preview_stored_before_hook_environment_redaction() {
    let [run_container, ..] = spec_carrying_operations(&secret_spec());
    let mut stored: DeployPreview =
        serde_json::from_value(single_operation_preview(&run_container)).unwrap();
    for row in &mut stored.operations {
        let DeployOperation::RunContainer { spec, .. } = &mut row.operation else {
            unreachable!("the stored preview holds one run_container");
        };
        spec.container.environment.clear();
        spec.mount_graph.redact_config_content();
    }

    assert_eq!(
        project(
            &stored,
            json!({"type":"success","completed":[run_container]})
        )["confirmedServices"],
        json!(["api"])
    );
}
