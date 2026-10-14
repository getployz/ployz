//! Images on this Machine: a deploy's registry pull, and Image Cleanup, which finds
//! each repository's superseded build images and removes them unforced, keeping any a
//! Container uses.
//!
//! Only `repository:ployz-sha256-*` tags, which Direct Image Transfer creates, are
//! candidates.

use std::collections::HashSet;

use bollard::{
    errors::Error as DockerError,
    query_parameters::{ListContainersOptionsBuilder, RemoveImageOptions},
};
use ployz_core::{
    DiskSpace, ImageRemoval, ImageRemovalOutcome, ImageSummary, ImagesRemoved, PullPolicy,
    RegistryAuth,
};

use super::{ContainerRuntime, Error};

const RETENTION_TAG: &str = ":ployz-sha256-";
/// Unused images kept for rollback.
const KEEP: usize = 3;
/// Unused images kept when the Docker root is nearly full.
const KEEP_UNDER_PRESSURE: usize = 1;
/// Beyond [`KEEP`], an unused image goes once this idle.
const IDLE_SECONDS: i64 = 7 * 24 * 60 * 60;
/// Below this free share of the Docker root, keep only [`KEEP_UNDER_PRESSURE`].
const PRESSURE_FREE_PERCENT: u64 = 20;

impl ContainerRuntime {
    /// Make `image` present as `policy` asks, then confirm Docker holds it.
    ///
    /// # Errors
    ///
    /// Returns when the pull fails or the image is absent afterwards, as under `Never`.
    pub async fn pull_image(
        &self,
        image: &str,
        policy: PullPolicy,
        auth: Option<&RegistryAuth>,
    ) -> Result<(), Error> {
        let docker = &self.docker.client;
        crate::docker_image::prepare_image(docker, image, policy, auth)
            .await
            .map_err(Error::ImagePull)?;
        docker
            .inspect_image(image)
            .await
            .map_err(Error::ImagePull)?;
        Ok(())
    }

