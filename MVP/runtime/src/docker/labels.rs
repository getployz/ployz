use std::collections::HashMap;

use mvp_deploy::{InstanceId, RevisionId};
use mvp_identity::NodeId;
use mvp_projection::ServiceName;

use crate::{RuntimeError, RuntimeInstance, RuntimeInstanceState, RuntimeResult};

pub const LABEL_MANAGED: &str = "dev.ployz.mvp.managed";
pub const LABEL_NODE: &str = "dev.ployz.mvp.node";
pub const LABEL_INSTANCE: &str = "dev.ployz.mvp.instance";
pub const LABEL_SERVICE: &str = "dev.ployz.mvp.service";
pub const LABEL_REVISION: &str = "dev.ployz.mvp.revision";
pub const LABEL_STATE: &str = "dev.ployz.mvp.state";

pub fn labels_for(
    node_id: &NodeId,
    instance_id: &InstanceId,
    service: &ServiceName,
    revision: &RevisionId,
    state: RuntimeInstanceState,
) -> HashMap<String, String> {
    HashMap::from([
        (LABEL_MANAGED.to_string(), "true".to_string()),
        (LABEL_NODE.to_string(), node_id.as_str().to_string()),
        (LABEL_INSTANCE.to_string(), instance_id.as_str().to_string()),
        (LABEL_SERVICE.to_string(), service.as_str().to_string()),
        (LABEL_REVISION.to_string(), revision.as_str().to_string()),
        (LABEL_STATE.to_string(), state_label(state).to_string()),
    ])
}

pub fn state_label(state: RuntimeInstanceState) -> &'static str {
    match state {
        RuntimeInstanceState::Prepared => "prepared",
        RuntimeInstanceState::Running => "running",
        RuntimeInstanceState::Draining => "draining",
        RuntimeInstanceState::Stopped => "stopped",
    }
}

pub fn parse_state(value: &str) -> RuntimeInstanceState {
    match value {
        "prepared" => RuntimeInstanceState::Prepared,
        "draining" => RuntimeInstanceState::Draining,
        "stopped" => RuntimeInstanceState::Stopped,
        "running" => RuntimeInstanceState::Running,
        _ => RuntimeInstanceState::Running,
    }
}

pub fn instance_from_labels(
    container_name: &str,
    container_id: Option<String>,
    labels: &HashMap<String, String>,
    address: String,
) -> RuntimeResult<RuntimeInstance> {
    let instance_id = required_label(container_name, labels, LABEL_INSTANCE)?;
    let service = required_label(container_name, labels, LABEL_SERVICE)?;
    let revision = required_label(container_name, labels, LABEL_REVISION)?;
    let state = labels
        .get(LABEL_STATE)
        .map_or(RuntimeInstanceState::Running, |value| parse_state(value));
    Ok(RuntimeInstance {
        instance_id: InstanceId::new(instance_id),
        service: ServiceName::new(service),
        revision: RevisionId::new(revision),
        address,
        backend_id: container_id,
        backend_name: Some(container_name.to_string()),
        pid: None,
        state,
    })
}

fn required_label(
    container_name: &str,
    labels: &HashMap<String, String>,
    label: &'static str,
) -> RuntimeResult<String> {
    labels
        .get(label)
        .cloned()
        .ok_or_else(|| RuntimeError::MissingDockerLabel {
            container_name: container_name.to_string(),
            label,
        })
}

#[cfg(test)]
mod tests {
    use mvp_deploy::{InstanceId, RevisionId};
    use mvp_identity::NodeId;
    use mvp_projection::ServiceName;

    use crate::RuntimeInstanceState;

    use super::{
        LABEL_INSTANCE, LABEL_MANAGED, LABEL_NODE, LABEL_REVISION, LABEL_SERVICE, LABEL_STATE,
        labels_for,
    };

    #[test]
    fn labels_capture_runtime_identity() {
        let labels = labels_for(
            &NodeId::new("node-a"),
            &InstanceId::new("web-1"),
            &ServiceName::new("web"),
            &RevisionId::new("rev-1"),
            RuntimeInstanceState::Running,
        );

        assert_eq!(labels.get(LABEL_MANAGED).map(String::as_str), Some("true"));
        assert_eq!(labels.get(LABEL_NODE).map(String::as_str), Some("node-a"));
        assert_eq!(
            labels.get(LABEL_INSTANCE).map(String::as_str),
            Some("web-1")
        );
        assert_eq!(labels.get(LABEL_SERVICE).map(String::as_str), Some("web"));
        assert_eq!(
            labels.get(LABEL_REVISION).map(String::as_str),
            Some("rev-1")
        );
        assert_eq!(labels.get(LABEL_STATE).map(String::as_str), Some("running"));
    }
}
