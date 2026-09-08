//! Shared setting comparison and redaction for runtime planning and read-only review.

use super::*;

/// Authored setting differences supported by detailed review.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SettingChange {
    pub setting: String,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
}

/// Settings with safe values available in both requested and observed specs.
/// Environment and attachment contents use redacted evidence rules.
pub const COMPARED_SERVICE_SETTINGS: &[&str] = &[
    "name",
    "mode",
    "replicas",
    "image",
    "command",
    "entrypoint",
    "labels",
    "hostname",
    "extra_hosts",
    "cap_add",
    "cap_drop",
    "healthcheck",
    "init",
    "user",
    "working_directory",
    "tty",
    "open_stdin",
    "privileged",
    "pid_mode",
    "log_driver",
    "stop_timeout_secs",
    "sysctls",
    "restart",
    "placement",
    "resources",
    "pull_policy",
    "pre_deploy",
    "update",
    "ports",
    "volumes",
    "mounts",
    "configs",
    "config_mounts",
    "ingress_proxy_fragment",
    "pre_deploy_environment",
];

/// Planning impact and the setting differences covered by review.
/// An empty change list covers only `COMPARED_SERVICE_SETTINGS`, not image content.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SpecComparison {
    pub impact: SpecChange,
    pub changes: Vec<SettingChange>,
}

/// Return the runtime action needed to reconcile the requested serving shape.
#[must_use]
pub fn compare_specs(
    current: &ResolvedServiceSpec,
    requested: &RequestedServiceSpec,
) -> SpecChange {
    compare_specs_detailed(current, requested).impact
}

/// Compare shared settings while redacting environment and attachment contents.
#[must_use]
pub fn compare_specs_detailed(
    current: &ResolvedServiceSpec,
    requested: &RequestedServiceSpec,
) -> SpecComparison {
    let mut before = ServingShape::fields_of_resolved(current);
    let mut after = ServingShape::fields_of_requested(requested);
    for (fields, container, mode, pre_deploy, update) in [
        (
            &mut before,
            &current.container,
            &current.mode,
            &current.pre_deploy,
            serde_json::json!(current.update),
        ),
        (
            &mut after,
            &requested.container,
            &requested.mode,
            &requested.pre_deploy,
            serde_json::json!(requested.update),
        ),
    ] {
        fields["replicas"] = match mode {
            ServiceMode::Replicated { replicas } => serde_json::json!(replicas),
            ServiceMode::Global => serde_json::Value::Null,
        };
        fields["resources"] = serde_json::json!(container.resources);
        fields["pull_policy"] = serde_json::json!(container.pull_policy);
        fields["pre_deploy"] = serde_json::json!(pre_deploy);
        fields["pre_deploy_environment"] = fields
            .get("pre_deploy")
            .and_then(|hook| hook.get("environment"))
            .cloned()
            .unwrap_or_default();
        if let Some(hook) = fields
            .get_mut("pre_deploy")
            .and_then(serde_json::Value::as_object_mut)
        {
            hook.remove("environment");
        }
        fields["update"] = update;
    }
    let changes = COMPARED_SERVICE_SETTINGS
        .iter()
        .filter(|&setting| before.get(*setting) != after.get(*setting))
        .map(|setting| SettingChange {
            setting: (*setting).into(),
            before: review_setting_value(
                setting,
                before.get(*setting).unwrap_or(&serde_json::Value::Null),
            ),
            after: review_setting_value(
                setting,
                after.get(*setting).unwrap_or(&serde_json::Value::Null),
            ),
        })
        .collect();
    let impact = if requested.container.pull_policy == PullPolicy::Always
        || current.serving_shape() != requested.serving_shape()
    {
        SpecChange::NeedsRecreate
    } else {
        resource_change(&current.container.resources, &requested.container.resources)
    };
    SpecComparison { impact, changes }
}

fn review_setting_value(setting: &str, value: &serde_json::Value) -> serde_json::Value {
    if matches!(
        setting,
        "volumes" | "configs" | "ingress_proxy_fragment" | "pre_deploy_environment"
    ) {
        // Driver options, config content, proxy fragments, and hook variables may
        // contain credentials. Report the change without publishing values or hashes.
        if value.is_null()
            || value.as_array().is_some_and(Vec::is_empty)
            || value.as_object().is_some_and(serde_json::Map::is_empty)
        {
            serde_json::Value::Null
        } else {
            serde_json::json!({"redacted": true})
        }
    } else {
        value.clone()
    }
}

