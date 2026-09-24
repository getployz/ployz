//! Image tag times and unforced, in-use-safe image removal for Image Cleanup.

use std::collections::HashSet;

use bollard::{
    errors::Error as DockerError,
    query_parameters::{ListContainersOptionsBuilder, RemoveImageOptions},
};
use ployz_core::{ImageRemoval, ImageRemovalOutcome, ImagesRemoved};

use super::{ContainerRuntime, Error};

impl ContainerRuntime {
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

fn refusal(error: DockerError) -> ImageRemovalOutcome {
    if let DockerError::DockerResponseServerError { status_code, .. } = &error {
        match status_code {
            404 => return ImageRemovalOutcome::NotFound,
            409 => return ImageRemovalOutcome::InUse,
            _ => {}
        }
    }
    ImageRemovalOutcome::Failed {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