    /// Unix seconds this Machine last tagged the image, when Docker recorded it.
    pub(super) async fn last_tagged(&self, id: &str) -> Result<Option<i64>, Error> {
        match self.docker.client.inspect_image(id).await {
            Ok(image) => Ok(image
                .metadata
                .and_then(|metadata| metadata.last_tag_time)
                .and_then(|time| chrono::DateTime::parse_from_rfc3339(&time).ok())
                .map(|time| time.timestamp())
                .filter(|seconds| *seconds > 0)),
            // Removed between list and inspect.
            Err(DockerError::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Remove each repository's superseded build images.
    ///
    /// # Errors
    ///
    /// Returns when Docker cannot list images or Containers; per-reference failures
    /// are results.
    pub async fn prune_images(&self, repositories: &[String]) -> Result<ImagesRemoved, Error> {
        let now = chrono::Utc::now().timestamp();
        let mut references = Vec::new();
        for repository in repositories {
            let store = self
                .list_images(Some(&format!("{repository}{RETENTION_TAG}*")), true)
                .await?;
            references.extend(superseded(
                &store.images,
                repository,
                under_pressure(store.docker_root),
                now,
            ));
        }
        if references.is_empty() {
            return Ok(ImagesRemoved::default());
        }
        self.remove_images(&references).await
    }

    /// Remove each reference without force. A reference whose image any Container
    /// uses, running or not, is kept: Docker alone would untag it when the image
    /// carries another tag.
    ///
    /// # Errors
    ///
    /// Returns when Docker cannot list Containers; per-reference failures are results.
    pub async fn remove_images(&self, references: &[String]) -> Result<ImagesRemoved, Error> {
        let options = ListContainersOptionsBuilder::default().all(true).build();
        let in_use = self
            .docker
            .client
            .list_containers(Some(options))
            .await?
            .into_iter()
            .filter_map(|container| container.image_id)
            .collect::<HashSet<_>>();
        let mut results = Vec::with_capacity(references.len());
        for reference in references {
            let outcome = match self.docker.client.inspect_image(reference).await {
                Ok(image) if image.id.as_ref().is_some_and(|id| in_use.contains(id)) => {
                    ImageRemovalOutcome::InUse
                }
                Ok(_) => match self
                    .docker
                    .client
                    .remove_image(reference, None::<RemoveImageOptions>, None)
                    .await
                {
                    Ok(_) => ImageRemovalOutcome::Removed,
                    Err(error) => refusal(error),
                },
                Err(error) => refusal(error),
            };
            results.push(ImageRemoval {
                reference: reference.clone(),
                outcome,
            });
        }
        Ok(ImagesRemoved { results })
    }
}

/// Retention tags in `repository` to remove, newest kept. Images a Container uses,
/// or whose use Docker did not count, are neither kept nor removed.
fn superseded(images: &[ImageSummary], repository: &str, pressure: bool, now: i64) -> Vec<String> {
    let prefix = format!("{repository}{RETENTION_TAG}");
    let mut unused = images
        .iter()
        .filter(|image| image.containers == 0)
        .flat_map(|image| {
            image
                .repo_tags
                .iter()
                .filter(|tag| tag.starts_with(&prefix))
                .map(|tag| (image.last_tagged, tag))
        })
        .collect::<Vec<_>>();
    // Newest first; an unknown tag time sorts oldest.
    unused.sort_by_key(|(tagged, _)| std::cmp::Reverse(*tagged));
    let keep = if pressure { KEEP_UNDER_PRESSURE } else { KEEP };
    unused
        .into_iter()
        .skip(keep)
        .filter(|(tagged, _)| pressure || tagged.is_some_and(|at| now - at > IDLE_SECONDS))
        .map(|(_, tag)| tag.clone())
        .collect()
}

fn under_pressure(space: Option<DiskSpace>) -> bool {
    space.is_some_and(|space| {
        u128::from(space.free_bytes) * 100
            < u128::from(space.total_bytes) * u128::from(PRESSURE_FREE_PERCENT)
    })
}

fn refusal(error: DockerError) -> ImageRemovalOutcome {
    if let DockerError::DockerResponseServerError { status_code, .. } = &error {
        match status_code {
            404 => return ImageRemovalOutcome::NotFound,
            409 => return ImageRemovalOutcome::InUse,
            _ => {}
        }
    }
    ImageRemovalOutcome::Failed {
        message: ployz_core::error_chain::inline(&error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 100 * 24 * 60 * 60;
    const DAY: i64 = 24 * 60 * 60;

    fn image(tag: &str, days_ago: Option<i64>, containers: i64) -> ImageSummary {
        ImageSummary {
            id: format!("sha256:{tag}"),
            repo_tags: vec![format!("ployz-build/web:ployz-sha256-{tag}")],
            created: 0,
            size: 1,
            containers,
            platforms: Vec::new(),
            last_tagged: days_ago.map(|days| NOW - days * DAY),
        }
    }

    fn removed(images: &[ImageSummary], pressure: bool) -> Vec<String> {
        superseded(images, "ployz-build/web", pressure, NOW)
            .into_iter()
            .map(|tag| {
                tag.trim_start_matches("ployz-build/web:ployz-sha256-")
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn keeps_three_unused_and_removes_older_idle_ones() {
        let images = [
            image("running", Some(30), 1),
            image("uncounted", Some(30), -1),
            image("a", Some(1), 0),
            image("b", Some(2), 0),
            image("c", Some(3), 0),
            image("recent", Some(4), 0),
            image("old", Some(8), 0),
            image("unknown", None, 0),
        ];
        assert_eq!(removed(&images, false), ["old"]);
    }

    #[test]
    fn pressure_keeps_only_the_newest_unused() {
        let images = [
            image("running", Some(30), 1),
            image("a", Some(1), 0),
            image("b", Some(2), 0),
            image("unknown", None, 0),
        ];
        assert_eq!(removed(&images, true), ["b", "unknown"]);
    }

    #[test]
    fn ignores_other_repositories_and_tags() {
        let mut other = image("x", Some(90), 0);
        other.repo_tags = vec![
            "ployz-build/web-api:ployz-sha256-x".into(),
            "ployz-build/web:latest".into(),
        ];
        assert!(removed(&[other.clone(), other.clone(), other.clone(), other], true).is_empty());
    }

    #[test]
    fn pressure_starts_below_a_fifth_free() {
        let space = |free_bytes| {
            Some(DiskSpace {
                total_bytes: 100,
                free_bytes,
            })
        };
        assert!(under_pressure(space(19)));
        assert!(!under_pressure(space(20)));
        assert!(!under_pressure(None));
    }

    #[test]
    fn docker_refusals_map_to_removal_outcomes() {
        let refused = |status_code| {
            refusal(DockerError::DockerResponseServerError {
                status_code,
                message: "refused".into(),
            })
        };
        assert_eq!(refused(404), ImageRemovalOutcome::NotFound);
        assert_eq!(refused(409), ImageRemovalOutcome::InUse);
        assert!(matches!(refused(500), ImageRemovalOutcome::Failed { .. }));
    }
}
