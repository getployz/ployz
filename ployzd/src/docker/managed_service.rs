use bollard::{
    Docker,
    errors::Error as DockerError,
    models::{ContainerCreateBody, ContainerInspectResponse},
    query_parameters::{
        CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder,
    },
};
use futures_util::TryStreamExt;

pub(crate) struct ManagedService {
    docker: Docker,
    name: String,
    image: &'static str,
}

impl ManagedService {
    pub(crate) fn new(docker: Docker, name: impl Into<String>, image: &'static str) -> Self {
        Self {
            docker,
            name: name.into(),
            image,
        }
    }

    pub(crate) async fn ensure(
        &self,
        config: ContainerCreateBody,
        matches: impl FnOnce(&ContainerInspectResponse) -> bool,
    ) -> Result<(), DockerError> {
        match self.docker.inspect_container(&self.name, None).await {
            Ok(container) if matches(&container) => {
                if !container
                    .state
                    .and_then(|state| state.running)
                    .unwrap_or(false)
                {
                    self.docker.start_container(&self.name, None).await?;
                }
            }
            Ok(_) => {
                self.remove().await?;
                self.create(config).await?;
            }
            Err(error) if is_not_found(&error) => self.create(config).await?,
            Err(error) => return Err(error),
        }
        Ok(())
    }

    async fn create(&self, config: ContainerCreateBody) -> Result<(), DockerError> {
        if let Err(error) = self.docker.inspect_image(self.image).await {
            if !is_not_found(&error) {
                return Err(error);
            }
            self.docker
                .create_image(
                    Some(
                        CreateImageOptionsBuilder::default()
                            .from_image(self.image)
                            .build(),
                    ),
                    None,
                    None,
                )
                .try_collect::<Vec<_>>()
                .await?;
        }
        self.docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::default()
                        .name(&self.name)
                        .build(),
                ),
                config,
            )
            .await?;
        self.docker.start_container(&self.name, None).await
    }

    pub(crate) async fn stop(&self) -> Result<(), DockerError> {
        match self.docker.stop_container(&self.name, None).await {
            Ok(()) => Ok(()),
            Err(error) if is_not_found(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn remove(&self) -> Result<(), DockerError> {
        self.stop().await?;
        match self
            .docker
            .remove_container(
                &self.name,
                Some(RemoveContainerOptionsBuilder::default().v(true).build()),
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if is_not_found(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn is_not_found(error: &DockerError) -> bool {
    matches!(
        error,
        DockerError::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}
