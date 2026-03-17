use super::labels::{
    LABEL_KEY, LABEL_KIND, LABEL_MANAGED, LABEL_PARENT_ID, LEGACY_LABEL_CONFIG_HASH,
};
use super::spec::{ObservedContainer, RuntimeContainerSpec};
use bollard::models::{RestartPolicy, RestartPolicyNameEnum};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangedField {
    Image,
    Cmd,
    Entrypoint,
    Env,
    Binds,
    Tmpfs,
    DnsServers,
    NetworkMode,
    PortBindings,
    CapAdd,
    CapDrop,
    Privileged,
    User,
    RestartPolicy,
    MemoryBytes,
    NanoCpus,
    Sysctls,
    PidMode,
}

#[derive(Debug)]
pub enum SpecChange {
    InSync,
    Missing,
    Drifted { fields: Vec<ChangedField> },
}

impl SpecChange {
    #[must_use]
    pub fn is_in_sync(&self) -> bool {
        matches!(self, Self::InSync)
    }
}

/// Compare observed container state against the desired spec.
///
/// Label-only drift (migration from old labels) is treated as InSync.
/// A non-running container is treated as Missing (needs recreation).
#[must_use]
pub fn eval_spec_change(
    observed: Option<&ObservedContainer>,
    desired: &RuntimeContainerSpec,
) -> SpecChange {
    let Some(observed) = observed else {
        return SpecChange::Missing;
    };

    if !observed.running {
        return SpecChange::Missing;
    }

    let mut fields = Vec::new();

    if observed.image != desired.image {
        fields.push(ChangedField::Image);
    }

    if observed.cmd != desired.cmd {
        fields.push(ChangedField::Cmd);
    }

    if !entrypoint_equal(observed.entrypoint.as_ref(), desired.entrypoint.as_ref()) {
        fields.push(ChangedField::Entrypoint);
    }

    // Compare env sorted by key for stable comparison
    if !env_equal(&observed.env, &desired.env) {
        fields.push(ChangedField::Env);
    }

    // Compare binds sorted
    if !sorted_eq(&observed.binds, &desired.binds) {
        fields.push(ChangedField::Binds);
    }

    if observed.tmpfs != desired.tmpfs {
        fields.push(ChangedField::Tmpfs);
    }

    if !sorted_eq(&observed.dns_servers, &desired.dns_servers) {
        fields.push(ChangedField::DnsServers);
    }

    if !network_mode_equal(
        observed.network_mode.as_deref(),
        desired.network_mode.as_deref(),
    ) {
        fields.push(ChangedField::NetworkMode);
    }

    if observed.port_bindings != desired.port_bindings {
        fields.push(ChangedField::PortBindings);
    }

    if !sorted_eq(&observed.cap_add, &desired.cap_add) {
        fields.push(ChangedField::CapAdd);
    }

    if !sorted_eq(&observed.cap_drop, &desired.cap_drop) {
        fields.push(ChangedField::CapDrop);
    }

    if observed.privileged != desired.privileged {
        fields.push(ChangedField::Privileged);
    }

    if observed.user != desired.user {
        fields.push(ChangedField::User);
    }

    if !restart_policy_equal(
        observed.restart_policy.as_ref(),
        desired.restart_policy.as_ref(),
    ) {
        fields.push(ChangedField::RestartPolicy);
    }

    if observed.memory_bytes != desired.memory_bytes {
        fields.push(ChangedField::MemoryBytes);
    }

    if observed.nano_cpus != desired.nano_cpus {
        fields.push(ChangedField::NanoCpus);
    }

    if observed.sysctls != desired.sysctls {
        fields.push(ChangedField::Sysctls);
    }

    if !pid_mode_equal(observed.pid_mode.as_deref(), desired.pid_mode.as_deref()) {
        fields.push(ChangedField::PidMode);
    }

    if fields.is_empty() {
        SpecChange::InSync
    } else {
        SpecChange::Drifted { fields }
    }
}

