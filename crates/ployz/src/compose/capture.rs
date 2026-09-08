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