fn resource_change(current: &ContainerResources, requested: &ContainerResources) -> SpecChange {
    let ContainerResources {
        cpu_nanos: current_cpu_nanos,
        memory_bytes: current_memory_bytes,
        memory_reservation_bytes: current_memory_reservation_bytes,
        shared_memory_bytes: current_shared_memory_bytes,
        devices: current_devices,
        device_reservations: current_device_reservations,
        ulimits: current_ulimits,
    } = current;
    let ContainerResources {
        cpu_nanos: requested_cpu_nanos,
        memory_bytes: requested_memory_bytes,
        memory_reservation_bytes: requested_memory_reservation_bytes,
        shared_memory_bytes: requested_shared_memory_bytes,
        devices: requested_devices,
        device_reservations: requested_device_reservations,
        ulimits: requested_ulimits,
    } = requested;
    if current_devices != requested_devices
        || current_device_reservations != requested_device_reservations
        || current_ulimits != requested_ulimits
    {
        return SpecChange::NeedsRecreate;
    }
    if current_cpu_nanos != requested_cpu_nanos
        || current_memory_bytes != requested_memory_bytes
        || current_memory_reservation_bytes != requested_memory_reservation_bytes
        || current_shared_memory_bytes != requested_shared_memory_bytes
    {
        return SpecChange::NeedsUpdate;
    }
    SpecChange::UpToDate
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn command_review_and_planning_share_the_field_change() {
        let current: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": "a".repeat(32), "name": "api",
            "mode": {"mode": "replicated", "replicas": 1},
            "container": {"image": "api:1", "pull_policy": "missing", "command": ["serve"]}
        }))
        .unwrap();
        let mut candidate = current.to_requested();
        candidate.container.command = vec!["serve".into(), "--workers=2".into()];
        let comparison = compare_specs_detailed(&current, &candidate);
        assert_eq!(comparison.impact, SpecChange::NeedsRecreate);
        assert_eq!(
            comparison.changes,
            vec![SettingChange {
                setting: "command".into(),
                before: json!(["serve"]),
                after: json!(["serve", "--workers=2"]),
            }]
        );
        assert_eq!(compare_specs(&current, &candidate), comparison.impact);
        candidate = current.to_requested();
        candidate.container.pull_policy = PullPolicy::Always;
        let comparison = compare_specs_detailed(&current, &candidate);
        assert_eq!(comparison.impact, SpecChange::NeedsRecreate);
        assert!(
            comparison
                .changes
                .iter()
                .all(|change| change.setting != "command"),
            "always-pull is not a command edit"
        );
    }

    #[test]
    fn setting_review_keeps_resource_impact_and_hook_secrets_separate() {
        let current: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": "a".repeat(32), "name": "api",
            "mode": {"mode": "replicated", "replicas": 1},
            "container": {"image": "api:1", "pull_policy": "missing"},
            "pre_deploy": {"command": ["migrate"], "environment": {"TOKEN": "secret-before"}}
        }))
        .unwrap();
        let mut requested = current.to_requested();
        requested.container.resources.memory_bytes =
            Some(crate::ByteQuantity::try_from(64).unwrap());
        requested
            .pre_deploy
            .as_mut()
            .unwrap()
            .environment
            .insert("TOKEN".into(), "secret-after".into());
        let comparison = compare_specs_detailed(&current, &requested);
        assert_eq!(comparison.impact, SpecChange::NeedsUpdate);
        assert_eq!(
            comparison
                .changes
                .iter()
                .map(|change| change.setting.as_str())
                .collect::<Vec<_>>(),
            ["resources", "pre_deploy_environment"]
        );
        assert!(
            !serde_json::to_string(&comparison)
                .unwrap()
                .contains("secret-")
        );
        requested.container.labels = serde_json::from_value(json!({"revision": "two"})).unwrap();
        let comparison = compare_specs_detailed(&current, &requested);
        assert_eq!(comparison.impact, SpecChange::NeedsRecreate);
        assert!(
            comparison
                .changes
                .iter()
                .any(|change| change.setting == "labels")
        );
        assert_eq!(comparison.impact, compare_specs(&current, &requested));
        requested
            .set_config_graph(
                ServiceConfigGraph::parse(
                    vec![ConfigSpec {
                        name: "credentials".into(),
                        content: b"secret-config".to_vec(),
                    }],
                    vec![ConfigMount {
                        config_name: "credentials".into(),
                        target: None,
                        uid: None,
                        gid: None,
                        mode: None,
                    }],
                )
                .unwrap(),
            )
            .unwrap();
        let comparison = compare_specs_detailed(&current, &requested);
        let config = comparison
            .changes
            .iter()
            .find(|change| change.setting == "configs")
            .unwrap();
        assert_eq!(config.before, serde_json::Value::Null);
        assert_eq!(config.after, json!({"redacted": true}));
        assert!(
            comparison
                .changes
                .iter()
                .any(|change| change.setting == "config_mounts")
        );
    }
}
