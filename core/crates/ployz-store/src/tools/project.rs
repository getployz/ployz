//! Tools on Projects: create one with its Default Environment, rename one.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::machine::{Cx, Effect, Stop, Tool, at};
use super::refusal::Call;
use super::step::RequestKey;
use crate::catalog::NODE_NAME;
use crate::{CreateProject, EnvironmentId, ProjectId, ProjectName, RenameProject};

const REQUEST_KEY: &str = "^[A-Za-z0-9_-]{8,64}$";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectCreate {
    /// The new Project's name, unique in the Organization.
    #[schemars(with = "String", pattern(NODE_NAME), example = &"docs")]
    name: ProjectName,
    /// Any id you choose. A call that repeats a key gets the Project the first call made, not a second one.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(REQUEST_KEY))]
    request_key: Option<RequestKey>,
}

impl Tool for ProjectCreate {
    const NAME: &'static str = "project_create";
    const DESCRIPTION: &'static str =
        "Create a Project with its Default Environment, production. It starts with no Services.";
    const EFFECT: Effect = Effect::Change;

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
        })?;
        Ok(json!({
            "at": at(&created.environment),
            "project": created.project.name,
            "environment": created.environment.name,
            "next": Call::new("environment_show", json!({ "project": created.project.name })),
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectRename {
    #[schemars(with = "String", pattern(NODE_NAME))]
    project: ProjectName,
    /// New name, unique in the Organization.
    #[schemars(with = "String", pattern(NODE_NAME))]
    to: ProjectName,
}

impl Tool for ProjectRename {
    const NAME: &'static str = "project_rename";
    const DESCRIPTION: &'static str =
        "Rename a Project. Applies at once; its Environments keep running.";
    const EFFECT: Effect = Effect::Change;

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
