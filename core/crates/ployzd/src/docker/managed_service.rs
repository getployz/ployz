//! Converges one named daemon-owned container on a desired spec.
//!
//! The container's `ployzd.spec` label is the digest of everything it was created
//! from, so comparing that label is the whole drift check.

use bollard::{
    Docker,
    errors::Error as DockerError,
    models::{ContainerCreateBody, ContainerInspectResponse},
    query_parameters::{CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder},
};
use ployz_core::PullPolicy;
use sha2::{Digest as _, Sha256};

use crate::docker_image::{is_not_found, prepare_image};

use super::{Error, LocalDocker};

const SPEC_LABEL: &str = "ployzd.spec";

#[derive(Clone, Debug, PartialEq, Eq)]
struct SpecDigest(String);

/// A create body whose `ployzd.spec` label is the digest of its own content and
/// of the files the container reads at start.
pub(crate) struct DesiredContainer {
    body: ContainerCreateBody,
    digest: SpecDigest,
}

impl DesiredContainer {
    /// `files` are the daemon-written files the container reads at start, in a
    /// caller-fixed order.
    pub(crate) fn new(mut body: ContainerCreateBody, files: &[&[u8]]) -> Self {
        if let Some(labels) = body.labels.as_mut() {
            labels.remove(SPEC_LABEL);
        }
        let mut canonical =
            serde_json::to_value(&body).expect("a container create body serializes to JSON");
        canonical.sort_all_objects();
        let mut hasher = Sha256::new();
        hasher.update(canonical.to_string());
        for file in files {
            hasher.update((file.len() as u64).to_le_bytes());
            hasher.update(file);
        }
        let digest = SpecDigest(hex::encode(hasher.finalize()));
        body.labels
            .get_or_insert_default()
            .insert(SPEC_LABEL.to_owned(), digest.0.clone());
        Self { body, digest }
    }
}

struct Existing {
    digest: Option<SpecDigest>,
    running: bool,
}

