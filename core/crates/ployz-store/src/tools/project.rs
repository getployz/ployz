//! The tool that creates a Project with its Default Environment.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::machine::{Cx, Stop, Tool, at};
use super::refusal::{Call, Refusal};
use super::step::{AttemptId, RequestKey};
use crate::catalog::NODE_NAME;
use crate::{CreateProject, EnvironmentId, ProjectId, ProjectName};

const REQUEST_KEY: &str = "^[A-Za-z0-9_-]{8,64}$";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectCreate {
    /// The new Project's name, unique in the Organization.
    #[schemars(with = "String", pattern(NODE_NAME), example = &"docs")]
    name: ProjectName,
    /// Any id you choose. Repeat the same call with the same key after a lost reply and you get the same Project back, not a second one.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(REQUEST_KEY))]
    request_key: Option<RequestKey>,
}

impl Tool for ProjectCreate {
    const NAME: &'static str = "project_create";
    const DESCRIPTION: &'static str = "Create a Project with its Default Environment, production. Then add Services to it with service_add, naming the Project.";
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "name": "docs", "request_key": "project-docs-0001" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let key = cx.request_key(self.request_key);
        // The Store replays a create by each of its IDs, so the two must differ.
        let id = ProjectId::parse(AttemptId::derive(Self::NAME, &key).to_string());
        let environment = EnvironmentId::parse(
            AttemptId::derive(&format!("{}-environment", Self::NAME), &key).to_string(),
        );
        let (Ok(id), Ok(default_environment)) = (id, environment) else {
            return Err(Refusal::internal("An ID did not mint").into());
        };
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
