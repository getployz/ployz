//! Command-owned temporary tags, independent of captured source and credentials.

use crate::{Deadline, Docker, Streams};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::DirBuilderExt as _,
    path::{Path, PathBuf},
    time::Duration,
};

/// Removes only this command's temporary tags when its image results are released.
pub struct ImageRetention {
    tags: Vec<String>,
    program: PathBuf,
    environment: BTreeMap<String, String>,
    root: PathBuf,
}

/// Completed host images and the guard that keeps their temporary tags alive.
pub struct RetainedImages {
    /// Immutable content reported by the completed Build.
    pub images: Vec<crate::BuiltImage>,
    /// Release after dependent builds and image delivery finish.
    pub retention: ImageRetention,
}

impl ImageRetention {
    /// Own cleanup of the explicitly generated tags, never user image references.
    /// # Errors
    /// Rejects references outside the reserved UUID-tag namespace or unavailable private staging.
    pub fn new(
        tags: Vec<String>,
        docker: Option<&Path>,
        mut environment: BTreeMap<String, String>,
    ) -> Result<Self, crate::BuildError> {
        for tag in &tags {
            let valid = tag
                .parse::<oci_client::Reference>()
                .ok()
                .is_some_and(|reference| {
                    reference.digest().is_none()
                        && reference
                            .tag()
                            .and_then(|tag| tag.strip_prefix("ployz-build-"))
                            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
                });
            if !valid {
                return Err(crate::BuildError::Request(
                    "invalid temporary Build tag".into(),
                ));
            }
        }
        // Image removal needs the captured local socket, not registry credentials
        // or the source tree. Docker uses an empty, private default configuration.
        let root =
            std::env::temp_dir().join(format!("ployz-image-retention-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(|error| {
                crate::BuildError::Request(format!("create image retention directory: {error}"))
            })?;
        environment
            .retain(|name, _| matches!(name.as_str(), "PATH" | "DOCKER_HOST" | "DOCKER_CONTEXT"));
        environment.insert("HOME".into(), root.to_string_lossy().into_owned());
        environment.insert("DOCKER_CONFIG".into(), root.to_string_lossy().into_owned());
        Ok(Self {
            tags,
            program: docker.unwrap_or_else(|| Path::new("docker")).to_owned(),
            environment,
            root,
        })
    }
}

impl Drop for ImageRetention {
    fn drop(&mut self) {
        if self.tags.is_empty() {
            let _ = fs::remove_dir_all(&self.root);
            return;
        }
        let docker = Docker {
            program: &self.program,
            environment: &self.environment,
            working_dir: &self.root,
            deadline: Deadline::starting_now(Duration::from_secs(10)),
            cancellation: None,
            progress: None,
        };
        let arguments: Vec<_> = ["image", "rm"]
            .into_iter()
            .chain(self.tags.iter().map(String::as_str))
            .collect();
        if let Err(error) = docker.require_local().and_then(|()| {
            docker.run(
                "release temporary Build tags",
                &arguments,
                Streams::Captured,
            )
        }) {
            eprintln!("Could not release temporary Build tags: {error}");
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_cannot_delete_user_tags_or_digest_references() {
        for reference in [
            "example.test/api:latest",
            "example.test/api:ployz-build-not-a-uuid",
            "--all",
            "example.test/api@sha256:1111111111111111111111111111111111111111111111111111111111111111",
        ] {
            assert!(
                ImageRetention::new(vec![reference.into()], None, BTreeMap::new()).is_err(),
                "{reference}"
            );
        }
    }
}
