//! Run one captured Build against this host's Docker, as `ployz build` does on a CI runner.

use std::{collections::BTreeMap, path::Path};

use ployz_build::{BuildError, BuiltImage, Output, OutputScrub, Staged, remote::Definition};

use super::{CapturedBuild, CapturedTarget, Error, SEALED_BUILDS_RUN_ON_SERVER, invalid};

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
    /// to stderr and to `observe`, both scrubbed of the Sealed values it holds. The
    /// runner's cache service variables pass through so Buildx can read it.
    ///
    /// # Errors
    /// Refuses a capture that is not exactly one target or that reads a Sealed value it
    /// doesn't hold, and reports the Build's failure.
    pub fn execute_local(
        &self,
        cancellation: &ployz_build::Cancellation,
        observe: &(dyn Fn(&ployz_build::Progress) + Sync),
    ) -> Result<LocalImage, Error> {
        let (captured, images) = self.run_local(|request, scrub| {
            if let Some(scrub) = scrub
                && std::env::var_os("GITHUB_ACTIONS").is_some_and(|value| value == "true")
            {
                scrub
                    .mask_github(&mut std::io::stderr().lock())
                    .map_err(|error| BuildError::Request(error.to_string()))?;
            }
            ployz_build::execute(request, cancellation, scrub, observe)
        })?;
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
    /// same Build again, producing no image. Without that service, or when the Build
    /// reads a Sealed value's plaintext, it does nothing: other workflows read that cache.
    ///
    /// # Errors
    /// Refuses a capture that is not exactly one target, and reports the export's failure.
    pub fn export_local_cache(
        &self,
        cancellation: &ployz_build::Cancellation,
    ) -> Result<(), Error> {
        self.run_local(|request, scrub| {
            ployz_build::export_cache(request, cancellation, scrub).map(|()| Vec::new())
        })
        .map(drop)
    }

    /// Run `execute` on the single captured target, with the runner's cache service
    /// variables and the Sealed values it holds staged for this solve alone.
    fn run_local(
        &self,
        execute: impl FnOnce(
            &ployz_build::Request<'_>,
            Option<&OutputScrub>,
        ) -> Result<Vec<BuiltImage>, BuildError>,
    ) -> Result<(&CapturedTarget, Vec<BuiltImage>), Error> {
        let [captured] = self.targets.as_slice() else {
            return Err(invalid(
                "a local Build takes exactly one Git-sourced Service",
            ));
        };
        if !captured.sealed.is_empty() && self.opened.is_none() {
            let names = captured.sealed.join(", ");
            let verb = if captured.sealed.len() == 1 {
                "is"
            } else {
                "are"
            };
            return Err(Error::Sealed(format!(
                "{names} {verb} sealed. {SEALED_BUILDS_RUN_ON_SERVER}"
            )));
        }
        let root = captured.inputs.root();
        let definition = Definition {
            retained_tags: Vec::new(),
            image_contexts: BTreeMap::new(),
            targets: vec![captured.target.clone()],
            output: Output::Load,
            no_cache: false,
            pull: false,
        };
        let build = |source| Error::Build {
            service: captured.name.clone(),
            source,
        };
        let staged = self
            .opened
            .as_ref()
            .map(|opened| Staged::stage(root, opened, &definition.targets))
            .transpose()
            .map_err(build)?;
        let sealed_override = staged.as_ref().and_then(Staged::compose_override);
        let railpack = ployz_build::remote::validate_capture(
            root,
            &definition,
            ployz_build::remote::SealedFiles::Opened,
        )
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
        if ployz_build::reads_plaintext(sealed_override, &railpack) {
            // Buildx provenance metadata records build argument values.
            environment.insert("BUILDX_METADATA_PROVENANCE".into(), "disabled".into());
        }
        let scrub = self.opened.as_ref().map(OutputScrub::new);
        let request = ployz_build::Request {
            image_contexts: &definition.image_contexts,
            compose_file: Path::new("compose.yaml"),
            sealed_override,
            working_dir: root,
            environment: &environment,
            docker: None,
            targets: &definition.targets,
            railpack: &railpack,
            build_args: &[],
            output: definition.output,
            no_cache: false,
            pull: false,
            sealed: self.opened.is_some(),
        };
        let images = execute(&request, scrub.as_ref()).map_err(build)?;
        Ok((captured, images))
    }
}

