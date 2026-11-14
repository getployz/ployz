//! Converges one named daemon-owned container on a desired spec.
//!
//! The container's `ployzd.spec` label is the digest of everything it was created
//! from, so comparing that label is the whole drift check.

use bollard::{
    Docker,
    errors::Error as DockerError,
    models::{ContainerCreateBody, ContainerInspectResponse},
    query_parameters::{
        CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, StopContainerOptions,
        StopContainerOptionsBuilder,
    },
};
use ployz_core::{PinnedImage, PullPolicy, labels};
use sha2::{Digest as _, Sha256};

use crate::docker_image::{is_not_found, prepare_image};

use super::{Error, LocalDocker, Status, classify};

#[derive(Clone, Debug, PartialEq, Eq)]
struct SpecDigest(String);

/// A create body running `image`, whose `ployzd.spec` label is the digest of its own
/// content and of the files the container reads at start.
pub(crate) struct DesiredContainer {
    image: PinnedImage,
    body: ContainerCreateBody,
    digest: SpecDigest,
}

impl DesiredContainer {
    /// `files` are the daemon-written files the container reads at start, in a
    /// caller-fixed order. `image` replaces any image `body` names.
    pub(crate) fn new(image: PinnedImage, mut body: ContainerCreateBody, files: &[&[u8]]) -> Self {
        body.image = Some(image.as_str().to_owned());
        if let Some(labels) = body.labels.as_mut() {
            labels.remove(labels::SPEC);
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
            .insert(labels::SPEC.to_owned(), digest.0.clone());
        Self {
            image,
            body,
            digest,
        }
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
                .and_then(|labels| labels.get(labels::SPEC))
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ensure {
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

/// The Docker a service lives on, and how that Docker makes and starts its container.
pub(crate) trait ServiceRuntime: Send + Sync {
    /// What failing to make the container reports as: the host's Docker answers with
    /// Docker's own error, the local endpoint with the daemon's.
    type Error: From<DockerError> + Send;

    fn client(&self) -> &Docker;

    fn create(
        &self,
        name: &str,
        config: ContainerCreateBody,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send;
}

impl ServiceRuntime for Docker {
    type Error = DockerError;

    fn client(&self) -> &Docker {
        self
    }

    async fn create(&self, name: &str, config: ContainerCreateBody) -> Result<(), DockerError> {
        self.create_container(
            Some(CreateContainerOptionsBuilder::default().name(name).build()),
            config,
        )
        .await?;
        self.start_container(name, None).await
    }
}

impl ServiceRuntime for LocalDocker {
    type Error = Error;

    fn client(&self) -> &Docker {
        &self.client
    }

    async fn create(&self, name: &str, config: ContainerCreateBody) -> Result<(), Error> {
        self.create_container(
            Some(CreateContainerOptionsBuilder::default().name(name).build()),
            config,
        )
        .await?;
        self.client.start_container(name, None).await?;
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct ManagedService<R> {
    runtime: R,
    name: String,
    stop_grace_seconds: Option<i32>,
}

impl<R: ServiceRuntime> ManagedService<R> {
    pub(crate) fn new(runtime: R, name: impl Into<String>) -> Self {
        Self {
            runtime,
            name: name.into(),
            stop_grace_seconds: None,
        }
    }

    /// Seconds Docker waits after SIGTERM before it kills the container, instead of
    /// its 10 s default.
    pub(crate) fn with_stop_grace(mut self, seconds: i32) -> Self {
        self.stop_grace_seconds = Some(seconds);
        self
    }

    pub(crate) async fn ensure(&self, desired: DesiredContainer) -> Result<Ensure, R::Error> {
        let docker = self.runtime.client();
        let existing = match docker.inspect_container(&self.name, None).await {
            Ok(container) => Some(Existing::of(&container)),
            Err(error) if is_not_found(&error) => None,
            Err(error) => return Err(error.into()),
        };
        let ensure = decide(existing.as_ref(), &desired.digest);
        match ensure {
            Ensure::Keep => {}
            Ensure::Start => docker.start_container(&self.name, None).await?,
            Ensure::Create => self.create(desired).await?,
            Ensure::Replace => {
                // A registry outage must leave the old container serving, not none at all.
                prepare_image(docker, desired.image.as_str(), PullPolicy::Missing, None).await?;
                self.remove().await?;
                self.create(desired).await?;
            }
        }
        Ok(ensure)
    }

    async fn create(&self, desired: DesiredContainer) -> Result<(), R::Error> {
        prepare_image(
            self.runtime.client(),
            desired.image.as_str(),
            PullPolicy::Missing,
            None,
        )
        .await?;
        self.runtime.create(&self.name, desired.body).await
    }

    async fn stop(&self) -> Result<(), DockerError> {
        match self
            .runtime
            .client()
            .stop_container(&self.name, self.stop_options())
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if matches!(classify(&error), Status::Unchanged | Status::Absent) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn stop_options(&self) -> Option<StopContainerOptions> {
        self.stop_grace_seconds
            .map(|seconds| StopContainerOptionsBuilder::default().t(seconds).build())
    }

    pub(crate) async fn remove(&self) -> Result<(), DockerError> {
        self.stop().await?;
        match self
            .runtime
            .client()
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

#[cfg(test)]
mod ensure_tests;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use bollard::models::HostConfig;

    use super::*;

    fn body(image: &str) -> ContainerCreateBody {
        ContainerCreateBody {
            image: Some(image.to_owned()),
            ..Default::default()
        }
    }

    const PINNED: PinnedImage = PinnedImage::new(
        "example.test/service:1@sha256:1111111111111111111111111111111111111111111111111111111111111111",
    );

    fn digest(body: ContainerCreateBody, files: &[&[u8]]) -> SpecDigest {
        DesiredContainer::new(PINNED, body, files).digest
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
            DesiredContainer::new(
                PinnedImage::new(
                    "example.test/service:1@sha256:2222222222222222222222222222222222222222222222222222222222222222",
                ),
                body("image:1"),
                files,
            )
            .digest,
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
                PINNED,
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
        let honest = labelled(&[(labels::SYSTEM_MANAGED, "")]);
        let forged = labelled(&[(labels::SYSTEM_MANAGED, ""), (labels::SPEC, "x")]);
        assert_eq!(forged.digest, honest.digest);
        for desired in [honest, forged] {
            assert_eq!(desired.body.image.as_deref(), Some(PINNED.as_str()));
            assert_eq!(
                desired.body.labels.unwrap().get(labels::SPEC),
                Some(&desired.digest.0)
            );
        }
    }
}
