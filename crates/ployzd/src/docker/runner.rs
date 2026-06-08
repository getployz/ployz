use crate::docker::labels::{MANAGED_LABEL, ManagedContainerLabelError, ManagedContainerLabels};
use crate::node_agent::runtime::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    NodeContainerRunner, NodeContainerRunnerError,
};
use bollard::Docker;
use bollard::models::{ContainerCreateBody, ContainerSummary, ContainerSummaryStateEnum};
use bollard::query_parameters::ListContainersOptionsBuilder;
use ployz_core::ids::{ContainerId, SubjectTokenError};
use std::collections::{BTreeMap, HashMap};
use std::fmt;

#[derive(Debug, Clone)]
pub struct DockerManagedContainerRunner {
    docker: Docker,
}

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
}

fn docker_container_state(
    state: ContainerSummaryStateEnum,
) -> Result<ExistingManagedContainerState, DockerManagedContainerSummaryError> {
    match state {
        ContainerSummaryStateEnum::RUNNING => Ok(ExistingManagedContainerState::Running),
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
    ContainerCreateBody {
        image: Some(command.image.as_str().to_owned()),
        labels: Some(hashmap_from_btree(command.labels.render())),
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

    Ok(ExistingManagedContainer {
        container_id: ContainerId::try_new(id)
            .map_err(DockerManagedContainerSummaryError::InvalidContainerId)?,
        labels: ManagedContainerLabels::parse(&btree_from_hashmap(labels))
            .map_err(DockerManagedContainerSummaryError::InvalidLabels)?,
        state: docker_container_state(state)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DockerManagedContainerSummaryError {
    MissingId,
    MissingLabels,
    MissingState,
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
            labels: managed_labels(),
        });

        assert_eq!(body.image, Some("ghcr.io/acme/api:rev-2".to_owned()));
        assert_eq!(
            body.labels,
            Some(hashmap_from_btree(managed_labels().render()))
        );
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
                state: ExistingManagedContainerState::Running,
            }
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
        }
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
