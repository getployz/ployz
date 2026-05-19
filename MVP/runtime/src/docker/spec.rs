use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use mvp_deploy::RevisionId;
use mvp_identity::NodeId;
use mvp_projection::ServiceName;

use crate::RuntimeInstanceSpec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerRuntimeConfig {
    pub node_id: NodeId,
    pub root: PathBuf,
    pub default_image: String,
    pub service_images: BTreeMap<String, String>,
    pub command: Option<Vec<String>>,
    pub network: Option<String>,
    pub service_port: u16,
    pub readiness_timeout: Duration,
    pub stop_timeout: Duration,
}

impl DockerRuntimeConfig {
    #[must_use]
    pub fn new(
        node_id: NodeId,
        root: impl Into<PathBuf>,
        default_image: impl Into<String>,
    ) -> Self {
        Self {
            node_id,
            root: root.into(),
            default_image: default_image.into(),
            service_images: BTreeMap::new(),
            command: None,
            network: None,
            service_port: 8080,
            readiness_timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(10),
        }
    }

    #[must_use]
    pub fn with_service_image(
        mut self,
        service: &ServiceName,
        revision: Option<&RevisionId>,
        image: impl Into<String>,
    ) -> Self {
        self.service_images
            .insert(image_key(service, revision), image.into());
        self
    }

    #[must_use]
    pub fn with_command(mut self, command: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.command = Some(command.into_iter().map(Into::into).collect());
        self
    }

    #[must_use]
    pub fn with_network(mut self, network: impl Into<String>) -> Self {
        self.network = Some(network.into());
        self
    }

    #[must_use]
    pub fn with_service_port(mut self, port: u16) -> Self {
        self.service_port = port;
        self
    }

    #[must_use]
    pub fn with_readiness_timeout(mut self, timeout: Duration) -> Self {
        self.readiness_timeout = timeout;
        self
    }

    #[must_use]
    pub fn with_stop_timeout(mut self, timeout: Duration) -> Self {
        self.stop_timeout = timeout;
        self
    }

    pub fn image_for(&self, spec: &RuntimeInstanceSpec) -> String {
        self.service_images
            .get(&image_key(&spec.service, Some(&spec.revision)))
            .or_else(|| self.service_images.get(&image_key(&spec.service, None)))
            .cloned()
            .unwrap_or_else(|| self.default_image.clone())
    }
}

pub fn container_name(node_id: &NodeId, spec: &RuntimeInstanceSpec) -> String {
    stable_name(format!(
        "ployz-mvp-{}-{}",
        node_id.as_str(),
        spec.instance_id.as_str()
    ))
}

fn image_key(service: &ServiceName, revision: Option<&RevisionId>) -> String {
    match revision {
        Some(revision) => format!("{}@{}", service.as_str(), revision.as_str()),
        None => service.as_str().to_string(),
    }
}

fn stable_name(value: String) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use mvp_deploy::{InstanceId, RevisionId};
    use mvp_identity::NodeId;
    use mvp_projection::ServiceName;

    use crate::RuntimeInstanceSpec;

    use super::{DockerRuntimeConfig, container_name};

    #[test]
    fn container_names_are_deterministic_and_docker_safe() {
        let spec = RuntimeInstanceSpec::new(
            InstanceId::new("web/1"),
            ServiceName::new("web"),
            RevisionId::new("rev-1"),
        );

        assert_eq!(
            container_name(&NodeId::new("node-a"), &spec),
            "ployz-mvp-node-a-web-1"
        );
    }

    #[test]
    fn image_lookup_prefers_revision_specific_mapping() {
        let spec = RuntimeInstanceSpec::new(
            InstanceId::new("web-1"),
            ServiceName::new("web"),
            RevisionId::new("rev-2"),
        );
        let config = DockerRuntimeConfig::new(NodeId::new("node-a"), "runtime", "default:latest")
            .with_service_image(&ServiceName::new("web"), None, "web:latest")
            .with_service_image(
                &ServiceName::new("web"),
                Some(&RevisionId::new("rev-2")),
                "web:v2",
            );

        assert_eq!(config.image_for(&spec), "web:v2");
    }
}
