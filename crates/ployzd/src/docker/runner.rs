use super::network::{ENDPOINT_NETWORK_NAME, endpoint_network_create_request};
use crate::docker::labels::{
    MANAGED_LABEL, ManagedContainerIdentity, ManagedContainerLabelError, ManagedContainerLabels,
};
use crate::machine_runtime::runner::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    MachineContainerRunner, MachineContainerRunnerError, MachineLogReader, MachineLogReaderError,
    MachineLogTail,
};
use bollard::Docker;
use bollard::errors::Error as BollardError;
use bollard::models::{
    ContainerCreateBody, ContainerSummary, ContainerSummaryNetworkSettings,
    ContainerSummaryStateEnum, EndpointSettings, HostConfig, NetworkingConfig,
};
use bollard::query_parameters::{
    CreateImageOptionsBuilder, InspectNetworkOptions, ListContainersOptionsBuilder,
    LogsOptionsBuilder, RemoveContainerOptionsBuilder, StopContainerOptionsBuilder,
};
use futures_util::StreamExt;
use ployz_core::ids::{ContainerId, SubjectTokenError};
use ployz_core::machine_runtime::ContainerEndpoint;
use ployz_core::ops::RoutePort;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;

const DEFAULT_LOG_TAIL_LINES: u16 = 200;
const MAX_LOG_TAIL_LINES: u16 = 1_000;
const MAX_LOG_TAIL_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct DockerManagedContainerRunner {
    docker: DockerHandle,
    endpoint_network_subnet: String,
    endpoint_bridge_ifname: String,
}

/// How the runner reaches the Docker daemon: an already-built client, or a
/// client built from local defaults on first use so the machine runtime can
/// start before Docker is reachable.
#[derive(Debug, Clone)]
enum DockerHandle {
    Connected(Docker),
    LazyLocalDefaults(Arc<tokio::sync::OnceCell<Docker>>),
}

impl DockerManagedContainerRunner {
    pub fn local_defaults(
        endpoint_network_subnet: impl Into<String>,
        endpoint_bridge_ifname: impl Into<String>,
    ) -> Result<Self, DockerManagedContainerRunnerConnectError> {
        let docker = connect_local_defaults()?;
        Ok(Self {
            docker: DockerHandle::Connected(docker),
            endpoint_network_subnet: endpoint_network_subnet.into(),
            endpoint_bridge_ifname: endpoint_bridge_ifname.into(),
        })
    }

    #[must_use]
    pub fn lazy_local_defaults(
        endpoint_network_subnet: String,
        endpoint_bridge_ifname: String,
    ) -> Self {
        Self {
            docker: DockerHandle::LazyLocalDefaults(Arc::new(tokio::sync::OnceCell::new())),
            endpoint_network_subnet,
            endpoint_bridge_ifname,
        }
    }

    async fn docker(&self) -> Result<&Docker, DockerManagedContainerRunnerConnectError> {
        match &self.docker {
            DockerHandle::Connected(docker) => Ok(docker),
            DockerHandle::LazyLocalDefaults(cell) => {
                cell.get_or_try_init(|| async { connect_local_defaults() })
                    .await
            }
        }
    }
}

fn connect_local_defaults() -> Result<Docker, DockerManagedContainerRunnerConnectError> {
    Docker::connect_with_local_defaults().map_err(|source| {
        DockerManagedContainerRunnerConnectError {
            message: source.to_string(),
        }
    })
}

