//! What the Store runner and the GitHub build follow share: narrowing a lowering input
//! to one Service's build, and rendering a build progress event as a log line.

use ployz_core::config::LowerDeploymentInput;
use ployz_core::{RpcError, ServiceName};
use serde::Deserialize as _;
use serde_json::Value;

/// Lowering input `input` narrowed to Service `service`: what its own build takes.
/// A build reads no Config, and Config files may hold secrets. It retires nothing.
///
/// # Errors
/// Returns `invalid_argument` when `input` is not a lowering input.
pub(super) fn only(input: &Value, service: &ServiceName) -> Result<Value, RpcError> {
    let input = LowerDeploymentInput::deserialize(input)
        .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
    serde_json::to_value(input.narrowed_to(service))
        .map_err(|error| RpcError::internal(error.to_string()))
}

/// What one build progress event adds to its log.
pub(super) fn log_line(event: &Value) -> String {
    let text = |pointer: &str| event.pointer(pointer).and_then(Value::as_str);
    if let Some(machine) = text("/Selected/machine/name") {
        return format!("Building on {machine}\n");
    }
    if text("/Build/Stage") == Some("Queued") {
        return "Waiting for a build slot\n".to_owned();
    }
    if let Some(output) = text("/Build/StepOutput/text") {
        return output.to_owned();
    }
    if let Some(bytes) = event.pointer("/Build/Output").and_then(Value::as_array) {
        let bytes: Vec<u8> = bytes
            .iter()
            .filter_map(|byte| u8::try_from(byte.as_u64()?).ok())
            .collect();
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    let name = text("/Build/Step/name").unwrap_or_default();
    if let Some(error) = text("/Build/Step/error") {
        return format!("ERROR {name}: {error}\n");
    }
    if text("/Build/Step/completed").is_some() {
        let cached = event.pointer("/Build/Step/cached") == Some(&Value::Bool(true));
        return format!("{} {name}\n", if cached { "CACHED" } else { "DONE" });
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_input() -> serde_json::Value {
        let org = "11111111-1111-4111-8111-111111111111";
        serde_json::json!({
            "namespace": "production",
            "snapshots": [
                {"serviceId": "22222222-2222-4222-8222-222222222222",
                 "config": {"privateDns": "web", "replicas": 2},
                 "replicas": 3, "resolvedEnv": {"PORT": "8080"},
                 "setupCommands": ["seed"], "builtWith": "railpack"},
                {"serviceId": null, "config": {"privateDns": "api"},
                 "resolvedEnv": {}, "setupCommands": []},
            ],
            "volumes": [{"volumeResourceId": "vol", "storage": {"kind": "docker"}, "deployed": true}],
            "configs": [{"configResourceId": "cfg", "name": "sentry", "references": [],
                "files": {"config.yml": {"content": "dsn", "mode": "0444", "uid": 0, "gid": 0}}}],
            "lineages": {"web-lineage": "22222222-2222-4222-8222-222222222222"},
            "selected": [{"name": "web"}, {"name": "old"}],
            "removals": ["old"],
            "clusterDomain": "example.test",
            "sealedFor": {"organization": "acme", "environment": org},
        })
    }

    #[test]
    fn a_build_keeps_every_other_field_of_the_lowering_input_as_written() {
        let input = full_input();
        let narrowed = only(&input, &ployz_core::ServiceName::parse("web").unwrap()).unwrap();
        let mut expected = input;
        let document = expected.as_object_mut().unwrap();
        document.remove("configs");
        document.remove("removals");
        document
            .get_mut("snapshots")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .pop();
        document.insert("selected".into(), serde_json::json!([]));
        assert_eq!(narrowed, expected);
    }

    #[test]
    fn a_build_of_a_service_nothing_names_takes_no_snapshot() {
        let narrowed = only(
            &full_input(),
            &ployz_core::ServiceName::parse("db").unwrap(),
        )
        .unwrap();
        assert_eq!(narrowed.get("snapshots"), Some(&serde_json::json!([])));
        assert_eq!(narrowed.get("selected"), Some(&serde_json::json!([])));
    }

    #[test]
    fn a_build_receives_its_own_service_and_no_config_file() {
        let input = serde_json::json!({
            "namespace": "production",
            "snapshots": [{"config": {"privateDns": "web"}}, {"config": {"privateDns": "api"}}],
            "selected": [{"name": "web"}, {"name": "old"}],
            "removals": ["old"],
            "configs": [{"configResourceId": "cfg", "name": "sentry", "references": [],
                "files": {"config.yml": {"content": "dsn: redis.internal", "mode": "0444", "uid": 0, "gid": 0}}}],
        });
        let narrowed = only(&input, &ployz_core::ServiceName::parse("web").unwrap()).unwrap();
        assert_eq!(
            narrowed,
            serde_json::json!({
                "namespace": "production",
                "snapshots": [{"serviceId": null, "config": {"privateDns": "web"},
                    "resolvedEnv": {}, "setupCommands": []}],
                "volumes": [],
                "lineages": {},
                "selected": [],
            })
        );
    }
}
