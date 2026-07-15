use super::labels::{self, MANAGED_LABEL, ManagedContainerLabelError};
use super::network::{
    ENDPOINT_NETWORK_NAME, ensure_endpoint_network, is_docker_object_missing,
    read_endpoint_network_status, require_endpoint_network,
};
use crate::roles::machine::execution::host_dataplane::{WireGuardMtuPolicy, resolve_wireguard_mtu};
use crate::roles::machine::protocol::MachineImagePull;
use crate::roles::machine::runner::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    MachineContainerRunner, MachineContainerRunnerError, MachineLogQuery, MachineLogReader,
    MachineLogReaderError, MachineLogTail, MachineLogTimestamps,
};
use crate::roles::machine::volume::docker_volume_name;
use bollard::Docker;
use bollard::auth::DockerCredentials;
use bollard::errors::Error as BollardError;
use bollard::models::{
    ContainerCreateBody, ContainerSummary, ContainerSummaryHealthStatusEnum,
    ContainerSummaryNetworkSettings, ContainerSummaryStateEnum, EndpointSettings, HealthConfig,
    HealthStatusEnum, HostConfig, Mount, MountType, NetworkingConfig, RestartPolicy,
    RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    CreateImageOptionsBuilder, InspectContainerOptions, ListContainersOptionsBuilder,
    LogsOptionsBuilder, RemoveContainerOptionsBuilder, RemoveVolumeOptionsBuilder,
    RestartContainerOptions, StopContainerOptionsBuilder,
};
use futures_util::StreamExt;
use ployz_core::deploy::{
    ContainerEntrypoint, ContainerHealthcheck, ContainerHealthcheckTest, ContainerRestartPolicy,
    ImageReference, RegistryCredential,
};
use ployz_core::ids::{ContainerId, SubjectTokenError};
use ployz_core::image::OciDigest;
use ployz_core::machine::runtime::{
    ContainerHealth, ManagedContainerHealthStatus, ManagedContainerIdentity,
};
use ployz_core::network::{
    EndpointBridgeStatus, INTERNAL_DNS_SUFFIX, MachineEndpointSubnet, endpoint_bridge_gateway_ipv4,
};
use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const DEFAULT_LOG_TAIL_LINES: u16 = 200;
const MAX_LOG_TAIL_LINES: u16 = 1_000;
const MAX_LOG_TAIL_BYTES: usize = 64 * 1024;
const REGISTRY_RESOLVE_RETRY_DELAYS: [Duration; 2] =
    [Duration::from_millis(250), Duration::from_secs(1)];

#[derive(Debug, Clone)]
pub struct DockerManagedContainerRunner {
    docker: DockerHandle,
    endpoint_network_subnet: String,
    endpoint_bridge_ifname: String,
    endpoint_wg_ifname: String,
    endpoint_mtu_policy: WireGuardMtuPolicy,
}

/// How the runner reaches the Docker daemon: an already-built client, or a
/// client built from local defaults on first use so the machine runtime can
/// start before Docker is reachable.
#[derive(Debug, Clone)]
enum DockerHandle {
    #[cfg(test)]
    Connected(Docker),
    LazyLocalDefaults(Arc<tokio::sync::OnceCell<Docker>>),
}

impl DockerManagedContainerRunner {
    #[cfg(test)]
    pub fn local_defaults(
        endpoint_network_subnet: impl Into<String>,
        endpoint_bridge_ifname: impl Into<String>,
        endpoint_wg_ifname: impl Into<String>,
        endpoint_mtu_policy: WireGuardMtuPolicy,
    ) -> Result<Self, DockerManagedContainerRunnerConnectError> {
        let docker = connect_local_defaults()?;
        Ok(Self {
            docker: DockerHandle::Connected(docker),
            endpoint_network_subnet: endpoint_network_subnet.into(),
            endpoint_bridge_ifname: endpoint_bridge_ifname.into(),
            endpoint_wg_ifname: endpoint_wg_ifname.into(),
            endpoint_mtu_policy,
        })
    }

    #[must_use]
    pub fn lazy_local_defaults(
        endpoint_network_subnet: String,
        endpoint_bridge_ifname: String,
        endpoint_wg_ifname: String,
        endpoint_mtu_policy: WireGuardMtuPolicy,
    ) -> Self {
        Self {
            docker: DockerHandle::LazyLocalDefaults(Arc::new(tokio::sync::OnceCell::new())),
            endpoint_network_subnet,
            endpoint_bridge_ifname,
            endpoint_wg_ifname,
            endpoint_mtu_policy,
        }
    }

    async fn docker(&self) -> Result<&Docker, DockerManagedContainerRunnerConnectError> {
        match &self.docker {
            #[cfg(test)]
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

        let mut containers = summaries
            .into_iter()
            .map(existing_container_from_summary)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| MachineContainerRunnerError::ListExisting {
                message: error.to_string(),
            })?;

        for container in &mut containers {
            let ExistingManagedContainerState::Running {
                health,
                started_at_unix_ms,
                ..
            } = &mut container.state
            else {
                continue;
            };
            let (observed_health, observed_started_at_unix_ms) =
                docker_container_details(docker, &container.container_id)
                    .await
                    .map_err(|error| MachineContainerRunnerError::ListExisting {
                        message: error,
                    })?;
            *health = observed_health;
            *started_at_unix_ms = Some(observed_started_at_unix_ms);
        }

        Ok(containers)
    }

    async fn ensure_endpoint_network(&self) -> Result<(), MachineContainerRunnerError> {
        let docker = self.docker().await.map_err(|error| {
            MachineContainerRunnerError::EnsureEndpointNetwork {
                message: error.to_string(),
            }
        })?;
        let endpoint_mtu =
            resolve_wireguard_mtu(self.endpoint_mtu_policy, &self.endpoint_wg_ifname).await;
        ensure_endpoint_network(
            docker,
            &self.endpoint_network_subnet,
            &self.endpoint_bridge_ifname,
            endpoint_mtu,
        )
        .await
    }

