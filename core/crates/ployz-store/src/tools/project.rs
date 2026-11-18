//! Tools on Projects: create one with its Default Environment, rename one.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::ToolId;
use super::input::tool_input;
use super::machine::{Cx, Effect, Stop, Tool};
use super::refusal::Call;
use crate::catalog::NODE_NAME;
use crate::{CreateProject, EnvironmentId, ProjectId, ProjectName, RenameProject};

tool_input! {
    pub(crate) struct ProjectCreate {
        /// Name for the new Project, unique in the Organization.
        #[schemars(with = "String", pattern(NODE_NAME), example = &"docs")]
        name: ProjectName,
        @key["A key you choose. Send it again and you get back the Project it created, not a failure for a taken name."],
    }
}

impl Tool for ProjectCreate {
    const NAME: &'static str = "project_create";
    const TITLE: &'static str = "Create a Project";
    const DESCRIPTION: &'static str = "Create a Project. It comes with a Default Environment, production, which starts Protected and empty. Fails if the name is taken.";
    // A repeat with the same key replays the create; one without is refused as a taken name.
    const EFFECT: Effect = Effect::Change { idempotent: true };

    fn example() -> Value {
        json!({ "name": "docs", "request_key": "q8Lz3fWn2KcT" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let key = cx.request_key(self.request_key);
        // The Store replays a create by each of its IDs, so the two must differ.
        let id: ProjectId = cx.mint_as(Self::NAME, &key)?;
        let default_environment: EnvironmentId =
            cx.mint_as(&format!("{}-environment", Self::NAME), &key)?;
        let created = cx.write(&CreateProject {
            id,
            name: self.name,
            default_environment,
            region: None,
        })?;
        Ok(json!({
            "at": created.environment.to_string(),
            "project": created.project.name,
            "environment": created.environment.name,
            "next": Call::to(ToolId::EnvironmentShow, json!({ "project": created.project.name })),
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectRename {
    /// The Project's current name.
    #[schemars(with = "String", pattern(NODE_NAME))]
    project: ProjectName,
    /// Its new name, unique in the Organization.
    #[schemars(with = "String", pattern(NODE_NAME))]
    to: ProjectName,
}

impl Tool for ProjectRename {
    const NAME: &'static str = "project_rename";
    const TITLE: &'static str = "Rename a Project";
    const DESCRIPTION: &'static str =
        "Rename a Project. Takes effect right away, and its Environments keep running.";
    const EFFECT: Effect = Effect::Change { idempotent: true };

    fn example() -> Value {
        json!({ "project": "docs", "to": "handbook" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let renamed = cx.write(&RenameProject {
            project: self.project.clone(),
            name: self.to,
        })?;
        Ok(json!({
            "at": renamed.name,
            "project": renamed.name,
            "renamed_from": self.project,
        }))
    }
}