impl MachineContainerRunner for DockerManagedContainerRunner {
    async fn existing_managed_containers(
        &self,
    ) -> Result<Vec<ExistingManagedContainer>, MachineContainerRunnerError> {
        let docker =
            self.docker()
                .await
                .map_err(|error| MachineContainerRunnerError::ListExisting {
                    message: error.to_string(),
                })?;
        let summaries = docker
            .list_containers(Some(managed_container_list_options()))
            .await
            .map_err(|error| MachineContainerRunnerError::ListExisting {
                message: error.to_string(),
            })?;

        summaries
            .into_iter()
            .map(existing_container_from_summary)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| MachineContainerRunnerError::ListExisting {
                message: error.to_string(),
            })
    }

    async fn ensure_endpoint_network(&self) -> Result<(), MachineContainerRunnerError> {
        self.ensure_endpoint_network_inner().await
    }

    async fn create_managed_container(
        &self,
        command: CreateManagedContainer,
    ) -> Result<ContainerId, MachineContainerRunnerError> {
        self.pull_image(command.image.as_str()).await?;

        let requires_endpoint_network = command.endpoint.is_some();
        if requires_endpoint_network {
            self.ensure_endpoint_network_inner().await?;
        }

        let docker = self
            .docker()
            .await
            .map_err(|error| MachineContainerRunnerError::Create {
                message: error.to_string(),
            })?;
        let response = docker
            .create_container(None, create_body(command))
            .await
            .map_err(|error| MachineContainerRunnerError::Create {
                message: error.to_string(),
            })?;
        ContainerId::try_new(response.id).map_err(|error| MachineContainerRunnerError::Create {
            message: error.to_string(),
        })
    }

    async fn start_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> Result<(), MachineContainerRunnerError> {
        let docker = self
            .docker()
            .await
            .map_err(|error| MachineContainerRunnerError::Start {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        docker
            .start_container(container_id.as_str(), None)
            .await
            .map_err(|error| MachineContainerRunnerError::Start {
                container_id: container_id.clone(),
                message: error.to_string(),
            })
    }

    async fn remove_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), MachineContainerRunnerError> {
        let existing = self
            .existing_managed_containers()
            .await?
            .into_iter()
            .find(|container| container.container_id == *container_id);
        let Some(existing) = existing else {
            return Ok(());
        };
        if existing.labels.identity() != *expected_identity {
            return Err(MachineContainerRunnerError::Remove {
                container_id: container_id.clone(),
                message: format!(
                    "container identity did not match cleanup target: expected {:?}, found {:?}",
                    expected_identity,
                    existing.labels.identity()
                ),
            });
        }

        let docker = self
            .docker()
            .await
            .map_err(|error| MachineContainerRunnerError::Remove {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        let options = RemoveContainerOptionsBuilder::new().force(true).build();
        match docker
            .remove_container(container_id.as_str(), Some(options))
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if is_container_missing(&error) => Ok(()),
            Err(error) => Err(MachineContainerRunnerError::Remove {
                container_id: container_id.clone(),
                message: error.to_string(),
            }),
        }
    }

    async fn stop_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), MachineContainerRunnerError> {
        let existing = self
            .existing_managed_containers()
            .await?
            .into_iter()
            .find(|container| container.container_id == *container_id);
        let Some(existing) = existing else {
            return Ok(());
        };
        if existing.labels.identity() != *expected_identity {
            return Err(MachineContainerRunnerError::Stop {
                container_id: container_id.clone(),
                message: format!(
                    "container identity did not match stop target: expected {:?}, found {:?}",
                    expected_identity,
                    existing.labels.identity()
                ),
            });
        }

        if !matches!(
            existing.state,
            ExistingManagedContainerState::Running { .. }
        ) {
            return Ok(());
        }

        let docker = self
            .docker()
            .await
            .map_err(|error| MachineContainerRunnerError::Stop {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        let options = StopContainerOptionsBuilder::new().build();
        match docker
            .stop_container(container_id.as_str(), Some(options))
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if is_container_missing(&error) => Ok(()),
            Err(error) => Err(MachineContainerRunnerError::Stop {
                container_id: container_id.clone(),
                message: error.to_string(),
            }),
        }
    }
}

impl MachineLogReader for DockerManagedContainerRunner {
    async fn tail_container_logs(
        &self,
        container_id: &ContainerId,
        tail_lines: Option<u16>,
    ) -> Result<MachineLogTail, MachineLogReaderError> {
        let docker = self
            .docker()
            .await
            .map_err(|error| MachineLogReaderError::ReadFailed {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        let mut output = Vec::new();
        let mut truncated = false;
        let mut stream = docker.logs(
            container_id.as_str(),
            Some(logs_options(capped_tail_lines(tail_lines))),
        );

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => {
                    let remaining = MAX_LOG_TAIL_BYTES.saturating_sub(output.len());
                    let bytes = chunk.as_ref();
                    if bytes.len() > remaining {
                        if let Some(capped) = bytes.get(..remaining) {
                            output.extend_from_slice(capped);
                        }
                        truncated = true;
                        break;
                    }
                    output.extend_from_slice(bytes);
                }
                Err(error) if is_container_missing(&error) => {
                    return Err(MachineLogReaderError::NotFound {
                        container_id: container_id.clone(),
                    });
                }
                Err(error) => {
                    return Err(MachineLogReaderError::ReadFailed {
                        container_id: container_id.clone(),
                        message: error.to_string(),
                    });
                }
            }
        }

        Ok(MachineLogTail {
            text: String::from_utf8_lossy(&output).into_owned(),
            truncated,
        })
    }
}

impl DockerManagedContainerRunner {
    async fn pull_image(&self, image: &str) -> Result<(), MachineContainerRunnerError> {
        let docker = self
            .docker()
            .await
            .map_err(|error| MachineContainerRunnerError::Create {
                message: error.to_string(),
            })?;
        let options = CreateImageOptionsBuilder::new().from_image(image).build();
        let mut stream = docker.create_image(Some(options), None, None);

        while let Some(result) = stream.next().await {
            result.map_err(|error| MachineContainerRunnerError::Create {
                message: format!("pull Docker image {image}: {error}"),
            })?;
        }

        Ok(())
    }

    async fn ensure_endpoint_network_inner(&self) -> Result<(), MachineContainerRunnerError> {
        let docker = self
            .docker()
            .await
            .map_err(|error| MachineContainerRunnerError::Create {
                message: error.to_string(),
            })?;
        if docker
            .inspect_network(ENDPOINT_NETWORK_NAME, None::<InspectNetworkOptions>)
            .await
            .is_ok()
        {
            return Ok(());
        }

        let request = endpoint_network_create_request(
            &self.endpoint_network_subnet,
            &self.endpoint_bridge_ifname,
        );

        match docker.create_network(request).await {
            Ok(_) => Ok(()),
            Err(error) if is_network_already_exists(&error) => Ok(()),
            Err(error) => {
                if docker
                    .inspect_network(ENDPOINT_NETWORK_NAME, None::<InspectNetworkOptions>)
                    .await
                    .is_ok()
                {
                    Ok(())
                } else {
                    Err(MachineContainerRunnerError::EnsureEndpointNetwork {
                        message: format!("ensure Docker network {ENDPOINT_NETWORK_NAME}: {error}"),
                    })
                }
            }
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

fn logs_options(tail_lines: u16) -> bollard::query_parameters::LogsOptions {
    let tail = tail_lines.to_string();
    LogsOptionsBuilder::default()
        .stdout(true)
        .stderr(true)
        .tail(&tail)
        .build()
}

const fn capped_tail_lines(tail_lines: Option<u16>) -> u16 {
    match tail_lines {
        Some(lines) if lines > MAX_LOG_TAIL_LINES => MAX_LOG_TAIL_LINES,
        Some(lines) => lines,
        None => DEFAULT_LOG_TAIL_LINES,
    }
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
    use ployz_core::ids::{NamespaceRevisionEntryId, OperationId, ServiceId, StepId};
    use ployz_core::machine_runtime::ManagedContainerKind;

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
            endpoint: Some(crate::machine_runtime::protocol::ContainerEndpointRequest {
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
            revision_id: namespace_revision_entry_id("entry_2"),
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

    fn namespace_revision_entry_id(value: &str) -> NamespaceRevisionEntryId {
        NamespaceRevisionEntryId::try_new(value).expect("valid namespace revision entry id")
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