    async fn ensure_projection_endpoint_network(
        &self,
        expected_subnet: &MachineEndpointSubnet,
    ) -> Result<(), MachineContainerRunnerError> {
        let observed =
            MachineEndpointSubnet::try_new(&self.endpoint_network_subnet).map_err(|error| {
                MachineContainerRunnerError::EnsureEndpointNetwork {
                    message: error.to_string(),
                }
            })?;
        if &observed != expected_subnet {
            return Err(MachineContainerRunnerError::EndpointNetworkSubnetMismatch {
                expected: expected_subnet.clone(),
                observed,
            });
        }
        self.ensure_endpoint_network().await
    }

    async fn read_endpoint_network_status(&self) -> EndpointBridgeStatus {
        let expected = match MachineEndpointSubnet::try_new(&self.endpoint_network_subnet) {
            Ok(expected) => expected,
            Err(_) => {
                return EndpointBridgeStatus::InvalidSubnet {
                    observed: self.endpoint_network_subnet.clone(),
                };
            }
        };
        let docker = match self.docker().await {
            Ok(docker) => docker,
            Err(error) => {
                return EndpointBridgeStatus::Unavailable {
                    message: ployz_core::operation::FailureMessage::try_new(error.to_string())
                        .expect("Docker connection failure is non-empty"),
                };
            }
        };
        let endpoint_mtu =
            resolve_wireguard_mtu(self.endpoint_mtu_policy, &self.endpoint_wg_ifname).await;
        read_endpoint_network_status(
            docker,
            expected,
            &self.endpoint_network_subnet,
            &self.endpoint_bridge_ifname,
            endpoint_mtu,
        )
        .await
    }

