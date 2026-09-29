//! Creating Projects and Environments.

use ployz_core::config::SavedEnvironmentIntent;
use ployz_core::{Namespace, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::error;
use crate::id::{EnvironmentId, EnvironmentName, ProjectId, ProjectName, Revision};
use crate::scope::{self, EnvironmentSummary};
use crate::storage::Tx;

/// The Default Environment every new Project starts with.
const DEFAULT_ENVIRONMENT: &str = "production";

/// Create a Project with its Default Environment, `production`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct CreateProject {
    pub id: ProjectId,
    pub name: ProjectName,
    pub default_environment: EnvironmentId,
}

/// The new Project and its Default Environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ProjectCreated {
    pub project: ProjectSummary,
    pub environment: EnvironmentSummary,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ProjectSummary {
    pub id: ProjectId,
    pub name: ProjectName,
}

/// Create an empty Environment in a Project.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct CreateEnvironment {
    pub id: EnvironmentId,
    #[serde(default)]
    pub project: Option<ProjectName>,
    pub name: EnvironmentName,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct EnvironmentCreated {
    pub environment: EnvironmentSummary,
}

pub(super) fn create_project(
    tx: &mut dyn Tx,
    who: &Actor,
    create: &CreateProject,
) -> Result<ProjectCreated, RpcError> {
    let taken = tx.query(
        "SELECT id FROM config_project WHERE organization_id = ?1 AND name = ?2",
        &[
            who.organization.as_str().into(),
            create.name.as_str().into(),
        ],
    )?;
    if !taken.is_empty() {
        return Err(error::conflict(
            format!("A Project named {} already exists", create.name),
            json!({ "project": create.name }),
        ));
    }
    tx.execute(
        "INSERT INTO config_project (id, organization_id, name, default_environment_id) \
         VALUES (?1, ?2, ?3, ?4)",
        &[
            create.id.as_str().into(),
            who.organization.as_str().into(),
            create.name.as_str().into(),
            create.default_environment.as_str().into(),
        ],
    )?;
    let name = EnvironmentName::parse(DEFAULT_ENVIRONMENT).expect("a valid name");
    let default_environment = insert_environment(
        tx,
        who,
        (&create.id, &create.name),
        &create.default_environment,
        name,
    )?;
    Ok(ProjectCreated {
        project: ProjectSummary {
            id: create.id.clone(),
            name: create.name.clone(),
        },
        environment: default_environment,
    })
}

pub(super) fn create_environment(
    tx: &mut dyn Tx,
    who: &Actor,
    create: &CreateEnvironment,
) -> Result<EnvironmentCreated, RpcError> {
    let project = scope::project(tx, who, create.project.as_ref())?;
    let taken = tx.query(
        "SELECT id FROM config_environment WHERE project_id = ?1 AND name = ?2",
        &[project.id.as_str().into(), create.name.as_str().into()],
    )?;
    if !taken.is_empty() {
        return Err(error::conflict(
            format!(
                "Project {} already has an Environment named {}",
                project.name, create.name
            ),
            json!({ "project": project.name, "environment": create.name }),
        ));
    }
    let environment = insert_environment(
        tx,
        who,
        (&project.id, &project.name),
        &create.id,
        create.name.clone(),
    )?;
    Ok(EnvironmentCreated { environment })
}

/// Insert an empty Environment deployed to the Namespace `PROJECT-ENVIRONMENT`.
fn insert_environment(
    tx: &mut dyn Tx,
    who: &Actor,
    (project_id, project): (&ProjectId, &ProjectName),
    id: &EnvironmentId,
    name: EnvironmentName,
) -> Result<EnvironmentSummary, RpcError> {
    let namespace = Namespace::parse(format!("{project}-{name}")).map_err(|_| {
        error::invalid(
            "The Project and Environment names together must fit a 63-character Namespace",
            json!({ "project": project, "environment": name }),
        )
    })?;
    let taken = tx.query(
        "SELECT id FROM config_environment WHERE organization_id = ?1 AND namespace = ?2",
        &[who.organization.as_str().into(), namespace.as_str().into()],
    )?;
    if !taken.is_empty() {
        return Err(error::conflict(
            format!("Another Environment already deploys to Namespace {namespace}"),
            json!({ "namespace": namespace }),
        ));
    }
    let working = SavedEnvironmentIntent {
        version: 1,
        environment_slug: name.to_string(),
        services: Vec::new(),
        volumes: Vec::new(),
    };
    let revision = Revision(1);
    tx.execute(
        "INSERT INTO config_environment \
         (id, organization_id, project_id, name, namespace, working_revision, working) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        &[
            id.as_str().into(),
            who.organization.as_str().into(),
            project_id.as_str().into(),
            name.as_str().into(),
            namespace.as_str().into(),
            scope::revision_param(revision)?.into(),
            serde_json::to_string(&working)
                .expect("Working State is JSON")
                .as_str()
                .into(),
        ],
    )?;
    Ok(EnvironmentSummary {
        id: id.clone(),
        project: project.clone(),
        name,
        namespace,
        revision,
    })
}