/// Check if the observed container is a legacy container (has old labels
/// but no new `dev.ployz.key` label). Used for migration-period adoption.
#[must_use]
pub fn is_legacy_container(observed: &ObservedContainer) -> bool {
    let has_new_key = observed.labels.contains_key(LABEL_KEY);
    let has_legacy = observed.labels.contains_key(LEGACY_LABEL_CONFIG_HASH)
        || observed.labels.contains_key(LABEL_MANAGED)
        || observed.labels.contains_key(LABEL_KIND);
    !has_new_key && (has_legacy || !observed.labels.is_empty())
}

/// Check if the observed container has the new unified label schema.
#[must_use]
pub fn has_new_labels(observed: &ObservedContainer) -> bool {
    observed.labels.contains_key(LABEL_KEY)
        && observed.labels.contains_key(LABEL_MANAGED)
        && observed.labels.contains_key(LABEL_KIND)
}

/// Compare parent container IDs for network namespace stability.
/// Returns true if the parent ID matches or no parent is expected.
#[must_use]
pub fn parent_id_matches(observed: &ObservedContainer, desired_parent_id: Option<&str>) -> bool {
    match desired_parent_id {
        None => true,
        Some(expected) => observed
            .labels
            .get(LABEL_PARENT_ID)
            .or_else(|| observed.labels.get(super::labels::LEGACY_LABEL_PARENT_ID))
            .map(|stored| stored == expected)
            .unwrap_or(false),
    }
}

fn entrypoint_equal(observed: Option<&Vec<String>>, desired: Option<&Vec<String>>) -> bool {
    match desired {
        None => true,
        Some(desired) => observed == Some(desired),
    }
}

fn env_equal(observed: &[(String, String)], desired: &[(String, String)]) -> bool {
    let ignore_path = !desired.iter().any(|(key, _)| key == "PATH");
    let mut observed_sorted: Vec<(&str, &str)> = observed
        .iter()
        .filter(|(key, _)| !(ignore_path && key == "PATH"))
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let mut desired_sorted: Vec<(&str, &str)> = desired
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    observed_sorted.sort();
    desired_sorted.sort();
    observed_sorted == desired_sorted
}

fn network_mode_equal(observed: Option<&str>, desired: Option<&str>) -> bool {
    if observed == desired {
        return true;
    }

    matches!(
        (observed, desired),
        (Some(observed), Some(desired))
            if observed.starts_with("container:") && desired.starts_with("container:")
    )
}

fn restart_policy_equal(observed: Option<&RestartPolicy>, desired: Option<&RestartPolicy>) -> bool {
    let observed = normalize_restart_policy(observed);
    let desired = desired.cloned();
    observed == desired
}

fn normalize_restart_policy(policy: Option<&RestartPolicy>) -> Option<RestartPolicy> {
    let Some(policy) = policy else {
        return None;
    };

    let name = policy.name.as_ref();
    let maximum_retry_count = policy.maximum_retry_count;
    if name == Some(&RestartPolicyNameEnum::NO) && maximum_retry_count.unwrap_or(0) == 0 {
        return None;
    }

    Some(policy.clone())
}

fn pid_mode_equal(observed: Option<&str>, desired: Option<&str>) -> bool {
    normalize_optional_string(observed) == normalize_optional_string(desired)
}

fn normalize_optional_string(value: Option<&str>) -> Option<&str> {
    match value {
        Some("") => None,
        _ => value,
    }
}

