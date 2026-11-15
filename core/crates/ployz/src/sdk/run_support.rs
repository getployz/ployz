//! What the Store runner and the GitHub build follow share: narrowing a lowering input
//! to one Service's build, and rendering a build progress event as a log line.

use ployz_core::ServiceName;
use serde_json::Value;

/// Lowering input `input` narrowed to Service `service`: what its own build takes.
/// A build reads no Config, and Config files may hold secrets. It retires nothing.
pub(super) fn only(input: &Value, service: &ServiceName) -> Value {
    let mut input = input.clone();
    if let Some(input) = input.as_object_mut() {
        input.remove("configs");
        input.remove("removals");
    }
    if let Some(snapshots) = input.get_mut("snapshots").and_then(Value::as_array_mut) {
        snapshots.retain(|snapshot| {
            snapshot
                .pointer("/config/privateDns")
                .and_then(Value::as_str)
                == Some(service.as_str())
        });
    }
    if let Some(selected) = input.get_mut("selected") {
        *selected = Value::Array(Vec::new());
    }
    input
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
        let narrowed = only(&input, &ployz_core::ServiceName::parse("web").unwrap());
        assert_eq!(
            narrowed,
            serde_json::json!({
                "namespace": "production",
                "snapshots": [{"config": {"privateDns": "web"}}],
                "selected": [],
            })
        );
    }
}