impl Existing {
    fn of(container: &ContainerInspectResponse) -> Self {
        Self {
            digest: container
                .config
                .as_ref()
                .and_then(|config| config.labels.as_ref())
                .and_then(|labels| labels.get(SPEC_LABEL))
                .cloned()
                .map(SpecDigest),
            running: container
                .state
                .as_ref()
                .and_then(|state| state.running)
                .unwrap_or(false),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Ensure {
    Create,
    Replace,
    Start,
    Keep,
}

fn decide(existing: Option<&Existing>, desired: &SpecDigest) -> Ensure {
    match existing {
        None => Ensure::Create,
        Some(Existing {
            digest: Some(digest),
            running,
        }) if digest == desired => {
            if *running {
                Ensure::Keep
            } else {
                Ensure::Start
            }
        }
        Some(_) => Ensure::Replace,
    }
}

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

    pub(crate) async fn ensure_host(&self, desired: DesiredContainer) -> Result<(), DockerError> {
        let Engine::Host(docker) = &self.engine else {
            unreachable!("endpoint service uses ensure_endpoint")
        };
        self.ensure(docker, desired, |config| async move {
            self.create_host(config).await
        })
        .await
    }

    pub(crate) async fn ensure_endpoint(&self, desired: DesiredContainer) -> Result<(), Error> {
        let Engine::Endpoint(docker) = &self.engine else {
            unreachable!("host service uses ensure_host")
        };
        self.ensure(&docker.client, desired, |config| async move {
            self.create_endpoint(config).await
        })
        .await
    }

    async fn ensure<E, F, Fut>(
        &self,
        docker: &Docker,
        desired: DesiredContainer,
        create: F,
    ) -> Result<(), E>
    where
        E: From<DockerError>,
        F: FnOnce(ContainerCreateBody) -> Fut,
        Fut: std::future::Future<Output = Result<(), E>>,
    {
        let existing = match docker.inspect_container(&self.name, None).await {
            Ok(container) => Some(Existing::of(&container)),
            Err(error) if is_not_found(&error) => None,
            Err(error) => return Err(error.into()),
        };
        match decide(existing.as_ref(), &desired.digest) {
            Ensure::Keep => {}
            Ensure::Start => docker.start_container(&self.name, None).await?,
            Ensure::Create => create(desired.body).await?,
            Ensure::Replace => {
                // A registry outage must leave the old container serving, not none at all.
                prepare_image(docker, self.image, PullPolicy::Missing, None).await?;
                self.remove().await?;
                create(desired.body).await?;
            }
        }
        Ok(())
    }

    async fn create_host(&self, config: ContainerCreateBody) -> Result<(), DockerError> {
        let Engine::Host(docker) = &self.engine else {
            unreachable!()
        };
        prepare_image(docker, self.image, PullPolicy::Missing, None).await?;
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
        prepare_image(&docker.client, self.image, PullPolicy::Missing, None).await?;
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

    async fn stop(&self) -> Result<(), DockerError> {
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
    use std::collections::HashMap;

    use bollard::models::HostConfig;

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

    fn body(image: &str) -> ContainerCreateBody {
        ContainerCreateBody {
            image: Some(image.to_owned()),
            ..Default::default()
        }
    }

    fn digest(body: ContainerCreateBody, files: &[&[u8]]) -> SpecDigest {
        DesiredContainer::new(body, files).digest
    }

    #[test]
    fn decide_table() {
        let desired = SpecDigest("d".into());
        let other = SpecDigest("o".into());
        let existing = |digest: Option<&SpecDigest>, running| Existing {
            digest: digest.cloned(),
            running,
        };
        let cases = [
            (None, Ensure::Create),
            (Some(existing(None, true)), Ensure::Replace),
            (Some(existing(None, false)), Ensure::Replace),
            (Some(existing(Some(&other), true)), Ensure::Replace),
            (Some(existing(Some(&other), false)), Ensure::Replace),
            (Some(existing(Some(&desired), false)), Ensure::Start),
            (Some(existing(Some(&desired), true)), Ensure::Keep),
        ];
        for (existing, expected) in cases {
            assert_eq!(decide(existing.as_ref(), &desired), expected);
        }
    }

    #[test]
    fn digest_covers_body_and_files() {
        let files: &[&[u8]] = &[b"config", b"schema"];
        let base = digest(body("image:1"), files);
        assert_eq!(base, digest(body("image:1"), files));
        let changed = [
            digest(body("image:2"), files),
            digest(
                ContainerCreateBody {
                    cmd: Some(vec!["agent".into()]),
                    ..body("image:1")
                },
                files,
            ),
            digest(
                ContainerCreateBody {
                    host_config: Some(HostConfig {
                        binds: Some(vec!["/data:/data".into()]),
                        ..Default::default()
                    }),
                    ..body("image:1")
                },
                files,
            ),
            digest(body("image:1"), &[b"config2", b"schema"]),
            digest(body("image:1"), &[b"config", b"schema2"]),
            digest(body("image:1"), &[b"configs", b"chema"]),
        ];
        for changed in changed {
            assert_ne!(base, changed);
        }
    }

    #[test]
    fn digest_ignores_map_order() {
        let pairs: Vec<_> = (0..32)
            .map(|n| (format!("k{n}"), format!("v{n}")))
            .collect();
        let labelled = |pairs: &mut dyn Iterator<Item = &(String, String)>| {
            let mut labels = HashMap::new();
            for (key, value) in pairs {
                labels.insert(key.clone(), value.clone());
            }
            ContainerCreateBody {
                labels: Some(labels),
                ..body("image:1")
            }
        };
        assert_eq!(
            digest(labelled(&mut pairs.iter()), &[]),
            digest(labelled(&mut pairs.iter().rev()), &[])
        );
    }

    #[test]
    fn new_stamps_its_own_label_and_ignores_a_forged_one() {
        let labelled = |labels: &[(&str, &str)]| {
            DesiredContainer::new(
                ContainerCreateBody {
                    labels: Some(
                        labels
                            .iter()
                            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                            .collect(),
                    ),
                    ..body("image:1")
                },
                &[b"config"],
            )
        };
        let honest = labelled(&[("ployzd.managed", "")]);
        let forged = labelled(&[("ployzd.managed", ""), (SPEC_LABEL, "x")]);
        assert_eq!(forged.digest, honest.digest);
        for desired in [honest, forged] {
            assert_eq!(
                desired.body.labels.unwrap().get(SPEC_LABEL),
                Some(&desired.digest.0)
            );
        }
    }
}
