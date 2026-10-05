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

fn run_container(spec: &Value) -> Value {
    json!({"type":"run_container", "machine_id":"a".repeat(32), "spec":spec, "skip_health_monitor":false})
}

fn single_operation_preview(operation: &Value) -> Value {
    json!({
        "namespace":"production",
        "operations":[{"index":0,"machine_id":"a".repeat(32),"service_name":"api","operation":operation,"status":{"type":"pending"}}],
        "warnings":[]
    })
}

fn previewed_spec(preview: &DeployPreview) -> &ResolvedServiceSpec {
    let DeployOperation::RunContainer { spec, .. } = &preview.operations[0].operation else {
        panic!("preview keeps the run_container operation");
    };
    spec
}

fn service_spec() -> Value {
    json!({
        "service_id":"c".repeat(32),
        "name":"api",
        "mode":{"mode":"replicated","replicas":1},
        "container":{"image":"api:1","pull_policy":"missing","environment":{"TOKEN":"service-secret"}}
    })
}

#[test]
fn parse_runtime_preview_empties_config_content_and_keeps_mounts() {
    let mut spec = service_spec();
    spec["container"]["config_mounts"] =
        json!([{"config_name":"settings","target":"/etc/settings.conf"}]);
    spec["configs"] = json!([{"name":"settings","content":b"token=config-secret".to_vec()}]);
    let submitted: ResolvedServiceSpec = serde_json::from_value(spec.clone()).unwrap();
    let operation = run_container(&spec);

    let preview = parse_runtime_preview(single_operation_preview(&operation)).unwrap();

    let redacted = previewed_spec(&preview);
    assert_eq!(
        redacted.configs(),
        [ConfigSpec {
            name: "settings".into(),
            content: Vec::new(),
        }]
    );
    assert_eq!(redacted.config_mounts(), submitted.config_mounts());
    assert!(redacted.container.environment.is_empty());
    let projection = project_runtime_outcome(
        &preview,
        json!({"version":1,"outcome":{"type":"success","completed":[operation]}}),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(projection).unwrap()["confirmedServices"],
        json!(["api"])
    );
}

#[test]
fn parse_runtime_preview_redacts_hook_environment() {
    let mut spec = service_spec();
    spec["pre_deploy"] =
        json!({"command":["migrate"],"environment":{"DATABASE_URL":"hook-secret"}});

    let preview = parse_runtime_preview(single_operation_preview(&run_container(&spec))).unwrap();

    let hook = previewed_spec(&preview).pre_deploy.as_ref().unwrap();
    assert!(hook.environment.is_empty());
    assert_eq!(
        serde_json::to_value(&hook.command).unwrap(),
        json!(["migrate"])
    );
}
