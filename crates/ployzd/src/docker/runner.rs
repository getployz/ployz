use crate::docker::labels::{
    MANAGED_LABEL, ManagedContainerIdentity, ManagedContainerLabelError, ManagedContainerLabels,
};
use crate::node_agent::runtime::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    NodeContainerRunner, NodeContainerRunnerError,
};
use bollard::Docker;
use bollard::errors::Error as BollardError;
use bollard::models::{
    ContainerCreateBody, ContainerSummary, ContainerSummaryNetworkSettings,
    ContainerSummaryStateEnum, EndpointSettings, HostConfig, NetworkCreateRequest,
    NetworkingConfig,
};
use bollard::query_parameters::{ListContainersOptionsBuilder, RemoveContainerOptionsBuilder};
use ployz_core::ids::{ContainerId, SubjectTokenError};
use ployz_core::node::ContainerEndpoint;
use ployz_core::ops::RoutePort;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::net::IpAddr;

const ENDPOINT_NETWORK_NAME: &str = "ployz";

#[derive(Debug, Clone)]
pub struct DockerManagedContainerRunner {
    docker: Docker,
}

#[derive(Debug, Clone, Copy)]
pub struct LazyLocalDockerManagedContainerRunner;

impl DockerManagedContainerRunner {
    pub fn local_defaults() -> Result<Self, DockerManagedContainerRunnerConnectError> {
        let docker = Docker::connect_with_local_defaults().map_err(|source| {
            DockerManagedContainerRunnerConnectError {
                message: source.to_string(),
            }
        })?;
        Ok(Self { docker })
    }

    #[must_use]
    pub fn new(docker: Docker) -> Self {
        Self { docker }
    }
}

impl NodeContainerRunner for LazyLocalDockerManagedContainerRunner {
    async fn existing_managed_containers(
        &self,
    ) -> Result<Vec<ExistingManagedContainer>, NodeContainerRunnerError> {
        let runner = connect_local_docker_for_list()?;
        runner.existing_managed_containers().await
    }

    async fn create_managed_container(
        &self,
        command: CreateManagedContainer,
    ) -> Result<ContainerId, NodeContainerRunnerError> {
        let runner = connect_local_docker_for_create()?;
        runner.create_managed_container(command).await
    }

    async fn start_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> Result<(), NodeContainerRunnerError> {
        let runner = DockerManagedContainerRunner::local_defaults().map_err(|error| {
            NodeContainerRunnerError::Start {
                container_id: container_id.clone(),
                message: error.to_string(),
            }
        })?;
        runner.start_managed_container(container_id).await
    }

    async fn remove_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), NodeContainerRunnerError> {
        let runner = DockerManagedContainerRunner::local_defaults().map_err(|error| {
            NodeContainerRunnerError::Remove {
                container_id: container_id.clone(),
                message: error.to_string(),
            }
        })?;
        runner
            .remove_managed_container(container_id, expected_identity)
            .await
    }
}

impl NodeContainerRunner for DockerManagedContainerRunner {
    async fn existing_managed_containers(
        &self,
    ) -> Result<Vec<ExistingManagedContainer>, NodeContainerRunnerError> {
        let summaries = self
            .docker
            .list_containers(Some(managed_container_list_options()))
            .await
            .map_err(|error| NodeContainerRunnerError::ListExisting {
                message: error.to_string(),
            })?;

        summaries
            .into_iter()
            .map(existing_container_from_summary)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| NodeContainerRunnerError::ListExisting {
                message: error.to_string(),
            })
    }

    async fn create_managed_container(
        &self,
        command: CreateManagedContainer,
    ) -> Result<ContainerId, NodeContainerRunnerError> {
        let requires_endpoint_network = command.endpoint.is_some();
        if requires_endpoint_network {
            self.ensure_endpoint_network().await?;
        }

        let response = self
            .docker
            .create_container(None, create_body(command))
            .await
            .map_err(|error| NodeContainerRunnerError::Create {
                message: error.to_string(),
            })?;
        ContainerId::try_new(response.id).map_err(|error| NodeContainerRunnerError::Create {
            message: error.to_string(),
        })
    }

    async fn start_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> Result<(), NodeContainerRunnerError> {
        self.docker
            .start_container(container_id.as_str(), None)
            .await
            .map_err(|error| NodeContainerRunnerError::Start {
                container_id: container_id.clone(),
                message: error.to_string(),
            })
    }

    async fn remove_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), NodeContainerRunnerError> {
        let existing = self
            .existing_managed_containers()
            .await?
            .into_iter()
            .find(|container| container.container_id == *container_id);
        let Some(existing) = existing else {
            return Ok(());
        };
        if existing.labels.identity() != *expected_identity {
            return Err(NodeContainerRunnerError::Remove {
                container_id: container_id.clone(),
                message: format!(
                    "container identity did not match cleanup target: expected {:?}, found {:?}",
                    expected_identity,
                    existing.labels.identity()
                ),
            });
        }

        let options = RemoveContainerOptionsBuilder::new().force(true).build();
        match self
            .docker
            .remove_container(container_id.as_str(), Some(options))
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if is_container_missing(&error) => Ok(()),
            Err(error) => Err(NodeContainerRunnerError::Remove {
                container_id: container_id.clone(),
                message: error.to_string(),
            }),
        }
    }
}

