//! Run one captured Build against this host's Docker, as `ployz build` does on a CI runner.

use std::{collections::BTreeMap, path::Path};

use ployz_build::{BuildError, BuiltImage, Output, remote::Definition};

use super::{CapturedBuild, CapturedTarget, Error, invalid};

/// Variables through which a GitHub Actions runner exposes its build cache service.
const ACTIONS_CACHE_VARIABLES: [&str; 4] = [
    "ACTIONS_CACHE_URL",
    "ACTIONS_RESULTS_URL",
    "ACTIONS_RUNTIME_TOKEN",
    "ACTIONS_CACHE_SERVICE_V2",
];

/// One completed image and the Docker repository its Service names.
pub struct LocalImage {
    pub image: BuiltImage,
    pub repository: String,
}

impl CapturedBuild {
    /// Build the single captured target with local Docker and Buildx, rendering progress
    /// to stderr and to `observe`. The runner's cache service variables pass through so
    /// Buildx can read it.
    ///
    /// # Errors
    /// Refuses a capture that is not exactly one target, and reports the Build's failure.
    pub fn execute_local(
        &self,
        cancellation: &ployz_build::Cancellation,
        observe: &(dyn Fn(&ployz_build::Progress) + Sync),
    ) -> Result<LocalImage, Error> {
        let (captured, images) =
            self.run_local(|request| ployz_build::execute(request, cancellation, observe))?;
        let image = images
            .into_iter()
            .next()
            .ok_or_else(|| invalid("the Build completed without an image"))?;
        let reference = captured
            .image
            .parse::<oci_client::Reference>()
            .map_err(|error| invalid(error.to_string()))?;
        Ok(LocalImage {
            image,
            repository: reference.repository().to_owned(),
        })
    }

    /// Upload this Build's cache to a GitHub Actions runner's cache service by running the
    /// same Build again, producing no image. Without that service it does nothing.
    ///
    /// # Errors
    /// Refuses a capture that is not exactly one target, and reports the export's failure.
    pub fn export_local_cache(
        &self,
        cancellation: &ployz_build::Cancellation,
    ) -> Result<(), Error> {
        self.run_local(|request| {
            ployz_build::export_cache(request, cancellation).map(|()| Vec::new())
        })
        .map(drop)
    }

    /// Run `execute` on the single captured target, with the runner's cache service variables.
    fn run_local(
        &self,
        execute: impl FnOnce(&ployz_build::Request<'_>) -> Result<Vec<BuiltImage>, BuildError>,
    ) -> Result<(&CapturedTarget, Vec<BuiltImage>), Error> {
        let [captured] = self.targets.as_slice() else {
            return Err(invalid(
                "a local Build takes exactly one Git-sourced Service",
            ));
        };
        let root = captured.inputs.root();
        let definition = Definition {
            retained_tags: Vec::new(),
            image_contexts: BTreeMap::new(),
            targets: vec![captured.target.clone()],
            output: Output::Load,
            no_cache: false,
            pull: false,
        };
        let railpack = ployz_build::remote::validate_capture(root, &definition)
            .map_err(|error| invalid(error.to_string()))?;
        let mut environment = BTreeMap::from([
            (
                "PATH".to_owned(),
                std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
            ),
            ("HOME".to_owned(), path(&root.join("private"))),
            (
                "DOCKER_CONFIG".to_owned(),
                path(&root.join("private/docker")),
            ),
        ]);
        for name in ACTIONS_CACHE_VARIABLES {
            if let Ok(value) = std::env::var(name) {
                environment.insert(name.to_owned(), value);
            }
        }
        let images = execute(&ployz_build::Request {
            image_contexts: &definition.image_contexts,
            compose_file: Path::new("compose.yaml"),
            working_dir: root,
            environment: &environment,
            docker: None,
            targets: &definition.targets,
            railpack: &railpack,
            build_args: &[],
            output: definition.output,
            no_cache: false,
            pull: false,
        })
        .map_err(|source| Error::Build {
            service: captured.name.clone(),
            source,
        })?;
        Ok((captured, images))
    }
}

fn path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
