//! A captured Build directory, prepared and requested the same way whether it runs on a
//! Build Machine or on the developer's or runner's own Docker.

use std::{collections::BTreeMap, path::Path};

use ployz_core::OpenedSealed;

use crate::{
    BuildError, Railpack, Request, SealedStaging, Staged,
    received_recipe::{SealedFiles, validate_capture},
    remote::Definition,
};

/// Why a capture could not be prepared. The two arms differ in how the local path reports them.
#[derive(Debug)]
pub enum PrepareError {
    /// Writing the opened Sealed values failed.
    Staging(BuildError),
    /// The capture is not one this Build may run.
    Invalid(String),
}

impl From<PrepareError> for BuildError {
    fn from(error: PrepareError) -> Self {
        match error {
            PrepareError::Staging(error) => error,
            PrepareError::Invalid(message) => Self::Request(message),
        }
    }
}

/// A capture with its Sealed values staged, validated, and its environment built. Dropping it
/// removes the staged files, so it must drop before the directory it was prepared in.
pub struct Prepared<'a> {
    root: &'a Path,
    definition: &'a Definition,
    railpack: Vec<Railpack>,
    staged: Option<Staged>,
    environment: BTreeMap<String, String>,
    sealed: bool,
}

impl<'a> Prepared<'a> {
    /// Stage `opened` (when there is a set), validate the capture at `root` against
    /// `definition`, and build the Build's environment: the private PATH, HOME and
    /// DOCKER_CONFIG, then `extra_environment`, and no Buildx provenance when the Build
    /// reads Sealed plaintext, since provenance records build argument values.
    ///
    /// # Errors
    /// [`PrepareError::Staging`] when the values cannot be written, and
    /// [`PrepareError::Invalid`] when the capture is refused.
    pub fn capture(
        root: &'a Path,
        definition: &'a Definition,
        opened: Option<&OpenedSealed>,
        extra_environment: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, PrepareError> {
        Self::staged_in(
            &SealedStaging::under(root),
            root,
            definition,
            opened,
            extra_environment,
            crate::Access::Full,
        )
    }

    /// [`Self::capture`], staging into `staging`, which its owner can revoke, for a Build
    /// with `access`. A Free Build's capture is checked as one, and its Docker client reads
    /// an empty configuration this host made, never the one the capture brought.
    pub(crate) fn staged_in(
        staging: &SealedStaging,
        root: &'a Path,
        definition: &'a Definition,
        opened: Option<&OpenedSealed>,
        extra_environment: impl IntoIterator<Item = (String, String)>,
        access: crate::Access,
    ) -> Result<Self, PrepareError> {
        let staged = opened
            .map(|opened| Staged::stage(staging, opened, &definition.targets))
            .transpose()
            .map_err(PrepareError::Staging)?;
        let files = match access {
            crate::Access::Full => SealedFiles::Opened,
            crate::Access::Free => SealedFiles::Free,
        };
        let railpack = validate_capture(root, definition, files)
            .map_err(|error| PrepareError::Invalid(error.to_string()))?;
        let mut environment = match access {
            crate::Access::Full => crate::upload::environment(root),
            crate::Access::Free => crate::upload::free_environment(root).map_err(|error| {
                PrepareError::Staging(BuildError::Prerequisite(format!(
                    "make a free Build's empty Docker configuration: {error}"
                )))
            })?,
        };
        environment.extend(extra_environment);
        if crate::reads_plaintext(
            staged.as_ref().and_then(Staged::compose_override),
            &railpack,
        ) {
            environment.insert("BUILDX_METADATA_PROVENANCE".into(), "disabled".into());
        }
        Ok(Self {
            root,
            definition,
            railpack,
            staged,
            environment,
            sealed: opened.is_some_and(|opened| !opened.is_empty()),
        })
    }

    #[must_use]
    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    /// The Request to run: the definition's own output, cache and pull choices.
    #[must_use]
    pub fn request<'r>(
        &'r self,
        docker: Option<&'r Path>,
        cache_scope: Option<&'r str>,
    ) -> Request<'r> {
        Request {
            image_contexts: &self.definition.image_contexts,
            railpack: &self.railpack,
            compose_file: Path::new(crate::capture::COMPOSE),
            sealed_override: self.staged.as_ref().and_then(Staged::compose_override),
            working_dir: self.root,
            environment: &self.environment,
            docker,
            targets: &self.definition.targets,
            build_args: &[],
            output: self.definition.output,
            no_cache: self.definition.no_cache,
            pull: self.definition.pull,
            sealed: self.sealed,
            cache_scope,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{PrepareError, Prepared};
    use crate::{BuildError, Output, remote::Definition};

    #[test]
    fn a_missing_capture_is_refused_with_the_text_both_paths_report() {
        let missing = tempfile::tempdir().unwrap().path().join("gone");
        let definition = Definition {
            retained_tags: Vec::new(),
            image_contexts: BTreeMap::new(),
            targets: Vec::new(),
            output: Output::Load,
            no_cache: false,
            pull: false,
        };

        let Err(error) = Prepared::capture(&missing, &definition, None, []) else {
            panic!("a missing capture directory must be refused");
        };

        let PrepareError::Invalid(message) = &error else {
            panic!("expected a refusal, got {error:?}");
        };
        assert_eq!(message, "Build capture directory is unavailable");
        assert!(matches!(
            BuildError::from(error),
            BuildError::Request(message) if message == "Build capture directory is unavailable"
        ));
    }
}