impl DockerManagedContainerRunner {
    async fn ensure_endpoint_network(&self) -> Result<(), NodeContainerRunnerError> {
        let request = NetworkCreateRequest {
            name: ENDPOINT_NETWORK_NAME.to_owned(),
            driver: Some("bridge".to_owned()),
            labels: Some(HashMap::from([(
                MANAGED_LABEL.to_owned(),
                "true".to_owned(),
            )])),
            ..Default::default()
        };

        match self.docker.create_network(request).await {
            Ok(_) => Ok(()),
            Err(error) if is_network_already_exists(&error) => Ok(()),
            Err(error) => Err(NodeContainerRunnerError::Create {
                message: format!("ensure Docker network {ENDPOINT_NETWORK_NAME}: {error}"),
            }),
        }?;

        Ok(())
    }
}

fn is_network_already_exists(error: &BollardError) -> bool {
    matches!(
        error,
        BollardError::DockerResponseServerError {
            status_code: 409,
            message
        } if message.contains("already exists")
    )
}

fn is_container_missing(error: &BollardError) -> bool {
    matches!(
        error,
        BollardError::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

fn connect_local_docker_for_list() -> Result<DockerManagedContainerRunner, NodeContainerRunnerError>
{
    DockerManagedContainerRunner::local_defaults().map_err(|error| {
        NodeContainerRunnerError::ListExisting {
            message: error.to_string(),
        }
    })
}

fn connect_local_docker_for_create()
-> Result<DockerManagedContainerRunner, NodeContainerRunnerError> {
    DockerManagedContainerRunner::local_defaults().map_err(|error| {
        NodeContainerRunnerError::Create {
            message: error.to_string(),
        }
    })
}

fn docker_container_state(
    state: ContainerSummaryStateEnum,
    labels: &ManagedContainerLabels,
    network_settings: Option<ContainerSummaryNetworkSettings>,
) -> Result<ExistingManagedContainerState, DockerManagedContainerSummaryError> {
    match state {
        ContainerSummaryStateEnum::RUNNING => Ok(ExistingManagedContainerState::Running {
            endpoint: container_endpoint(labels.endpoint_port, network_settings)?,
        }),
        ContainerSummaryStateEnum::CREATED | ContainerSummaryStateEnum::EXITED => {
            Ok(ExistingManagedContainerState::StartableStopped)
        }
        ContainerSummaryStateEnum::PAUSED
        | ContainerSummaryStateEnum::RESTARTING
        | ContainerSummaryStateEnum::REMOVING
        | ContainerSummaryStateEnum::DEAD
        | ContainerSummaryStateEnum::STOPPING => Ok(ExistingManagedContainerState::NotStartable {
            description: state.to_string(),
        }),
        ContainerSummaryStateEnum::EMPTY => Err(DockerManagedContainerSummaryError::MissingState),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerManagedContainerRunnerConnectError {
    message: String,
}

impl fmt::Display for DockerManagedContainerRunnerConnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to connect to local Docker: {}",
            self.message
        )
    }
}

impl std::error::Error for DockerManagedContainerRunnerConnectError {}

fn managed_container_list_options() -> bollard::query_parameters::ListContainersOptions {
    let filters = HashMap::from([("label".to_owned(), vec![format!("{MANAGED_LABEL}=true")])]);
    ListContainersOptionsBuilder::new()
        .all(true)
        .filters(&filters)
        .build()
}

fn create_body(command: CreateManagedContainer) -> ContainerCreateBody {
    let endpoint_port = command.endpoint.as_ref().map(|endpoint| endpoint.port);
    let exposed_ports = endpoint_port.map(|port| vec![format!("{}/tcp", port.get())]);
    let host_config = endpoint_port.map(|_| HostConfig {
        network_mode: Some(ENDPOINT_NETWORK_NAME.to_owned()),
        ..Default::default()
    });
    let networking_config = endpoint_port.map(|_| NetworkingConfig {
        endpoints_config: Some(HashMap::from([(
            ENDPOINT_NETWORK_NAME.to_owned(),
            EndpointSettings::default(),
        )])),
    });

    ContainerCreateBody {
        image: Some(command.image.as_str().to_owned()),
        labels: Some(hashmap_from_btree(command.labels.render())),
        exposed_ports,
        host_config,
        networking_config,
        ..Default::default()
    }
}

fn existing_container_from_summary(
    summary: ContainerSummary,
) -> Result<ExistingManagedContainer, DockerManagedContainerSummaryError> {
    let id = summary
        .id
        .ok_or(DockerManagedContainerSummaryError::MissingId)?;
    let labels = summary
        .labels
        .ok_or(DockerManagedContainerSummaryError::MissingLabels)?;
    let state = summary
        .state
        .ok_or(DockerManagedContainerSummaryError::MissingState)?;

    let labels = ManagedContainerLabels::parse(&btree_from_hashmap(labels))
        .map_err(DockerManagedContainerSummaryError::InvalidLabels)?;
    Ok(ExistingManagedContainer {
        container_id: ContainerId::try_new(id)
            .map_err(DockerManagedContainerSummaryError::InvalidContainerId)?,
        state: docker_container_state(state, &labels, summary.network_settings)?,
        labels,
    })
}

fn container_endpoint(
    port: Option<RoutePort>,
    network_settings: Option<ContainerSummaryNetworkSettings>,
) -> Result<Option<ContainerEndpoint>, DockerManagedContainerSummaryError> {
    let Some(port) = port else {
        return Ok(None);
    };
    let Some(network_settings) = network_settings else {
        return Ok(None);
    };
    let Some(networks) = network_settings.networks else {
        return Ok(None);
    };

    let Some(endpoint) = networks.get(ENDPOINT_NETWORK_NAME) else {
        return Ok(None);
    };
    let Some(ip) = endpoint
        .ip_address
        .as_ref()
        .or(endpoint.global_ipv6_address.as_ref())
        .filter(|ip| !ip.is_empty())
    else {
        return Ok(None);
    };

    Ok(Some(ContainerEndpoint {
        ip: ip.parse::<IpAddr>().map_err(|_| {
            DockerManagedContainerSummaryError::InvalidEndpointIp {
                value: ip.to_owned(),
            }
        })?,
        port,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DockerManagedContainerSummaryError {
    MissingId,
    MissingLabels,
    MissingState,
    InvalidEndpointIp { value: String },
    InvalidContainerId(SubjectTokenError),
    InvalidLabels(ManagedContainerLabelError),
}

impl fmt::Display for DockerManagedContainerSummaryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingId => formatter.write_str("managed Docker container is missing id"),
            Self::MissingLabels => {
                formatter.write_str("managed Docker container is missing labels")
            }
            Self::MissingState => formatter.write_str("managed Docker container is missing state"),
            Self::InvalidEndpointIp { value } => {
                write!(
                    formatter,
                    "managed Docker container has invalid endpoint ip: {value}"
                )
            }
            Self::InvalidContainerId(error) => {
                write!(
                    formatter,
                    "managed Docker container has invalid id: {error}"
                )
            }
            Self::InvalidLabels(error) => {
                write!(
                    formatter,
                    "managed Docker container has invalid labels: {error:?}"
                )
            }
        }
    }
}

fn hashmap_from_btree(map: BTreeMap<String, String>) -> HashMap<String, String> {
    map.into_iter().collect()
}

fn btree_from_hashmap(map: HashMap<String, String>) -> BTreeMap<String, String> {
    map.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::ImageReference;
    use ployz_core::ids::{OperationId, RevisionId, ServiceId, StepId};
    use ployz_core::node::ManagedContainerKind;

    #[test]
    fn list_options_filter_to_managed_containers() {
        let options = managed_container_list_options();

        assert!(options.all);
        assert_eq!(
            options.filters,
            Some(HashMap::from([(
                "label".to_owned(),
                vec!["plz.managed=true".to_owned()]
            )]))
        );
    }

    #[test]
    fn create_body_preserves_image_and_labels() {
        let body = create_body(CreateManagedContainer {
            image: image("ghcr.io/acme/api:rev-2"),
            endpoint: None,
            labels: managed_labels(),
        });

        assert_eq!(body.image, Some("ghcr.io/acme/api:rev-2".to_owned()));
        assert_eq!(
            body.labels,
            Some(hashmap_from_btree(managed_labels().render()))
        );
    }

    #[test]
    fn create_body_exposes_endpoint_port_when_routable() {
        let labels = ManagedContainerLabels {
            endpoint_port: Some(route_port(8080)),
            ..managed_labels()
        };
        let body = create_body(CreateManagedContainer {
            image: image("ghcr.io/acme/api:rev-2"),
            endpoint: Some(crate::node_runtime_types::ContainerEndpointRequest {
                port: route_port(8080),
            }),
            labels,
        });

        assert_eq!(body.exposed_ports, Some(vec!["8080/tcp".to_owned()]));
        assert_eq!(
            body.labels.as_ref().and_then(|labels| {
                labels
                    .get(crate::docker::labels::ENDPOINT_PORT_LABEL)
                    .cloned()
            }),
            Some("8080".to_owned())
        );
        assert_eq!(
            body.host_config.and_then(|config| config.network_mode),
            Some(ENDPOINT_NETWORK_NAME.to_owned())
        );
        assert_eq!(
            body.networking_config
                .and_then(|config| config.endpoints_config)
                .map(|endpoints| endpoints.contains_key(ENDPOINT_NETWORK_NAME)),
            Some(true)
        );
    }

    #[test]
    fn create_body_without_endpoint_has_no_endpoint_label_or_networking() {
        let body = create_body(CreateManagedContainer {
            image: image("ghcr.io/acme/api:rev-2"),
            endpoint: None,
            labels: managed_labels(),
        });

        assert_eq!(
            body.labels.as_ref().and_then(|labels| labels
                .get(crate::docker::labels::ENDPOINT_PORT_LABEL)
                .cloned()),
            None
        );
        assert_eq!(body.exposed_ports, None);
        assert_eq!(body.host_config, None);
        assert_eq!(body.networking_config, None);
    }

    #[test]
    fn endpoint_network_create_conflict_is_idempotent() {
        assert!(is_network_already_exists(
            &BollardError::DockerResponseServerError {
                status_code: 409,
                message: "network with name ployz already exists".to_owned(),
            }
        ));
        assert!(!is_network_already_exists(
            &BollardError::DockerResponseServerError {
                status_code: 409,
                message: "different conflict".to_owned(),
            }
        ));
    }

    #[test]
    fn summary_with_managed_labels_becomes_existing_container() {
        let summary = ContainerSummary {
            id: Some("0123456789abcdef".to_owned()),
            labels: Some(hashmap_from_btree(managed_labels().render())),
            state: Some(ContainerSummaryStateEnum::RUNNING),
            ..Default::default()
        };

        assert_eq!(
            existing_container_from_summary(summary).expect("summary parses"),
            ExistingManagedContainer {
                container_id: container_id("0123456789abcdef"),
                labels: managed_labels(),
                state: ExistingManagedContainerState::Running { endpoint: None },
            }
        );
    }

    #[test]
    fn running_summary_with_endpoint_label_and_network_ip_becomes_routable_container() {
        let labels = ManagedContainerLabels {
            endpoint_port: Some(route_port(8080)),
            ..managed_labels()
        };
        let summary = ContainerSummary {
            id: Some("0123456789abcdef".to_owned()),
            labels: Some(hashmap_from_btree(labels.render())),
            state: Some(ContainerSummaryStateEnum::RUNNING),
            network_settings: Some(ContainerSummaryNetworkSettings {
                networks: Some(HashMap::from([(
                    "ployz".to_owned(),
                    bollard::models::EndpointSettings {
                        ip_address: Some("10.42.0.9".to_owned()),
                        ..Default::default()
                    },
                )])),
            }),
            ..Default::default()
        };

        assert_eq!(
            existing_container_from_summary(summary)
                .expect("summary parses")
                .state,
            ExistingManagedContainerState::Running {
                endpoint: Some(ContainerEndpoint {
                    ip: "10.42.0.9".parse().expect("valid endpoint ip"),
                    port: route_port(8080),
                }),
            }
        );
    }

    #[test]
    fn running_summary_uses_only_the_ployz_network_for_endpoint_evidence() {
        let labels = ManagedContainerLabels {
            endpoint_port: Some(route_port(8080)),
            ..managed_labels()
        };
        let summary = ContainerSummary {
            id: Some("0123456789abcdef".to_owned()),
            labels: Some(hashmap_from_btree(labels.render())),
            state: Some(ContainerSummaryStateEnum::RUNNING),
            network_settings: Some(ContainerSummaryNetworkSettings {
                networks: Some(HashMap::from([
                    (
                        "ployz".to_owned(),
                        bollard::models::EndpointSettings {
                            ip_address: Some("10.42.0.9".to_owned()),
                            ..Default::default()
                        },
                    ),
                    (
                        "bridge".to_owned(),
                        bollard::models::EndpointSettings {
                            ip_address: Some("172.17.0.2".to_owned()),
                            ..Default::default()
                        },
                    ),
                ])),
            }),
            ..Default::default()
        };

        assert_eq!(
            existing_container_from_summary(summary)
                .expect("summary parses")
                .state,
            ExistingManagedContainerState::Running {
                endpoint: Some(ContainerEndpoint {
                    ip: "10.42.0.9".parse().expect("valid endpoint ip"),
                    port: route_port(8080),
                }),
            }
        );
    }

    #[test]
    fn running_summary_without_ployz_network_is_running_but_unroutable() {
        let labels = ManagedContainerLabels {
            endpoint_port: Some(route_port(8080)),
            ..managed_labels()
        };
        let summary = ContainerSummary {
            id: Some("0123456789abcdef".to_owned()),
            labels: Some(hashmap_from_btree(labels.render())),
            state: Some(ContainerSummaryStateEnum::RUNNING),
            network_settings: Some(ContainerSummaryNetworkSettings {
                networks: Some(HashMap::from([(
                    "bridge".to_owned(),
                    bollard::models::EndpointSettings {
                        ip_address: Some("172.17.0.2".to_owned()),
                        ..Default::default()
                    },
                )])),
            }),
            ..Default::default()
        };

        assert_eq!(
            existing_container_from_summary(summary)
                .expect("summary parses")
                .state,
            ExistingManagedContainerState::Running { endpoint: None }
        );
    }

    #[test]
    fn summary_with_created_state_is_not_reusable_as_running() {
        let summary = ContainerSummary {
            id: Some("0123456789abcdef".to_owned()),
            labels: Some(hashmap_from_btree(managed_labels().render())),
            state: Some(ContainerSummaryStateEnum::CREATED),
            ..Default::default()
        };

        assert_eq!(
            existing_container_from_summary(summary)
                .expect("summary parses")
                .state,
            ExistingManagedContainerState::StartableStopped
        );
    }

    #[test]
    fn summary_with_paused_state_is_not_startable() {
        let summary = ContainerSummary {
            id: Some("0123456789abcdef".to_owned()),
            labels: Some(hashmap_from_btree(managed_labels().render())),
            state: Some(ContainerSummaryStateEnum::PAUSED),
            ..Default::default()
        };

        assert_eq!(
            existing_container_from_summary(summary)
                .expect("summary parses")
                .state,
            ExistingManagedContainerState::NotStartable {
                description: "paused".to_owned(),
            }
        );
    }

    #[test]
    fn summary_without_labels_is_not_silently_accepted() {
        let summary = ContainerSummary {
            id: Some("0123456789abcdef".to_owned()),
            state: Some(ContainerSummaryStateEnum::RUNNING),
            labels: None,
            ..Default::default()
        };

        assert_eq!(
            existing_container_from_summary(summary),
            Err(DockerManagedContainerSummaryError::MissingLabels)
        );
    }

    fn managed_labels() -> ManagedContainerLabels {
        ManagedContainerLabels {
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_2"),
            operation_id: operation_id("op_123"),
            step_id: step_id("run_1"),
            kind: ManagedContainerKind::Service,
            endpoint_port: None,
        }
    }

    fn route_port(value: u16) -> RoutePort {
        RoutePort::try_new(value).expect("valid route port")
    }

    fn service_id(value: &str) -> ServiceId {
        ServiceId::try_new(value).expect("valid service id")
    }

    fn revision_id(value: &str) -> RevisionId {
        RevisionId::try_new(value).expect("valid revision id")
    }

    fn operation_id(value: &str) -> OperationId {
        OperationId::try_new(value).expect("valid operation id")
    }

    fn step_id(value: &str) -> StepId {
        StepId::try_new(value).expect("valid step id")
    }

    fn container_id(value: &str) -> ContainerId {
        ContainerId::try_new(value).expect("valid container id")
    }

    fn image(value: &str) -> ImageReference {
        ImageReference::try_new(value).expect("valid image")
    }
}
