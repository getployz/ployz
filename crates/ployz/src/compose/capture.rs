//! Captured Compose candidates retain source selection and inputs across review and deployment.

use std::{collections::BTreeMap, path::PathBuf};

use ployz_core::{ComposePruneRefusal, DeployIntent, PlanOptions, ProjectName};
use serde::{Deserialize, Serialize};

use super::{BuildSpec, ComposeProject, model::ProjectSecret};

/// Source selection used when capturing a Compose candidate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComposeSource {
    pub working_dir: PathBuf,
    /// Empty means Compose's default file discovery was requested.
    pub requested_files: Vec<PathBuf>,
}

/// Captured input, including unresolved providers. Never a display payload.
/// Review exposes only setting evidence; capture serialization can contain secrets.
#[derive(Clone, Serialize, Deserialize)]
pub struct CapturedCompose {
    id: String,
    intent: DeployIntent,
    source: ComposeSource,
    builds: BTreeMap<String, BuildSpec>,
    secrets: BTreeMap<String, ProjectSecret>,
    environment: BTreeMap<String, String>,
    context: Option<String>,
    warnings: Vec<String>,
}

impl CapturedCompose {
    /// Use each Service's completed content for Containers and hooks. A tag
    /// overwritten by another Service or client cannot substitute its image.
    pub fn bind_builds(
        &mut self,
        builds: &[super::BuiltService],
    ) -> Result<(), super::ComposeError> {
        for service in &mut self.intent.target {
            if let Some(build) = builds
                .iter()
                .find(|build| build.name == service.name.as_str())
            {
                service.container.image =
                    build
                        .built
                        .repository_reference(&build.image)
                        .map_err(|error| super::ComposeError::Build {
                            services: build.name.clone(),
                            source: error,
                        })?;
                service.container.pull_policy = ployz_core::PullPolicy::Never;
            }
        }
        Ok(())
    }

    /// Identity of this capture, unchanged by later edits to the source files.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Runtime target and selection frozen for this candidate.
    #[must_use]
    pub fn intent(&self) -> &DeployIntent {
        &self.intent
    }

    /// Compose location and file selection used to load this candidate.
    #[must_use]
    pub fn source(&self) -> &ComposeSource {
        &self.source
    }
}

impl ComposeProject {
    /// Lower loaded services with dependency and profile selection preserved.
    #[must_use]
    pub fn deploy_intent(&self, project_name: ProjectName, options: PlanOptions) -> DeployIntent {
        DeployIntent::from_named_specs(project_name, &self.services, &self.dependencies, options)
            .with_service_profiles(self.service_profiles())
    }

    /// Capture once without builds, provider resolution, or Cluster operations.
    #[must_use]
    pub fn capture(
        self,
        project_name: ProjectName,
        options: PlanOptions,
        requested_profiles: Vec<String>,
        compose_refusal: Option<ComposePruneRefusal>,
        requested_files: Vec<PathBuf>,
    ) -> CapturedCompose {
        let intent = self
            .deploy_intent(project_name, options)
            .with_requested_profiles(requested_profiles)
            .with_compose_refusal(compose_refusal);
        CapturedCompose {
            id: uuid::Uuid::new_v4().to_string(),
            intent,
            source: ComposeSource {
                working_dir: self.working_dir,
                requested_files,
            },
            builds: self.builds,
            secrets: self.secrets,
            environment: self.environment,
            context: self.context,
            warnings: self.warnings,
        }
    }
}

#[cfg(test)]
#[test]
fn deploy_binds_each_service_to_its_build_when_requested_tags_are_shared() {
    use super::build::BuiltService;
    use crate::compose::{BuildLocation, parse_normalized};
    let first_content = format!("sha256:{}", "1".repeat(64));
    let second_content = format!("sha256:{}", "2".repeat(64));
    let project = parse_normalized(
        "services: {one: {image: 'example.test/shared:latest', build: .}, two: {image: 'example.test/shared:latest', build: .}}",
        ".",
    ).unwrap();
    let mut candidate = project.capture(
        ployz_core::ProjectName::parse("app").unwrap(),
        Default::default(),
        vec![],
        None,
        vec![],
    );
    let builds = [
        ("one", first_content.as_str()),
        ("two", second_content.as_str()),
    ]
    .map(|(name, digest)| BuiltService {
        name: name.into(),
        _retention: None,
        image: "example.test/shared:latest".into(),
        machines: vec![],
        location: BuildLocation::Machine(ployz_core::MachineId::parse("a".repeat(32)).unwrap()),
        built: ployz_build::BuiltImage {
            reference: digest.into(),
            tags: vec![
                "auxiliary.test:5000/other:extra".into(),
                "example.test/shared:latest".into(),
            ],
            platforms: vec!["linux/amd64".into()],
            location: "unix:///var/run/docker.sock".into(),
        },
    });
    candidate.bind_builds(&builds).unwrap();
    for (name, digest) in [
        ("one", first_content.as_str()),
        ("two", second_content.as_str()),
    ] {
        let service = candidate
            .intent()
            .target
            .iter()
            .find(|service| service.name.as_str() == name)
            .unwrap();
        assert_eq!(
            service.container.image,
            format!("example.test/shared@{digest}")
        );
        assert_eq!(service.container.pull_policy, ployz_core::PullPolicy::Never);
    }
}