fn path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::{BuildSpec, Recipe, capture, tests::intent};
    use ployz_core::{DeployIntent, OpenedSealed};
    use std::fs;

    const MARKER: &str = "marker-sealed-0123456789";

    fn dockerfile(root: &Path, dockerfile: &str) -> BuildSpec {
        let source = root.join("repository");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("Dockerfile"), dockerfile).unwrap();
        BuildSpec {
            context: source.clone(),
            recipe: Recipe::Dockerfile(source.join("Dockerfile")),
        }
    }

    fn sealed(build: &CapturedBuild) -> Option<String> {
        let root = build.targets.first().unwrap().inputs.root();
        fs::read_to_string(root.join(ployz_build::sealed_file("NPM_TOKEN"))).ok()
    }

    #[test]
    fn a_runner_holding_its_sealed_values_stages_them_for_each_solve_alone() {
        let root = tempfile::tempdir().unwrap();
        let spec = dockerfile(
            root.path(),
            "FROM scratch\nARG NPM_TOKEN\nRUN --mount=type=secret,id=DATABASE_URL true\n",
        );
        let intent = sealed_intent();
        let name = intent.target.first().unwrap().name.clone();
        let opened = OpenedSealed::new(
            BTreeMap::from([
                ("NPM_TOKEN".into(), MARKER.into()),
                ("DATABASE_URL".into(), "postgres://marker".into()),
            ]),
            Vec::new(),
        );
        let build = capture(&intent, BTreeMap::from([(name, spec)]), Some(opened)).unwrap();
        for _ in 0..2 {
            build
                .run_local(|request, scrub| {
                    assert!(request.sealed && scrub.is_some());
                    let args = fs::read_to_string(
                        request.working_dir.join(request.sealed_override.unwrap()),
                    )
                    .unwrap();
                    assert!(args.contains(MARKER), "{args}");
                    assert_eq!(
                        request.environment["BUILDX_METADATA_PROVENANCE"],
                        "disabled"
                    );
                    assert_eq!(sealed(&build).as_deref(), Some(MARKER));
                    Ok(Vec::new())
                })
                .unwrap();
            assert_eq!(sealed(&build), None);
        }
    }

    #[test]
    fn a_build_without_sealed_values_solves_as_it_always_did() {
        let root = tempfile::tempdir().unwrap();
        let mut intent = intent(&["api"]);
        let name = intent.target.first().unwrap().name.clone();
        intent
            .target
            .first_mut()
            .unwrap()
            .container
            .environment
            .insert("PLAIN".into(), "visible".into());
        let spec = dockerfile(root.path(), "FROM scratch\n");
        let build = capture(&intent, BTreeMap::from([(name, spec)]), None).unwrap();
        build
            .run_local(|request, scrub| {
                assert!(!request.sealed && scrub.is_none());
                assert_eq!(request.sealed_override, None);
                assert!(
                    !request
                        .environment
                        .contains_key("BUILDX_METADATA_PROVENANCE")
                );
                Ok(Vec::new())
            })
            .unwrap();
    }

    #[test]
    fn a_railpack_build_planned_with_a_sealed_setting_records_no_provenance() {
        let root = tempfile::tempdir().unwrap();
        let mut intent = intent(&["api"]);
        let container = &mut intent.target.first_mut().unwrap().container;
        container.environment.insert(
            "RAILPACK_BUILD_CMD".into(),
            ployz_core::EnvValue::Sealed(ployz_core::SealedValue {
                sealed: ployz_core::Ciphertext::from_bytes(vec![7; 48]),
                scope: ployz_core::SealedScope::Shared,
                key: "RAILPACK_BUILD_CMD".into(),
            }),
        );
        container.sealed_identity = Some(ployz_core::SealedIdentity {
            organization: "org_test".into(),
            environment: uuid::Uuid::from_u128(1),
            service: uuid::Uuid::from_u128(2),
        });
        let name = intent.target.first().unwrap().name.clone();
        let spec = BuildSpec {
            context: root.path().to_owned(),
            recipe: Recipe::Railpack { command: None },
        };
        let opened = OpenedSealed::new(
            BTreeMap::from([("RAILPACK_BUILD_CMD".into(), MARKER.into())]),
            Vec::new(),
        );
        let build = capture(&intent, BTreeMap::from([(name, spec)]), Some(opened)).unwrap();
        build
            .run_local(|request, _| {
                assert_eq!(request.sealed_override, None);
                assert_eq!(
                    request.environment["BUILDX_METADATA_PROVENANCE"],
                    "disabled"
                );
                Ok(Vec::new())
            })
            .unwrap();
    }

    fn sealed_intent() -> DeployIntent {
        let mut intent = intent(&["api"]);
        let container = &mut intent.target.first_mut().unwrap().container;
        for name in ["NPM_TOKEN", "DATABASE_URL"] {
            container.environment.insert(
                name.into(),
                ployz_core::EnvValue::Sealed(ployz_core::SealedValue {
                    sealed: ployz_core::Ciphertext::from_bytes(vec![7; 48]),
                    scope: ployz_core::SealedScope::Shared,
                    key: name.into(),
                }),
            );
        }
        container.sealed_identity = Some(ployz_core::SealedIdentity {
            organization: "org_test".into(),
            environment: uuid::Uuid::from_u128(1),
            service: uuid::Uuid::from_u128(2),
        });
        intent
    }

    #[test]
    fn a_build_reading_sealed_values_is_refused_here_by_name() {
        let root = tempfile::tempdir().unwrap();
        let spec = dockerfile(root.path(), "FROM scratch\n");
        let intent = sealed_intent();
        let name = intent.target.first().unwrap().name.clone();
        let build = capture(&intent, BTreeMap::from([(name, spec)]), None).unwrap();
        let error = build
            .run_local(|_, _| panic!("a sealed Build must not reach Docker"))
            .err()
            .unwrap()
            .to_string();
        assert_eq!(
            error,
            "DATABASE_URL, NPM_TOKEN are sealed. Builds that read a sealed value run on your server."
        );
    }
}
