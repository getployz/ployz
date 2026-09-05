use bollard::{
    Docker,
    errors::Error as DockerError,
    models::{ContainerCreateBody, ContainerInspectResponse},
    query_parameters::{
        CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder,
    },
};
use futures_util::TryStreamExt;

use super::{Error, LocalDocker};

pub(crate) struct ManagedService {
    engine: Engine,
    name: String,
    image: &'static str,
}

enum Engine {
    Host(Docker),
    Endpoint(LocalDocker),
}

impl ManagedService {
    pub(crate) fn host(docker: Docker, name: impl Into<String>, image: &'static str) -> Self {
        Self {
            engine: Engine::Host(docker),
            name: name.into(),
            image,
        }
    }

    pub(crate) fn endpoint(
        docker: LocalDocker,
        name: impl Into<String>,
        image: &'static str,
    ) -> Self {
        Self {
            engine: Engine::Endpoint(docker),
            name: name.into(),
            image,
        }
    }

    pub(crate) async fn ensure_host(
        &self,
        config: ContainerCreateBody,
        matches: impl FnOnce(&ContainerInspectResponse) -> bool,
    ) -> Result<(), DockerError> {
        let Engine::Host(docker) = &self.engine else {
            unreachable!("endpoint service uses ensure_endpoint")
        };
        self.ensure(docker, config, matches, |config| async move {
            self.create_host(config).await
        })
        .await
    }

    pub(crate) async fn ensure_endpoint(
        &self,
        config: ContainerCreateBody,
        matches: impl FnOnce(&ContainerInspectResponse) -> bool,
    ) -> Result<(), Error> {
        let Engine::Endpoint(docker) = &self.engine else {
            unreachable!("host service uses ensure_host")
        };
        self.ensure(&docker.client, config, matches, |config| async move {
            self.create_endpoint(config).await
        })
        .await
    }

    async fn ensure<E, F, Fut>(
        &self,
        docker: &Docker,
        config: ContainerCreateBody,
        matches: impl FnOnce(&ContainerInspectResponse) -> bool,
        create: F,
    ) -> Result<(), E>
    where
        E: From<DockerError>,
        F: FnOnce(ContainerCreateBody) -> Fut,
        Fut: std::future::Future<Output = Result<(), E>>,
    {
        match docker.inspect_container(&self.name, None).await {
            Ok(container) if matches(&container) => {
                if !container
                    .state
                    .and_then(|state| state.running)
                    .unwrap_or(false)
                {
                    docker.start_container(&self.name, None).await?;
                }
            }
            Ok(_) => {
                self.remove().await.map_err(E::from)?;
                create(config).await?;
            }
            Err(error) if is_not_found(&error) => create(config).await?,
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    async fn prepare_image(&self, docker: &Docker) -> Result<(), DockerError> {
        if let Err(error) = docker.inspect_image(self.image).await {
            if !is_not_found(&error) {
                return Err(error);
            }
            docker
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
        Ok(())
    }

    async fn create_host(&self, config: ContainerCreateBody) -> Result<(), DockerError> {
        let Engine::Host(docker) = &self.engine else {
            unreachable!()
        };
        self.prepare_image(docker).await?;
        docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::default()
                        .name(&self.name)
                        .build(),
                ),
                config,
            )
            .await?;
        docker.start_container(&self.name, None).await
    }

    async fn create_endpoint(&self, config: ContainerCreateBody) -> Result<(), Error> {
        let Engine::Endpoint(docker) = &self.engine else {
            unreachable!()
        };
        self.prepare_image(&docker.client).await?;
        docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::default()
                        .name(&self.name)
                        .build(),
                ),
                config,
            )
            .await?;
        docker.client.start_container(&self.name, None).await?;
        Ok(())
    }

    pub(crate) async fn stop(&self) -> Result<(), DockerError> {
        match self.docker().stop_container(&self.name, None).await {
            Ok(()) => Ok(()),
            Err(error) if is_already_stopped(&error) || is_not_found(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn remove(&self) -> Result<(), DockerError> {
        self.stop().await?;
        match self
            .docker()
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

    fn docker(&self) -> &Docker {
        match &self.engine {
            Engine::Host(docker) => docker,
            Engine::Endpoint(docker) => &docker.client,
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

fn is_already_stopped(error: &DockerError) -> bool {
    matches!(
        error,
        DockerError::DockerResponseServerError {
            status_code: 304,
            ..
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopped_and_missing_services_are_idempotent_stop_outcomes() {
        let response = |status_code| DockerError::DockerResponseServerError {
            status_code,
            message: String::new(),
        };
        assert!(is_already_stopped(&response(304)));
        assert!(is_not_found(&response(404)));
        assert!(!is_already_stopped(&response(500)));
        assert!(!is_not_found(&response(500)));
    }
}