    async fn resolve_registry_image(
        &self,
        reference: &ImageReference,
        credential: Option<&RegistryCredential>,
    ) -> Result<OciDigest, MachineContainerRunnerError> {
        let docker =
            self.docker()
                .await
                .map_err(|error| MachineContainerRunnerError::ImagePull {
                    message: error.to_string(),
                })?;
        let failure = |error: BollardError| MachineContainerRunnerError::ImagePull {
            message: redact_registry_credential(
                format!("resolve Docker image {}: {error}", reference.as_str()),
                credential,
            ),
        };
        let mut retry_delays = REGISTRY_RESOLVE_RETRY_DELAYS.into_iter();
        let inspected = loop {
            match docker
                .inspect_registry_image(reference.as_str(), docker_credentials(credential))
                .await
            {
                Ok(inspected) => break inspected,
                Err(error) => {
                    if retryable_registry_resolution(&error)
                        && let Some(delay) = retry_delays.next()
                    {
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(failure(error));
                }
            }
        };
        let Some(digest) = inspected.descriptor.digest else {
            return Err(MachineContainerRunnerError::ImagePull {
                message: format!("registry returned no digest for {}", reference.as_str()),
            });
        };
        OciDigest::try_new(digest).map_err(|error| MachineContainerRunnerError::ImagePull {
            message: error.to_string(),
        })
    }

    async fn create_managed_container(
        &self,
        command: CreateManagedContainer,
    ) -> Result<ContainerId, MachineContainerRunnerError> {
        let docker = self
            .docker()
            .await
            .map_err(|error| MachineContainerRunnerError::Create {
                message: error.to_string(),
            })?;
        let endpoint_mtu =
            resolve_wireguard_mtu(self.endpoint_mtu_policy, &self.endpoint_wg_ifname).await;
        require_endpoint_network(
            docker,
            &self.endpoint_network_subnet,
            &self.endpoint_bridge_ifname,
            endpoint_mtu,
        )
        .await?;
        let pull_reference = command.pull.reference();
        let credential = match &command.pull {
            MachineImagePull::Registry { credential, .. } => credential.as_ref(),
            MachineImagePull::MeshSeed { .. } => None,
        };
        self.pull_image(&pull_reference, credential).await?;
        // Every service container joins the already-converged endpoint
        // network; route state alone decides whether anything dials it.
        let response = docker
            .create_container(None, create_body(command, &self.endpoint_network_subnet))
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

    async fn wait_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> Result<i64, MachineContainerRunnerError> {
        let docker = self
            .docker()
            .await
            .map_err(|error| MachineContainerRunnerError::Wait {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        let result = docker
            .wait_container(container_id.as_str(), None)
            .next()
            .await;
        match result {
            Some(Ok(response)) => Ok(response.status_code),
            Some(Err(BollardError::DockerContainerWaitError { code, .. })) => Ok(code),
            Some(Err(error)) => Err(MachineContainerRunnerError::Wait {
                container_id: container_id.clone(),
                message: error.to_string(),
            }),
            None => Err(MachineContainerRunnerError::Wait {
                container_id: container_id.clone(),
                message: "Docker wait stream ended without a status code".to_owned(),
            }),
        }
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
        if existing.identity != *expected_identity {
            return Err(MachineContainerRunnerError::Remove {
                container_id: container_id.clone(),
                message: format!(
                    "container identity did not match cleanup target: expected {:?}, found {:?}",
                    expected_identity, existing.identity
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
            Err(error) if is_docker_object_missing(&error) => Ok(()),
            Err(error) => Err(MachineContainerRunnerError::Remove {
                container_id: container_id.clone(),
                message: error.to_string(),
            }),
        }
    }

    async fn remove_volume(
        &self,
        docker_volume_name: &str,
    ) -> Result<(), MachineContainerRunnerError> {
        let docker =
            self.docker()
                .await
                .map_err(|error| MachineContainerRunnerError::RemoveVolume {
                    docker_volume_name: docker_volume_name.to_owned(),
                    message: error.to_string(),
                })?;
        let options = RemoveVolumeOptionsBuilder::new().force(true).build();
        match docker
            .remove_volume(docker_volume_name, Some(options))
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if is_docker_object_missing(&error) => Ok(()),
            Err(error) => Err(MachineContainerRunnerError::RemoveVolume {
                docker_volume_name: docker_volume_name.to_owned(),
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
        if existing.identity != *expected_identity {
            return Err(MachineContainerRunnerError::Stop {
                container_id: container_id.clone(),
                message: format!(
                    "container identity did not match stop target: expected {:?}, found {:?}",
                    expected_identity, existing.identity
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
            Err(error) if is_docker_object_missing(&error) => Ok(()),
            Err(error) => Err(MachineContainerRunnerError::Stop {
                container_id: container_id.clone(),
                message: error.to_string(),
            }),
        }
    }

    async fn restart_managed_container(
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
            return Err(MachineContainerRunnerError::Restart {
                container_id: container_id.clone(),
                message: "container was not found".to_owned(),
            });
        };
        if existing.identity != *expected_identity {
            return Err(MachineContainerRunnerError::Restart {
                container_id: container_id.clone(),
                message: format!(
                    "container identity did not match restart target: expected {:?}, found {:?}",
                    expected_identity, existing.identity
                ),
            });
        }

        let docker = self
            .docker()
            .await
            .map_err(|error| MachineContainerRunnerError::Restart {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        match docker
            .restart_container(container_id.as_str(), None::<RestartContainerOptions>)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => Err(MachineContainerRunnerError::Restart {
                container_id: container_id.clone(),
                message: error.to_string(),
            }),
        }
    }
}

fn retryable_registry_resolution(error: &BollardError) -> bool {
    if let BollardError::DockerResponseServerError { status_code, .. } = error {
        return *status_code == 429 || *status_code >= 500;
    }
    matches!(
        error,
        BollardError::RequestTimeoutError
            | BollardError::HyperResponseError { .. }
            | BollardError::HyperLegacyError { .. }
            | BollardError::IOError { .. }
    )
}

impl MachineLogReader for DockerManagedContainerRunner {
    async fn tail_container_logs(
        &self,
        container_id: &ContainerId,
        query: MachineLogQuery,
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
            Some(logs_options(
                capped_tail_lines(query.tail_lines),
                query.since_unix_seconds,
                query.timestamps,
            )),
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
                Err(error) if is_docker_object_missing(&error) => {
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
    pub(crate) async fn pull_image(
        &self,
        image: &str,
        credential: Option<&RegistryCredential>,
    ) -> Result<(), MachineContainerRunnerError> {
        let docker =
            self.docker()
                .await
                .map_err(|error| MachineContainerRunnerError::ImagePull {
                    message: error.to_string(),
                })?;
        let options = CreateImageOptionsBuilder::new().from_image(image).build();
        let mut stream = docker.create_image(Some(options), None, docker_credentials(credential));

        while let Some(result) = stream.next().await {
            result.map_err(|error| MachineContainerRunnerError::ImagePull {
                message: redact_registry_credential(
                    format!("pull Docker image {image}: {error}"),
                    credential,
                ),
            })?;
        }

        Ok(())
    }
}

fn docker_credentials(credential: Option<&RegistryCredential>) -> Option<DockerCredentials> {
    credential.map(|credential| match credential {
        RegistryCredential::Basic { username, password } => DockerCredentials {
            username: Some(username.as_str().to_owned()),
            password: Some(password.secret().to_owned()),
            ..DockerCredentials::default()
        },
        RegistryCredential::IdentityToken { token } => DockerCredentials {
            identitytoken: Some(token.secret().to_owned()),
            ..DockerCredentials::default()
        },
    })
}

fn redact_registry_credential(message: String, credential: Option<&RegistryCredential>) -> String {
    match credential {
        Some(credential) => credential.redact_secret_in(message),
        None => message,
    }
}

fn docker_container_state(
    state: ContainerSummaryStateEnum,
    network_settings: Option<ContainerSummaryNetworkSettings>,
) -> Result<ExistingManagedContainerState, DockerManagedContainerSummaryError> {
    match state {
        ContainerSummaryStateEnum::RUNNING => Ok(ExistingManagedContainerState::Running {
            ip: container_ip(network_settings)?,
            health: ContainerHealth::None,
            started_at_unix_ms: None,
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

async fn docker_container_details(
    docker: &Docker,
    container_id: &ContainerId,
) -> Result<(ContainerHealth, u64), String> {
    let inspect = docker
        .inspect_container(container_id.as_str(), None::<InspectContainerOptions>)
        .await
        .map_err(|error| error.to_string())?;
    let state = inspect
        .state
        .ok_or_else(|| "Docker inspect omitted container state".to_owned())?;
    let health = match state
        .health
        .as_ref()
        .and_then(|health| health.status)
        .unwrap_or(HealthStatusEnum::NONE)
    {
        HealthStatusEnum::NONE | HealthStatusEnum::EMPTY => ContainerHealth::None,
        HealthStatusEnum::STARTING => ContainerHealth::Starting,
        HealthStatusEnum::HEALTHY => ContainerHealth::Healthy,
        HealthStatusEnum::UNHEALTHY => ContainerHealth::Unhealthy,
    };
    let started_at = state
        .started_at
        .ok_or_else(|| "Docker inspect omitted State.StartedAt".to_owned())?;
    let started_at_unix_ms = parse_docker_started_at(&started_at)?;
    Ok((health, started_at_unix_ms))
}

fn parse_docker_started_at(started_at: &str) -> Result<u64, String> {
    let parsed = OffsetDateTime::parse(started_at, &Rfc3339)
        .map_err(|error| format!("invalid Docker StartedAt `{started_at}`: {error}"))?;
    u64::try_from(parsed.unix_timestamp_nanos() / 1_000_000)
        .map_err(|_| format!("Docker StartedAt `{started_at}` is out of range"))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("failed to connect to local Docker: {message}")]
pub struct DockerManagedContainerRunnerConnectError {
    message: String,
}

fn managed_container_list_options() -> bollard::query_parameters::ListContainersOptions {
    let filters = HashMap::from([("label".to_owned(), vec![format!("{MANAGED_LABEL}=true")])]);
    ListContainersOptionsBuilder::new()
        .all(true)
        .filters(&filters)
        .build()
}

fn logs_options(
    tail_lines: u16,
    since_unix_seconds: Option<u64>,
    timestamps: MachineLogTimestamps,
) -> bollard::query_parameters::LogsOptions {
    let tail = tail_lines.to_string();
    let mut options = LogsOptionsBuilder::default()
        .stdout(true)
        .stderr(true)
        .timestamps(matches!(timestamps, MachineLogTimestamps::Include))
        .tail(&tail);
    if let Some(since_unix_seconds) = since_unix_seconds {
        let since = i32::try_from(since_unix_seconds).unwrap_or(i32::MAX);
        options = options.since(since);
    }
    options.build()
}

const fn capped_tail_lines(tail_lines: Option<u16>) -> u16 {
    match tail_lines {
        Some(lines) if lines > MAX_LOG_TAIL_LINES => MAX_LOG_TAIL_LINES,
        Some(lines) => lines,
        None => DEFAULT_LOG_TAIL_LINES,
    }
}

fn create_body(
    command: CreateManagedContainer,
    endpoint_network_subnet: &str,
) -> ContainerCreateBody {
    let image = command.pull.reference();
    let runtime = command.runtime;
    let env = if runtime.environment.is_empty() {
        None
    } else {
        Some(
            runtime
                .environment
                .iter()
                .map(|(name, value)| format!("{}={}", name.as_str(), value.as_str()))
                .collect(),
        )
    };
    let cmd = runtime.command.map(Vec::from);
    let entrypoint = runtime.entrypoint.map(|entrypoint| match entrypoint {
        ContainerEntrypoint::Clear => Vec::new(),
        ContainerEntrypoint::Argv(argv) => Vec::from(argv),
    });
    let healthcheck = runtime.healthcheck.as_ref().map(health_config);
    let restart_policy = restart_policy(runtime.restart_policy);
    let cap_add = capabilities(&runtime.cap_add);
    let cap_drop = capabilities(&runtime.cap_drop);
    let memory = runtime
        .resources
        .memory_bytes
        .map(|value| saturating_i64(value.get()));
    let nano_cpus = runtime
        .resources
        .nano_cpus
        .map(|value| saturating_i64(value.get()));
    let pids_limit = runtime.resources.pids.map(|value| value.get());
    // ponytail: invalid endpoint subnet omits the resolver; validated machine
    // configuration is the upgrade path.
    let dns = endpoint_bridge_gateway_ipv4(endpoint_network_subnet)
        .map(|gateway| vec![gateway.to_string()]);
    let dns_search = Some(vec![
        format!(
            "{}.{}",
            command.identity.namespace_id.as_str(),
            INTERNAL_DNS_SUFFIX
        )
        .to_ascii_lowercase(),
    ]);
    ContainerCreateBody {
        image: Some(image),
        env,
        cmd,
        entrypoint,
        healthcheck,
        stop_timeout: Some(i64::from(runtime.stop_grace_period.as_seconds())),
        labels: Some(hashmap_from_btree(labels::render(&command.identity))),
        host_config: Some(HostConfig {
            network_mode: Some(ENDPOINT_NETWORK_NAME.to_owned()),
            mounts: docker_volume_mounts(&command.identity.namespace_id, &runtime.volume_mounts),
            restart_policy,
            cap_add,
            cap_drop,
            memory,
            nano_cpus,
            pids_limit,
            dns,
            dns_search,
            // musl applies the namespace search domain only when a query has fewer dots
            // than ndots; an inherited host ndots:0 disables search for plain service names.
            dns_options: Some(vec!["ndots:1".to_owned()]),
            ..Default::default()
        }),
        networking_config: Some(NetworkingConfig {
            endpoints_config: Some(HashMap::from([(
                ENDPOINT_NETWORK_NAME.to_owned(),
                EndpointSettings::default(),
            )])),
        }),
        ..Default::default()
    }
}

fn docker_volume_mounts(
    namespace_id: &ployz_core::ids::NamespaceId,
    mounts: &[ployz_core::deploy::ServiceVolumeMount],
) -> Option<Vec<Mount>> {
    if mounts.is_empty() {
        return None;
    }
    Some(
        mounts
            .iter()
            .map(|mount| Mount {
                target: Some(mount.target.as_str().to_owned()),
                source: Some(docker_volume_name(namespace_id, &mount.volume_name)),
                typ: Some(MountType::VOLUME),
                read_only: None,
                consistency: None,
                bind_options: None,
                volume_options: None,
                image_options: None,
                tmpfs_options: None,
            })
            .collect(),
    )
}

fn health_config(healthcheck: &ContainerHealthcheck) -> HealthConfig {
    HealthConfig {
        test: Some(match &healthcheck.test {
            ContainerHealthcheckTest::Inherit => Vec::new(),
            ContainerHealthcheckTest::Disable => vec!["NONE".to_owned()],
            ContainerHealthcheckTest::Exec(command) => std::iter::once("CMD".to_owned())
                .chain(command.as_slice().iter().cloned())
                .collect(),
            ContainerHealthcheckTest::Shell(command) => {
                vec!["CMD-SHELL".to_owned(), command.as_str().to_owned()]
            }
        }),
        interval: healthcheck
            .interval
            .map(|value| saturating_i64(value.as_nanos())),
        timeout: healthcheck
            .timeout
            .map(|value| saturating_i64(value.as_nanos())),
        retries: healthcheck.retries.map(|value| i64::from(value.get())),
        start_period: healthcheck
            .start_period
            .map(|value| saturating_i64(value.as_nanos())),
        start_interval: None,
    }
}

/// Docker's API carries byte and nanosecond quantities as `i64`; product
/// values are `u64`, so anything past `i64::MAX` (physically impossible
/// limits) clamps instead of failing the create call.
fn saturating_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn restart_policy(policy: ContainerRestartPolicy) -> Option<RestartPolicy> {
    let name = match policy {
        ContainerRestartPolicy::DockerDefault => return None,
        ContainerRestartPolicy::No => RestartPolicyNameEnum::NO,
        ContainerRestartPolicy::Always => RestartPolicyNameEnum::ALWAYS,
        ContainerRestartPolicy::OnFailure => RestartPolicyNameEnum::ON_FAILURE,
        ContainerRestartPolicy::UnlessStopped => RestartPolicyNameEnum::UNLESS_STOPPED,
    };
    Some(RestartPolicy {
        name: Some(name),
        maximum_retry_count: None,
    })
}

fn capabilities(capabilities: &[ployz_core::deploy::LinuxCapability]) -> Option<Vec<String>> {
    if capabilities.is_empty() {
        return None;
    }
    Some(
        ployz_core::deploy::canonical_capabilities(capabilities)
            .into_iter()
            .map(|capability| capability.as_str().to_owned())
            .collect(),
    )
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
    let health_status = summary
        .health
        .and_then(|health| health.status)
        .and_then(docker_health_status);

    let identity = labels::parse(&btree_from_hashmap(labels))
        .map_err(DockerManagedContainerSummaryError::InvalidLabels)?;
    Ok(ExistingManagedContainer {
        container_id: ContainerId::try_new(id)
            .map_err(DockerManagedContainerSummaryError::InvalidContainerId)?,
        state: docker_container_state(state, summary.network_settings)?,
        identity,
        health_status: health_status.or_else(|| summary.status.as_deref().and_then(status_health)),
        resolved_image_identity: summary.image_id,
        created_at_unix_seconds: summary.created,
    })
}

fn docker_health_status(
    status: ContainerSummaryHealthStatusEnum,
) -> Option<ManagedContainerHealthStatus> {
    match status {
        ContainerSummaryHealthStatusEnum::EMPTY | ContainerSummaryHealthStatusEnum::NONE => None,
        ContainerSummaryHealthStatusEnum::STARTING => Some(ManagedContainerHealthStatus::Starting),
        ContainerSummaryHealthStatusEnum::HEALTHY => Some(ManagedContainerHealthStatus::Healthy),
        ContainerSummaryHealthStatusEnum::UNHEALTHY => {
            Some(ManagedContainerHealthStatus::Unhealthy)
        }
    }
}

fn status_health(status: &str) -> Option<ManagedContainerHealthStatus> {
    if status.contains("unhealthy") {
        Some(ManagedContainerHealthStatus::Unhealthy)
    } else if status.contains("healthy") {
        Some(ManagedContainerHealthStatus::Healthy)
    } else if status.contains("health: starting") {
        Some(ManagedContainerHealthStatus::Starting)
    } else {
        None
    }
}

fn container_ip(
    network_settings: Option<ContainerSummaryNetworkSettings>,
) -> Result<Option<IpAddr>, DockerManagedContainerSummaryError> {
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

    Ok(Some(ip.parse::<IpAddr>().map_err(|_| {
        DockerManagedContainerSummaryError::InvalidEndpointIp {
            value: ip.to_owned(),
        }
    })?))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum DockerManagedContainerSummaryError {
    #[error("managed Docker container is missing id")]
    MissingId,
    #[error("managed Docker container is missing labels")]
    MissingLabels,
    #[error("managed Docker container is missing state")]
    MissingState,
    #[error("managed Docker container has invalid endpoint ip: {value}")]
    InvalidEndpointIp { value: String },
    #[error("managed Docker container has invalid id: {0}")]
    InvalidContainerId(SubjectTokenError),
    #[error("managed Docker container has invalid labels: {0:?}")]
    InvalidLabels(ManagedContainerLabelError),
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
    use crate::roles::machine::protocol::MachineImagePull;
    use ployz_core::deploy::{
        ContainerCommand, ContainerEntrypoint, ContainerHealthcheck, ContainerHealthcheckTest,
        ContainerMountPath, ContainerResourceLimits, ContainerRestartPolicy, ContainerRuntimeSpec,
        EnvName, EnvValue, HealthcheckDurationNanos, HealthcheckRetries, HealthcheckShellCommand,
        ImageReference, LinuxCapability, MemoryBytes, NanoCpus, PidsLimit, ServiceEnvironment,
        ServiceVolumeMount, StopGracePeriod, VolumeName,
    };
    use ployz_core::ids::{NamespaceRevisionEntryId, OperationId, ServiceId, StepId};
    use ployz_core::machine::runtime::{ManagedContainerIdentity, ManagedContainerKind};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    const TEST_ENDPOINT_SUBNET: &str = "10.42.7.0/24";

    #[tokio::test]
    async fn registry_resolution_retries_transient_server_failures() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let success = format!(r#"{{"Descriptor":{{"digest":"{digest}"}},"Platforms":[]}}"#);
        let (runner, attempts, _socket_dir) = registry_runner_with_responses(vec![
            (429, r#"{"message":"rate limited"}"#.to_owned()),
            (500, r#"{"message":"registry unavailable"}"#.to_owned()),
            (200, success),
        ])
        .await;

        let resolved = runner
            .resolve_registry_image(&image("nginx:1.27-alpine"), None)
            .await
            .expect("third registry inspection succeeds");

        assert_eq!(resolved, OciDigest::try_new(digest).expect("valid digest"));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn registry_resolution_does_not_retry_terminal_client_failures() {
        let digest = format!("sha256:{}", "b".repeat(64));
        let (runner, attempts, _socket_dir) = registry_runner_with_responses(vec![
            (404, r#"{"message":"manifest unknown"}"#.to_owned()),
            (
                200,
                format!(r#"{{"Descriptor":{{"digest":"{digest}"}},"Platforms":[]}}"#),
            ),
        ])
        .await;

        let error = runner
            .resolve_registry_image(&image("nginx:missing"), None)
            .await
            .expect_err("missing manifest is terminal");

        let MachineContainerRunnerError::ImagePull { message } = error else {
            panic!("expected image pull failure");
        };
        assert!(message.contains("status code 404"), "{message}");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn registry_resolution_stops_after_three_transient_server_failures() {
        let digest = format!("sha256:{}", "c".repeat(64));
        let (runner, attempts, _socket_dir) = registry_runner_with_responses(vec![
            (500, r#"{"message":"first failure"}"#.to_owned()),
            (500, r#"{"message":"second failure"}"#.to_owned()),
            (500, r#"{"message":"final failure"}"#.to_owned()),
            (
                200,
                format!(r#"{{"Descriptor":{{"digest":"{digest}"}},"Platforms":[]}}"#),
            ),
        ])
        .await;

        let error = runner
            .resolve_registry_image(&image("nginx:1.27-alpine"), None)
            .await
            .expect_err("three transient failures exhaust retries");

        let MachineContainerRunnerError::ImagePull { message } = error else {
            panic!("expected image pull failure");
        };
        assert!(message.contains("final failure"), "{message}");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    async fn registry_runner_with_responses(
        responses: Vec<(u16, String)>,
    ) -> (
        DockerManagedContainerRunner,
        Arc<AtomicUsize>,
        tempfile::TempDir,
    ) {
        let socket_dir = tempfile::TempDir::new().expect("Docker API stub directory");
        let socket_path = socket_dir.path().join("docker.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind Docker API stub");
        let attempts = Arc::new(AtomicUsize::new(0));
        let server_attempts = attempts.clone();
        tokio::spawn(async move {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().await.expect("accept Docker API request");
                let mut request = Vec::new();
                let mut buffer = [0; 512];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream
                        .read(&mut buffer)
                        .await
                        .expect("read Docker API request");
                    assert_ne!(read, 0, "Docker API request ended before its headers");
                    request.extend_from_slice(
                        buffer
                            .get(..read)
                            .expect("read length is bounded by buffer"),
                    );
                }
                server_attempts.fetch_add(1, Ordering::SeqCst);
                let reason = if status == 200 {
                    "OK"
                } else {
                    "Internal Server Error"
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write Docker API response");
            }
        });
        let docker = Docker::connect_with_socket(
            socket_path.to_str().expect("UTF-8 Docker API socket path"),
            5,
            bollard::API_DEFAULT_VERSION,
        )
        .expect("connect Docker API stub");
        (
            DockerManagedContainerRunner {
                docker: DockerHandle::Connected(docker),
                endpoint_network_subnet: TEST_ENDPOINT_SUBNET.to_owned(),
                endpoint_bridge_ifname: "br-test".to_owned(),
                endpoint_wg_ifname: "wg-test".to_owned(),
                endpoint_mtu_policy: WireGuardMtuPolicy::Fixed(1_420),
            },
            attempts,
            socket_dir,
        )
    }

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
        let body = create_body(
            CreateManagedContainer {
                pull: MachineImagePull::Registry {
                    credential: None,
                    reference: image("ghcr.io/acme/api:rev-2"),
                },
                runtime: ContainerRuntimeSpec::image_defaults(),
                identity: managed_identity(),
            },
            TEST_ENDPOINT_SUBNET,
        );

        assert_eq!(body.image, Some("ghcr.io/acme/api:rev-2".to_owned()));
        assert_eq!(
            body.labels,
            Some(hashmap_from_btree(labels::render(&managed_identity())))
        );
    }

    #[test]
    fn create_body_sets_runtime_fields() {
        let body = create_body(
            CreateManagedContainer {
                pull: MachineImagePull::Registry {
                    credential: None,
                    reference: image("ghcr.io/acme/api:rev-2"),
                },
                runtime: runtime_spec(),
                identity: managed_identity(),
            },
            TEST_ENDPOINT_SUBNET,
        );

        assert_eq!(
            body.env,
            Some(vec!["ALPHA=1".to_owned(), "BETA=two".to_owned()])
        );
        assert_eq!(body.cmd, Some(vec!["serve".to_owned(), "api".to_owned()]));
        assert_eq!(body.entrypoint, Some(vec!["/init".to_owned()]));
        assert_eq!(body.stop_timeout, Some(30));
    }

    #[test]
    fn create_body_sets_machine_local_dns_and_namespace_search_domain() {
        let body = create_body(
            CreateManagedContainer {
                pull: MachineImagePull::Registry {
                    credential: None,
                    reference: image("ghcr.io/acme/api:rev-2"),
                },
                runtime: ContainerRuntimeSpec::image_defaults(),
                identity: managed_identity(),
            },
            TEST_ENDPOINT_SUBNET,
        );
        let host = body.host_config.expect("host config exists");

        assert_eq!(host.dns, Some(vec!["10.42.7.1".to_owned()]));
        assert_eq!(host.dns_search, Some(vec!["default.internal".to_owned()]));
        assert_eq!(host.dns_options, Some(vec!["ndots:1".to_owned()]));
    }

    #[test]
    fn create_body_sets_runtime_controls() {
        let mut runtime = ContainerRuntimeSpec::image_defaults();
        runtime.healthcheck = Some(ContainerHealthcheck {
            test: ContainerHealthcheckTest::Shell(
                HealthcheckShellCommand::try_new("wget -q -O - http://127.0.0.1/")
                    .expect("valid healthcheck"),
            ),
            interval: Some(HealthcheckDurationNanos::try_new(5_000_000_000).expect("duration")),
            timeout: Some(HealthcheckDurationNanos::try_new(2_000_000_000).expect("duration")),
            retries: Some(HealthcheckRetries::try_new(3).expect("retries")),
            start_period: Some(HealthcheckDurationNanos::try_new(1_000_000_000).expect("duration")),
        });
        runtime.restart_policy = ContainerRestartPolicy::UnlessStopped;
        runtime.cap_add = vec![LinuxCapability::try_new("NET_ADMIN").expect("capability")];
        runtime.cap_drop = vec![LinuxCapability::try_new("MKNOD").expect("capability")];
        runtime.resources = ContainerResourceLimits {
            nano_cpus: Some(NanoCpus::try_new(500_000_000).expect("nano cpus")),
            memory_bytes: Some(MemoryBytes::try_new(64_000_000).expect("memory")),
            pids: Some(PidsLimit::try_new(64).expect("pids")),
        };

        let body = create_body(
            CreateManagedContainer {
                pull: MachineImagePull::Registry {
                    credential: None,
                    reference: image("ghcr.io/acme/api:rev-2"),
                },
                runtime,
                identity: managed_identity(),
            },
            TEST_ENDPOINT_SUBNET,
        );

        assert_eq!(
            body.healthcheck.map(|health| health.test),
            Some(Some(vec![
                "CMD-SHELL".to_owned(),
                "wget -q -O - http://127.0.0.1/".to_owned()
            ]))
        );
        let host = body.host_config.expect("host config exists");
        assert_eq!(
            host.restart_policy.and_then(|policy| policy.name),
            Some(RestartPolicyNameEnum::UNLESS_STOPPED)
        );
        assert_eq!(host.cap_add, Some(vec!["NET_ADMIN".to_owned()]));
        assert_eq!(host.cap_drop, Some(vec!["MKNOD".to_owned()]));
        assert_eq!(host.nano_cpus, Some(500_000_000));
        assert_eq!(host.memory, Some(64_000_000));
        assert_eq!(host.pids_limit, Some(64));
    }

    #[test]
    fn create_body_clears_entrypoint_when_runtime_requests_clear() {
        let mut runtime = ContainerRuntimeSpec::image_defaults();
        runtime.entrypoint = Some(ContainerEntrypoint::Clear);
        let body = create_body(
            CreateManagedContainer {
                pull: MachineImagePull::Registry {
                    credential: None,
                    reference: image("ghcr.io/acme/api:rev-2"),
                },
                runtime,
                identity: managed_identity(),
            },
            TEST_ENDPOINT_SUBNET,
        );

        assert_eq!(body.entrypoint, Some(Vec::new()));
    }

    #[test]
    fn create_body_sets_default_stop_timeout() {
        let body = create_body(
            CreateManagedContainer {
                pull: MachineImagePull::Registry {
                    credential: None,
                    reference: image("ghcr.io/acme/api:rev-2"),
                },
                runtime: ContainerRuntimeSpec::image_defaults(),
                identity: managed_identity(),
            },
            TEST_ENDPOINT_SUBNET,
        );

        assert_eq!(body.stop_timeout, Some(10));
    }

    #[test]
    fn create_body_mounts_named_volumes() {
        let mut runtime = ContainerRuntimeSpec::image_defaults();
        runtime.volume_mounts = vec![ServiceVolumeMount {
            volume_name: VolumeName::try_new("postgres_data").expect("valid volume name"),
            target: ContainerMountPath::try_new("/var/lib/postgresql/data")
                .expect("valid mount path"),
        }];
        let body = create_body(
            CreateManagedContainer {
                pull: MachineImagePull::Registry {
                    credential: None,
                    reference: image("ghcr.io/acme/api:rev-2"),
                },
                runtime,
                identity: managed_identity(),
            },
            TEST_ENDPOINT_SUBNET,
        );

        let mounts = body
            .host_config
            .expect("host config")
            .mounts
            .expect("named volume mounts");
        let [mount] = mounts.as_slice() else {
            panic!("expected one named volume mount");
        };
        assert_eq!(mount.typ, Some(MountType::VOLUME));
        assert_eq!(
            mount.source,
            Some("ployz-n7-default-v13-postgres_data".to_owned())
        );
        assert_eq!(mount.target, Some("/var/lib/postgresql/data".to_owned()));
    }

    #[test]
    fn create_body_always_joins_the_endpoint_network() {
        // Ports never influence network membership (ADR 0023): even a
        // route-less service container joins the endpoint network so a
        // later route attach can reach it without recreation.
        let body = create_body(
            CreateManagedContainer {
                pull: MachineImagePull::Registry {
                    credential: None,
                    reference: image("ghcr.io/acme/api:rev-2"),
                },
                runtime: ContainerRuntimeSpec::image_defaults(),
                identity: managed_identity(),
            },
            TEST_ENDPOINT_SUBNET,
        );

        assert_eq!(body.exposed_ports, None);
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

    #[tokio::test]
    async fn projection_network_rejects_configured_subnet_before_touching_docker() {
        let runner = DockerManagedContainerRunner::lazy_local_defaults(
            "10.198.1.0/24".to_owned(),
            "br-ployz".to_owned(),
            "ployz-wg0".to_owned(),
            WireGuardMtuPolicy::Auto,
        );
        let expected = MachineEndpointSubnet::try_new("10.198.2.0/24").expect("subnet");

        assert!(matches!(
            runner.ensure_projection_endpoint_network(&expected).await,
            Err(MachineContainerRunnerError::EndpointNetworkSubnetMismatch { .. })
        ));
    }

    #[test]
    fn summary_with_managed_labels_becomes_existing_container() {
        let summary = ContainerSummary {
            id: Some("0123456789abcdef".to_owned()),
            labels: Some(hashmap_from_btree(labels::render(&managed_identity()))),
            state: Some(ContainerSummaryStateEnum::RUNNING),
            ..Default::default()
        };

        assert_eq!(
            existing_container_from_summary(summary).expect("summary parses"),
            ExistingManagedContainer {
                container_id: container_id("0123456789abcdef"),
                identity: managed_identity(),
                state: ExistingManagedContainerState::Running {
                    ip: None,
                    health: ContainerHealth::None,
                    started_at_unix_ms: None,
                },
                health_status: None,
                resolved_image_identity: None,
                created_at_unix_seconds: None,
            }
        );
    }

    #[test]
    fn docker_started_at_becomes_unix_milliseconds() {
        assert_eq!(
            parse_docker_started_at("2026-07-10T08:09:10.123456789Z")
                .expect("Docker timestamp parses"),
            1_783_670_950_123,
        );
    }

    #[test]
    fn status_health_reads_unhealthy_as_unhealthy() {
        assert_eq!(
            status_health("Up 2 hours (unhealthy)"),
            Some(ManagedContainerHealthStatus::Unhealthy),
        );
    }

    #[test]
    fn running_summary_with_network_ip_reports_the_endpoint_ip() {
        let summary = ContainerSummary {
            id: Some("0123456789abcdef".to_owned()),
            labels: Some(hashmap_from_btree(labels::render(&managed_identity()))),
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
                ip: Some("10.42.0.9".parse().expect("valid endpoint ip")),
                health: ContainerHealth::None,
                started_at_unix_ms: None,
            }
        );
    }

    #[test]
    fn running_summary_uses_only_the_ployz_network_for_endpoint_evidence() {
        let summary = ContainerSummary {
            id: Some("0123456789abcdef".to_owned()),
            labels: Some(hashmap_from_btree(labels::render(&managed_identity()))),
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
                ip: Some("10.42.0.9".parse().expect("valid endpoint ip")),
                health: ContainerHealth::None,
                started_at_unix_ms: None,
            }
        );
    }

    #[test]
    fn running_summary_without_ployz_network_is_running_but_unroutable() {
        let summary = ContainerSummary {
            id: Some("0123456789abcdef".to_owned()),
            labels: Some(hashmap_from_btree(labels::render(&managed_identity()))),
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
            ExistingManagedContainerState::Running {
                ip: None,
                health: ContainerHealth::None,
                started_at_unix_ms: None,
            }
        );
    }

    #[test]
    fn summary_with_created_state_is_not_reusable_as_running() {
        let summary = ContainerSummary {
            id: Some("0123456789abcdef".to_owned()),
            labels: Some(hashmap_from_btree(labels::render(&managed_identity()))),
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
            labels: Some(hashmap_from_btree(labels::render(&managed_identity()))),
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

    #[test]
    fn docker_credentials_keep_basic_and_identity_token_modes_distinct() {
        let basic = RegistryCredential::try_basic("alice", "password").expect("valid basic auth");
        let token = RegistryCredential::try_identity_token("token").expect("valid token auth");

        let basic = docker_credentials(Some(&basic)).expect("basic credentials");
        assert_eq!(basic.username.as_deref(), Some("alice"));
        assert_eq!(basic.password.as_deref(), Some("password"));
        assert_eq!(basic.identitytoken, None);

        let token = docker_credentials(Some(&token)).expect("token credentials");
        assert_eq!(token.username, None);
        assert_eq!(token.password, None);
        assert_eq!(token.identitytoken.as_deref(), Some("token"));
    }

    #[test]
    fn registry_errors_redact_the_deploy_scoped_secret() {
        let basic = RegistryCredential::try_basic("alice", "password").expect("valid basic auth");
        let token = RegistryCredential::try_identity_token("token").expect("valid token auth");

        assert_eq!(
            redact_registry_credential(
                "registry reflected password in its response".to_owned(),
                Some(&basic),
            ),
            "registry reflected [redacted] in its response"
        );
        assert_eq!(
            redact_registry_credential(
                "registry reflected token in its response".to_owned(),
                Some(&token),
            ),
            "registry reflected [redacted] in its response"
        );
    }

    fn managed_identity() -> ManagedContainerIdentity {
        ManagedContainerIdentity {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
            namespace_revision_entry_id: namespace_revision_entry_id("entry_2"),
            operation_id: operation_id("op_123"),
            step_id: step_id("run_1"),
            kind: ManagedContainerKind::Service,
        }
    }

    fn namespace_id(value: &str) -> ployz_core::ids::NamespaceId {
        ployz_core::ids::NamespaceId::try_new(value).expect("valid namespace id")
    }

    fn volume_name(value: &str) -> VolumeName {
        VolumeName::try_new(value).expect("valid volume name")
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

    fn runtime_spec() -> ContainerRuntimeSpec {
        let mut runtime = ContainerRuntimeSpec::image_defaults();
        runtime.command = Some(
            ContainerCommand::try_new(vec!["serve".to_owned(), "api".to_owned()])
                .expect("valid command"),
        );
        runtime.entrypoint = Some(ContainerEntrypoint::Argv(
            ContainerCommand::try_new(vec!["/init".to_owned()]).expect("valid entrypoint"),
        ));
        runtime.environment = ServiceEnvironment::from(BTreeMap::from([
            (
                EnvName::try_new("BETA").expect("valid env name"),
                EnvValue::try_new("two").expect("valid env value"),
            ),
            (
                EnvName::try_new("ALPHA").expect("valid env name"),
                EnvValue::try_new("1").expect("valid env value"),
            ),
        ]));
        runtime.stop_grace_period = StopGracePeriod::from(30);
        runtime
    }
}
