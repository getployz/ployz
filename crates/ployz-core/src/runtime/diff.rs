use super::labels::{
    LABEL_KEY, LABEL_KIND, LABEL_MANAGED, LABEL_PARENT_ID, LEGACY_LABEL_CONFIG_HASH,
};
use super::spec::{ObservedContainer, RuntimeContainerSpec};

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

    if observed.entrypoint != desired.entrypoint {
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

    if observed.network_mode != desired.network_mode {
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

    if observed.restart_policy != desired.restart_policy {
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

    if observed.pid_mode != desired.pid_mode {
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

fn env_equal(a: &[(String, String)], b: &[(String, String)]) -> bool {
    let mut a_sorted: Vec<_> = a.to_vec();
    let mut b_sorted: Vec<_> = b.to_vec();
    a_sorted.sort();
    b_sorted.sort();
    a_sorted == b_sorted
}

fn sorted_eq(a: &[String], b: &[String]) -> bool {
    let mut a_sorted: Vec<_> = a.to_vec();
    let mut b_sorted: Vec<_> = b.to_vec();
    a_sorted.sort();
    b_sorted.sort();
    a_sorted == b_sorted
}

#[cfg(test)]
mod tests {
    use super::*;
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
