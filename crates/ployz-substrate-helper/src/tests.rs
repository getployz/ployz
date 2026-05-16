use super::*;
use std::process::Command;

#[test]
fn handle_line_rejects_unknown_operation() {
    let response = handle_line(
        r#"{"version":1,"request_id":"req-1","op":"docker.rm","params":{"name":"web"}}"#,
    );

    let error = response.error.expect("expected protocol error");
    assert_eq!(error.code, "invalid_request");
}

#[test]
fn handle_line_rejects_unsupported_version() {
    let response =
        handle_line(r#"{"version":2,"request_id":"req-1","op":"docker.list_ployz","params":{}}"#);

    let error = response.error.expect("expected version error");
    assert_eq!(error.code, "unsupported_version");
}

#[test]
fn helper_error_redacts_secret_details() {
    let error = helper_error(
        "failed",
        "failed",
        json!({
            "token": "abc",
            "nested": {
                "private_key": "secret",
                "safe": "value"
            }
        }),
    );

    assert_eq!(error.details.get("token"), Some(&json!("[REDACTED]")));

    let nested = error
        .details
        .get("nested")
        .and_then(Value::as_object)
        .expect("expected nested details object");

    assert_eq!(nested.get("private_key"), Some(&json!("[REDACTED]")));
    assert_eq!(nested.get("safe"), Some(&json!("value")));
}

#[test]
fn validate_container_name_rejects_shell_metacharacters() {
    let error = validate_container_name("web;rm").expect_err("expected invalid argument");
    assert_eq!(error.code, "invalid_argument");
}

#[test]
fn validate_image_rejects_docker_option_injection() {
    for image in ["--privileged", "--network=host", "-v", "busy box"] {
        let error = validate_image(image).expect_err("expected invalid image argument");
        assert_eq!(error.code, "invalid_argument");
    }

    validate_image("registry.example.com/team/app:sha-123").expect("expected valid image");
    validate_image("busybox@sha256:abc123").expect("expected valid digest image");
}

#[test]
fn valid_request_id_accepts_protocol_ids_only() {
    assert!(valid_request_id("req-1:abc.def"));
    assert!(!valid_request_id(""));
    assert!(!valid_request_id("req 1"));
}

#[test]
fn docker_operations_start_inspect_list_and_stop_container_when_docker_is_available() {
    if !docker_is_available() {
        eprintln!("skipping Docker-backed helper test because Docker is unavailable");
        return;
    }

    let image =
        std::env::var("PLOYZ_HELPER_E2E_IMAGE").unwrap_or_else(|_| String::from("busybox:latest"));
    let name = format!("ployz-helper-e2e-{}", std::process::id());
    cleanup_container(&name);

    let start = handle_line(&format!(
        r#"{{"version":1,"request_id":"start","op":"docker.start","params":{{"name":"{name}","image":"{image}","args":["sleep","30"],"labels":{{"ployz.test":"substrate-helper"}}}}}}"#
    ));

    assert_no_helper_error(&start);
    assert_eq!(
        start.ok.as_ref().expect("expected start payload")["name"],
        name
    );

    let inspect = handle_line(&format!(
        r#"{{"version":1,"request_id":"inspect","op":"docker.inspect","params":{{"name":"{name}"}}}}"#
    ));

    assert_no_helper_error(&inspect);
    assert!(inspect.ok.as_ref().expect("expected inspect payload")["resources"].is_array());

    let list =
        handle_line(r#"{"version":1,"request_id":"list","op":"docker.list_ployz","params":{}}"#);

    assert_no_helper_error(&list);
    let listed = list.ok.as_ref().expect("expected list payload")["resources"]
        .as_array()
        .expect("expected resource array")
        .iter()
        .any(|resource| {
            resource
                .get("Names")
                .and_then(Value::as_str)
                .is_some_and(|names| names.split(',').any(|candidate| candidate == name))
        });
    assert!(
        listed,
        "started container should appear in docker.list_ployz"
    );

    let stop = handle_line(&format!(
        r#"{{"version":1,"request_id":"stop","op":"docker.stop","params":{{"name":"{name}"}}}}"#
    ));

    cleanup_container(&name);
    assert_no_helper_error(&stop);
}

fn docker_is_available() -> bool {
    Command::new("docker")
        .args(["info", "--format", "{{json .ServerVersion}}"])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn cleanup_container(name: &str) {
    let _ = Command::new("docker").args(["rm", "-f", name]).output();
}

fn assert_no_helper_error(response: &Response) {
    if let Some(error) = &response.error {
        panic!(
            "helper operation failed: {} {} {}",
            error.code, error.message, error.details
        );
    }
}
