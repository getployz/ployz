use super::labels::{LABEL_KEY, LABEL_KIND, LABEL_MANAGED, LABEL_PARENT_ID};
use super::spec::{
    Observation, ObservedContainer, RestartPolicy, RestartPolicyName, RuntimeContainerSpec,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangedField {
    Image,
    Cmd,
    Entrypoint,
    Env,
    Labels,
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
    StopTimeout,
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
/// A non-running container is treated as Missing (needs recreation).
#[must_use]
pub fn eval_spec_change(
    observed: Option<&ObservedContainer>,
    desired: &RuntimeContainerSpec,
) -> SpecChange {
    let Some(observed) = observed else {
        return SpecChange::Missing;
    };

    if observed.running != Observation::Observed(true) {
        return SpecChange::Missing;
    }

    let mut fields = Vec::new();

    if observed.image.as_observed() != Some(&desired.image) {
        fields.push(ChangedField::Image);
    }

    if observed.cmd.as_observed() != Some(&desired.cmd) {
        fields.push(ChangedField::Cmd);
    }

    if !entrypoint_equal(
        observed.entrypoint.as_observed().and_then(Option::as_ref),
        desired.entrypoint.as_ref(),
    ) {
        fields.push(ChangedField::Entrypoint);
    }

    // Compare env sorted by key for stable comparison
    if !observed
        .env
        .as_observed()
        .is_some_and(|observed| env_equal(observed, &desired.env))
    {
        fields.push(ChangedField::Env);
    }

    if !observed
        .labels
        .as_observed()
        .is_some_and(|observed| labels_match(observed, &desired.labels))
    {
        fields.push(ChangedField::Labels);
    }

    // Compare binds sorted
    if !observed
        .binds
        .as_observed()
        .is_some_and(|observed| sorted_eq(observed, &desired.binds))
    {
        fields.push(ChangedField::Binds);
    }

    if observed.tmpfs.as_observed() != Some(&desired.tmpfs) {
        fields.push(ChangedField::Tmpfs);
    }

    if !observed
        .dns_servers
        .as_observed()
        .is_some_and(|observed| sorted_eq(observed, &desired.dns_servers))
    {
        fields.push(ChangedField::DnsServers);
    }

    if !network_mode_equal(
        observed
            .network_mode
            .as_observed()
            .and_then(Option::as_deref),
        desired.network_mode.as_deref(),
    ) {
        fields.push(ChangedField::NetworkMode);
    }

    if observed.port_bindings.as_observed() != Some(&desired.port_bindings) {
        fields.push(ChangedField::PortBindings);
    }

    if !observed
        .cap_add
        .as_observed()
        .is_some_and(|observed| sorted_eq(observed, &desired.cap_add))
    {
        fields.push(ChangedField::CapAdd);
    }

    if !observed
        .cap_drop
        .as_observed()
        .is_some_and(|observed| sorted_eq(observed, &desired.cap_drop))
    {
        fields.push(ChangedField::CapDrop);
    }

    if observed.privileged.as_observed() != Some(&desired.privileged) {
        fields.push(ChangedField::Privileged);
    }

    if observed.user.as_observed() != Some(&desired.user) {
        fields.push(ChangedField::User);
    }

    if !restart_policy_equal(
        observed
            .restart_policy
            .as_observed()
            .and_then(Option::as_ref),
        desired.restart_policy.as_ref(),
    ) {
        fields.push(ChangedField::RestartPolicy);
    }

    if observed.memory_bytes.as_observed() != Some(&desired.memory_bytes) {
        fields.push(ChangedField::MemoryBytes);
    }

    if observed.nano_cpus.as_observed() != Some(&desired.nano_cpus) {
        fields.push(ChangedField::NanoCpus);
    }

    if observed.sysctls.as_observed() != Some(&desired.sysctls) {
        fields.push(ChangedField::Sysctls);
    }

    if observed.stop_timeout.as_observed() != Some(&desired.stop_timeout) {
        fields.push(ChangedField::StopTimeout);
    }

    if !pid_mode_equal(
        observed.pid_mode.as_observed().and_then(Option::as_deref),
        desired.pid_mode.as_deref(),
    ) {
        fields.push(ChangedField::PidMode);
    }

    if fields.is_empty() {
        SpecChange::InSync
    } else {
        SpecChange::Drifted { fields }
    }
}

/// Check if the observed container has the unified label schema.
#[must_use]
pub fn has_new_labels(observed: &ObservedContainer) -> bool {
    observed.labels.as_observed().is_some_and(|labels| {
        labels.contains_key(LABEL_KEY)
            && labels.contains_key(LABEL_MANAGED)
            && labels.contains_key(LABEL_KIND)
    })
}

/// Compare parent container IDs for network namespace stability.
/// Returns true if the parent ID matches or no parent is expected.
#[must_use]
pub fn parent_id_matches(observed: &ObservedContainer, desired_parent_id: Option<&str>) -> bool {
    match desired_parent_id {
        None => true,
        Some(expected) => observed.labels.as_observed().is_some_and(|labels| {
            labels
                .get(LABEL_PARENT_ID)
                .is_some_and(|stored| stored == expected)
        }),
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

fn labels_match(
    observed: &std::collections::HashMap<String, String>,
    desired: &std::collections::HashMap<String, String>,
) -> bool {
    if desired.get(LABEL_KIND).map(String::as_str) == Some("system") {
        return desired
            .iter()
            .all(|(key, value)| observed.get(key) == Some(value));
    }
    observed == desired
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
    let policy = policy?;

    let maximum_retry_count = policy.maximum_retry_count;
    if policy.name == Some(RestartPolicyName::No) && maximum_retry_count.unwrap_or(0) == 0 {
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
    use std::collections::HashMap;

    fn observed<T>(value: T) -> Observation<T> {
        Observation::Observed(value)
    }

    fn base_observed() -> ObservedContainer {
        ObservedContainer {
            container_id: observed("abc123".into()),
            container_name: observed("test".into()),
            running: observed(true),
            image: observed("myimage:latest".into()),
            cmd: observed(Some(vec!["run".into()])),
            entrypoint: observed(None),
            env: observed(vec![("FOO".into(), "bar".into())]),
            labels: observed(HashMap::new()),
            binds: observed(vec!["/host:/container".into()]),
            tmpfs: observed(HashMap::new()),
            dns_servers: observed(Vec::new()),
            network_mode: observed(None),
            port_bindings: observed(None),
            cap_add: observed(Vec::new()),
            cap_drop: observed(Vec::new()),
            privileged: observed(false),
            user: observed(None),
            restart_policy: observed(None),
            memory_bytes: observed(None),
            nano_cpus: observed(None),
            sysctls: observed(HashMap::new()),
            stop_timeout: observed(None),
            pid_mode: observed(None),
            ip_address: observed(None),
            networks: observed(HashMap::new()),
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
        observed.running = Observation::Observed(false);
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
    fn changed_desired_label_is_drifted() {
        let mut observed = base_observed();
        observed
            .labels
            .as_observed_mut()
            .expect("labels observed")
            .insert("dev.ployz.kind".into(), "system".into());
        observed
            .labels
            .as_observed_mut()
            .expect("labels observed")
            .insert("dev.ployz.version".into(), "0.5.2".into());
        let mut desired = base_spec();
        desired
            .labels
            .insert("dev.ployz.kind".into(), "system".into());
        desired
            .labels
            .insert("dev.ployz.version".into(), "0.5.3".into());

        let change = eval_spec_change(Some(&observed), &desired);

        let SpecChange::Drifted { fields } = change else {
            panic!("expected drifted change");
        };
        assert!(fields.contains(&ChangedField::Labels));
    }

    #[test]
    fn extra_observed_system_label_is_adopted() {
        let mut observed = base_observed();
        observed
            .labels
            .as_observed_mut()
            .expect("labels observed")
            .insert("dev.ployz.kind".into(), "system".into());
        observed
            .labels
            .as_observed_mut()
            .expect("labels observed")
            .insert("extra".into(), "value".into());
        let mut desired = base_spec();
        desired
            .labels
            .insert("dev.ployz.kind".into(), "system".into());

        let change = eval_spec_change(Some(&observed), &desired);

        assert!(change.is_in_sync());
    }

    #[test]
    fn extra_observed_workload_label_is_drifted() {
        let mut observed = base_observed();
        observed
            .labels
            .as_observed_mut()
            .expect("labels observed")
            .insert("dev.ployz.kind".into(), "workload".into());
        observed
            .labels
            .as_observed_mut()
            .expect("labels observed")
            .insert("extra".into(), "value".into());
        let mut desired = base_spec();
        desired
            .labels
            .insert("dev.ployz.kind".into(), "workload".into());

        let change = eval_spec_change(Some(&observed), &desired);

        let SpecChange::Drifted { fields } = change else {
            panic!("expected drifted change");
        };
        assert!(fields.contains(&ChangedField::Labels));
    }

    #[test]
    fn observed_path_is_ignored_when_not_explicitly_desired() {
        let mut observed = base_observed();
        observed
            .env
            .as_observed_mut()
            .expect("env observed")
            .push(("PATH".into(), "/usr/local/bin".into()));
        let desired = base_spec();
        let change = eval_spec_change(Some(&observed), &desired);
        assert!(change.is_in_sync());
    }

    #[test]
    fn extra_non_path_env_is_drifted() {
        let mut observed = base_observed();
        observed
            .env
            .as_observed_mut()
            .expect("env observed")
            .push(("BAR".into(), "baz".into()));
        let desired = base_spec();
        let change = eval_spec_change(Some(&observed), &desired);
        let SpecChange::Drifted { fields } = change else {
            panic!("expected drifted change");
        };
        assert!(fields.contains(&ChangedField::Env));
    }

    #[test]
    fn malformed_observed_field_is_not_silently_adopted() {
        let mut observed = base_observed();
        observed.labels = Observation::Malformed("labels were not a map".into());
        let desired = base_spec();

        let change = eval_spec_change(Some(&observed), &desired);

        let SpecChange::Drifted { fields } = change else {
            panic!("expected malformed observation to force drift");
        };
        assert_eq!(fields, vec![ChangedField::Labels]);
    }

    #[test]
    fn unknown_running_state_requires_recreation() {
        let mut observed = base_observed();
        observed.running = Observation::Unknown;
        let desired = base_spec();

        let change = eval_spec_change(Some(&observed), &desired);

        assert!(matches!(change, SpecChange::Missing));
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
        observed.entrypoint = Observation::Observed(Some(vec!["/bin/service".into()]));
        let desired = base_spec();
        let change = eval_spec_change(Some(&observed), &desired);
        assert!(change.is_in_sync());
    }

    #[test]
    fn explicit_entrypoint_mismatch_is_drifted() {
        let mut observed = base_observed();
        observed.entrypoint = Observation::Observed(Some(vec!["/bin/service".into()]));
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
        observed.network_mode = Observation::Observed(Some("container:abc123".into()));
        let mut desired = base_spec();
        desired.network_mode = Some("container:ployz-networking".into());
        let change = eval_spec_change(Some(&observed), &desired);
        assert!(change.is_in_sync());
    }

    #[test]
    fn non_container_network_mode_mismatch_is_drifted() {
        let mut observed = base_observed();
        observed.network_mode = Observation::Observed(Some("host".into()));
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
        observed.restart_policy = Observation::Observed(Some(RestartPolicy {
            name: Some(RestartPolicyName::No),
            maximum_retry_count: Some(0),
        }));
        let desired = base_spec();
        let change = eval_spec_change(Some(&observed), &desired);
        assert!(change.is_in_sync());
    }

    #[test]
    fn explicit_restart_policy_mismatch_is_drifted() {
        let mut observed = base_observed();
        observed.restart_policy = Observation::Observed(Some(RestartPolicy {
            name: Some(RestartPolicyName::Always),
            maximum_retry_count: None,
        }));
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
        observed.pid_mode = Observation::Observed(Some(String::new()));
        let desired = base_spec();
        let change = eval_spec_change(Some(&observed), &desired);
        assert!(change.is_in_sync());
    }

    #[test]
    fn explicit_pid_mode_mismatch_is_drifted() {
        let mut observed = base_observed();
        observed.pid_mode = Observation::Observed(Some("host".into()));
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
        observed.env =
            Observation::Observed(vec![("B".into(), "2".into()), ("A".into(), "1".into())]);
        let mut desired = base_spec();
        desired.env = vec![("A".into(), "1".into()), ("B".into(), "2".into())];
        desired.image = "myimage:latest".into();
        desired.cmd = Some(vec!["run".into()]);
        desired.binds = vec!["/host:/container".into()];
        let change = eval_spec_change(Some(&observed), &desired);
        assert!(change.is_in_sync());
    }

    #[test]
    fn parent_id_matches_when_equal() {
        let mut observed = base_observed();
        observed
            .labels
            .as_observed_mut()
            .expect("labels observed")
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
