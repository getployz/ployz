use std::collections::{BTreeMap, HashMap};

// Required on every managed container
pub const LABEL_MANAGED: &str = "dev.ployz.managed";
pub const LABEL_KIND: &str = "dev.ployz.kind";
pub const LABEL_KEY: &str = "dev.ployz.key";
pub const LABEL_PARENT_ID: &str = "dev.ployz.parent-id";

// Workload-specific (set when kind=workload)
pub const LABEL_NAMESPACE: &str = "dev.ployz.namespace";
pub const LABEL_SERVICE: &str = "dev.ployz.service";
pub const LABEL_REVISION: &str = "dev.ployz.revision";
pub const LABEL_DEPLOY: &str = "dev.ployz.deploy";
pub const LABEL_INSTANCE: &str = "dev.ployz.instance";
pub const LABEL_SLOT: &str = "dev.ployz.slot";
pub const LABEL_MACHINE: &str = "dev.ployz.machine";

pub struct WorkloadMeta<'a> {
    pub namespace: &'a str,
    pub service: &'a str,
    pub revision: &'a str,
    pub deploy_id: &'a str,
    pub instance_id: &'a str,
    pub slot_id: &'a str,
    pub machine_id: &'a str,
}

#[must_use]
pub fn build_system_labels(key: &str, parent_id: Option<&str>) -> HashMap<String, String> {
    let mut labels = HashMap::new();
    labels.insert(LABEL_MANAGED.into(), "true".into());
    labels.insert(LABEL_KIND.into(), "system".into());
    labels.insert(LABEL_KEY.into(), key.into());
    if let Some(pid) = parent_id {
        labels.insert(LABEL_PARENT_ID.into(), pid.into());
    }
    labels
}

#[must_use]
pub fn build_workload_labels(
    key: &str,
    meta: &WorkloadMeta<'_>,
    extra: &BTreeMap<String, String>,
) -> HashMap<String, String> {
    let mut labels = HashMap::new();
    labels.insert(LABEL_MANAGED.into(), "true".into());
    labels.insert(LABEL_KIND.into(), "workload".into());
    labels.insert(LABEL_KEY.into(), key.into());
    labels.insert(LABEL_NAMESPACE.into(), meta.namespace.into());
    labels.insert(LABEL_SERVICE.into(), meta.service.into());
    labels.insert(LABEL_REVISION.into(), meta.revision.into());
    labels.insert(LABEL_DEPLOY.into(), meta.deploy_id.into());
    labels.insert(LABEL_INSTANCE.into(), meta.instance_id.into());
    labels.insert(LABEL_SLOT.into(), meta.slot_id.into());
    labels.insert(LABEL_MACHINE.into(), meta.machine_id.into());
    for (k, v) in extra {
        labels.insert(k.clone(), v.clone());
    }
    labels
}

#[must_use]
pub fn parse_key(labels: &HashMap<String, String>) -> Option<&str> {
    labels.get(LABEL_KEY).map(String::as_str)
}

/// Extract workload labels from an observed container's label map.
/// Returns `None` if any required label is missing.
#[must_use]
pub fn extract_workload_labels(labels: &HashMap<String, String>) -> Option<WorkloadLabels> {
    Some(WorkloadLabels {
        instance_id: labels.get(LABEL_INSTANCE)?.clone(),
        service: labels.get(LABEL_SERVICE)?.clone(),
        slot_id: labels.get(LABEL_SLOT)?.clone(),
        machine_id: labels.get(LABEL_MACHINE)?.clone(),
        revision_hash: labels.get(LABEL_REVISION)?.clone(),
        deploy_id: labels.get(LABEL_DEPLOY)?.clone(),
    })
}

/// Owned workload label values extracted from a container.
pub struct WorkloadLabels {
    pub instance_id: String,
    pub service: String,
    pub slot_id: String,
    pub machine_id: String,
    pub revision_hash: String,
    pub deploy_id: String,
}

// Old label constants for migration-period adoption
pub(crate) const LEGACY_LABEL_CONFIG_HASH: &str = "ployz.config-hash";
pub(crate) const LEGACY_LABEL_PARENT_ID: &str = "ployz.parent-container-id";