fn sorted_eq(a: &[String], b: &[String]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut a_sorted: Vec<&str> = a.iter().map(String::as_str).collect();
    let mut b_sorted: Vec<&str> = b.iter().map(String::as_str).collect();
    a_sorted.sort();
    b_sorted.sort();
    a_sorted == b_sorted
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{RestartPolicy, RestartPolicyNameEnum};
    use std::collections::HashMap;

    fn base_observed() -> ObservedContainer {
        ObservedContainer {
            container_id: "abc123".into(),
            container_name: "test".into(),
            running: true,
            image: "myimage:latest".into(),
            cmd: Some(vec!["run".into()]),
            entrypoint: None,
            env: vec![("FOO".into(), "bar".into())],
            labels: HashMap::new(),
            binds: vec!["/host:/container".into()],
            tmpfs: HashMap::new(),
            dns_servers: Vec::new(),
            network_mode: None,
            port_bindings: None,
            cap_add: Vec::new(),
            cap_drop: Vec::new(),
            privileged: false,
            user: None,
            restart_policy: None,
            memory_bytes: None,
            nano_cpus: None,
            sysctls: HashMap::new(),
            pid_mode: None,
            ip_address: None,
            networks: HashMap::new(),
        }
    }

    fn base_spec() -> RuntimeContainerSpec {
        RuntimeContainerSpec {
            key: "system/test".into(),
            container_name: "test".into(),
            image: "myimage:latest".into(),
            cmd: Some(vec!["run".into()]),
            env: vec![("FOO".into(), "bar".into())],
            binds: vec!["/host:/container".into()],
            dns_servers: Vec::new(),
            ..Default::default()
        }
    }

    #[test]
    fn identical_specs_in_sync() {
        let observed = base_observed();
        let desired = base_spec();
        let change = eval_spec_change(Some(&observed), &desired);
        assert!(change.is_in_sync());
    }

    #[test]
    fn none_observed_is_missing() {
        let desired = base_spec();
        let change = eval_spec_change(None, &desired);
        assert!(matches!(change, SpecChange::Missing));
    }

    #[test]
    fn not_running_is_missing() {
        let mut observed = base_observed();
        observed.running = false;
        let desired = base_spec();
        let change = eval_spec_change(Some(&observed), &desired);
        assert!(matches!(change, SpecChange::Missing));
    }

    #[test]
    fn changed_image_is_drifted() {
        let observed = base_observed();
        let mut desired = base_spec();
        desired.image = "other:v2".into();
        let change = eval_spec_change(Some(&observed), &desired);
        let SpecChange::Drifted { fields } = change else {
            panic!("expected drifted change");
        };
        assert!(fields.contains(&ChangedField::Image));
    }

    #[test]
    fn changed_env_is_drifted() {
        let observed = base_observed();
        let mut desired = base_spec();
        desired.env = vec![("FOO".into(), "baz".into())];
        let change = eval_spec_change(Some(&observed), &desired);
        let SpecChange::Drifted { fields } = change else {
            panic!("expected drifted change");
        };
        assert!(fields.contains(&ChangedField::Env));
    }

    #[test]
    fn observed_path_is_ignored_when_not_explicitly_desired() {
        let mut observed = base_observed();
        observed.env.push(("PATH".into(), "/usr/local/bin".into()));
        let desired = base_spec();
        let change = eval_spec_change(Some(&observed), &desired);
        assert!(change.is_in_sync());
    }

    #[test]
    fn extra_non_path_env_is_drifted() {
        let mut observed = base_observed();
        observed.env.push(("BAR".into(), "baz".into()));
        let desired = base_spec();
        let change = eval_spec_change(Some(&observed), &desired);
        let SpecChange::Drifted { fields } = change else {
            panic!("expected drifted change");
        };
        assert!(fields.contains(&ChangedField::Env));
    }

    #[test]
    fn changed_dns_servers_is_drifted() {
        let observed = base_observed();
        let mut desired = base_spec();
        desired.dns_servers = vec!["10.210.0.2".into()];
        let change = eval_spec_change(Some(&observed), &desired);
        let SpecChange::Drifted { fields } = change else {
            panic!("expected drifted change");
        };
        assert!(fields.contains(&ChangedField::DnsServers));
    }

    #[test]
    fn image_entrypoint_is_ignored_when_not_explicitly_desired() {
        let mut observed = base_observed();
        observed.entrypoint = Some(vec!["/bin/service".into()]);
        let desired = base_spec();
        let change = eval_spec_change(Some(&observed), &desired);
        assert!(change.is_in_sync());
    }

    #[test]
    fn explicit_entrypoint_mismatch_is_drifted() {
        let mut observed = base_observed();
        observed.entrypoint = Some(vec!["/bin/service".into()]);
        let mut desired = base_spec();
        desired.entrypoint = Some(vec!["/bin/other".into()]);
        let change = eval_spec_change(Some(&observed), &desired);
        let SpecChange::Drifted { fields } = change else {
            panic!("expected drifted change");
        };
        assert!(fields.contains(&ChangedField::Entrypoint));
    }

    #[test]
    fn container_network_mode_uses_parent_match_not_raw_string() {
        let mut observed = base_observed();
        observed.network_mode = Some("container:abc123".into());
        let mut desired = base_spec();
        desired.network_mode = Some("container:ployz-networking".into());
        let change = eval_spec_change(Some(&observed), &desired);
        assert!(change.is_in_sync());
    }

    #[test]
    fn non_container_network_mode_mismatch_is_drifted() {
        let mut observed = base_observed();
        observed.network_mode = Some("host".into());
        let mut desired = base_spec();
        desired.network_mode = Some("none".into());
        let change = eval_spec_change(Some(&observed), &desired);
        let SpecChange::Drifted { fields } = change else {
            panic!("expected drifted change");
        };
        assert!(fields.contains(&ChangedField::NetworkMode));
    }

    #[test]
    fn default_restart_policy_is_equivalent_to_none() {
        let mut observed = base_observed();
        observed.restart_policy = Some(RestartPolicy {
            name: Some(RestartPolicyNameEnum::NO),
            maximum_retry_count: Some(0),
        });
        let desired = base_spec();
        let change = eval_spec_change(Some(&observed), &desired);
        assert!(change.is_in_sync());
    }

    #[test]
    fn explicit_restart_policy_mismatch_is_drifted() {
        let mut observed = base_observed();
        observed.restart_policy = Some(RestartPolicy {
            name: Some(RestartPolicyNameEnum::ALWAYS),
            maximum_retry_count: None,
        });
        let desired = base_spec();
        let change = eval_spec_change(Some(&observed), &desired);
        let SpecChange::Drifted { fields } = change else {
            panic!("expected drifted change");
        };
        assert!(fields.contains(&ChangedField::RestartPolicy));
    }

    #[test]
    fn empty_pid_mode_is_equivalent_to_none() {
        let mut observed = base_observed();
        observed.pid_mode = Some(String::new());
        let desired = base_spec();
        let change = eval_spec_change(Some(&observed), &desired);
        assert!(change.is_in_sync());
    }

    #[test]
    fn explicit_pid_mode_mismatch_is_drifted() {
        let mut observed = base_observed();
        observed.pid_mode = Some("host".into());
        let desired = base_spec();
        let change = eval_spec_change(Some(&observed), &desired);
        let SpecChange::Drifted { fields } = change else {
            panic!("expected drifted change");
        };
        assert!(fields.contains(&ChangedField::PidMode));
    }

    #[test]
    fn env_order_independent() {
        let mut observed = base_observed();
        observed.env = vec![("B".into(), "2".into()), ("A".into(), "1".into())];
        let mut desired = base_spec();
        desired.env = vec![("A".into(), "1".into()), ("B".into(), "2".into())];
        desired.image = "myimage:latest".into();
        desired.cmd = Some(vec!["run".into()]);
        desired.binds = vec!["/host:/container".into()];
        let change = eval_spec_change(Some(&observed), &desired);
        assert!(change.is_in_sync());
    }

    #[test]
    fn old_labels_only_treated_as_legacy() {
        let mut observed = base_observed();
        observed
            .labels
            .insert("ployz.config-hash".into(), "abc".into());
        assert!(is_legacy_container(&observed));
        assert!(!has_new_labels(&observed));
    }

    #[test]
    fn parent_id_matches_when_equal() {
        let mut observed = base_observed();
        observed
            .labels
            .insert(LABEL_PARENT_ID.into(), "container123".into());
        assert!(parent_id_matches(&observed, Some("container123")));
        assert!(!parent_id_matches(&observed, Some("other")));
    }

    #[test]
    fn parent_id_matches_when_none_expected() {
        let observed = base_observed();
        assert!(parent_id_matches(&observed, None));
    }
}
