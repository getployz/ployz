//! Creating Projects and Environments.

use ployz_core::RpcError;
use ployz_core::config::SavedEnvironmentIntent;
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::error;
use crate::id::{EnvironmentId, EnvironmentName, ProjectId, ProjectName, Revision};
use crate::scope::{self, EnvironmentSummary, Project};
use crate::storage::Tx;

/// The Default Environment every new Project starts with.
const DEFAULT_ENVIRONMENT: &str = "production";

/// Create a Project with its Default Environment, `production`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CreateProject {
    /// The new Project's ID.
    pub id: ProjectId,
    /// Its name, unique in the Organization.
    pub name: ProjectName,
    /// The ID of its Default Environment, created with it.
    pub default_environment: EnvironmentId,
}

/// The new Project and its Default Environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ProjectCreated {
    /// The Project.
    pub project: ProjectSummary,
    /// Its Default Environment.
    pub environment: EnvironmentSummary,
}

/// A Project as results name it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ProjectSummary {
    /// Its durable identity.
    pub id: ProjectId,
    /// Its name.
    pub name: ProjectName,
}

/// Rename a Project. Its Environments keep the Namespaces they run in on the Servers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RenameProject {
    /// The Project, by its current name.
    pub project: ProjectName,
    /// Its new name, unique in the Organization.
    pub name: ProjectName,
}

/// Create an empty Environment in a Project.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CreateEnvironment {
    /// The new Environment's ID.
    pub id: EnvironmentId,
    /// The Project to create it in; omitted means the Organization's only Project.
    #[serde(default)]
    pub project: Option<ProjectName>,
    /// Its name, unique in the Project.
    pub name: EnvironmentName,
}

/// The new, empty Environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct EnvironmentCreated {
    /// The Environment.
    pub environment: EnvironmentSummary,
}


pub(crate) fn create_project(
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
    let project = Project {
        id: create.id.clone(),
        name: create.name.clone(),
        default_environment: create.default_environment.clone(),
    };
    let name = EnvironmentName::parse(DEFAULT_ENVIRONMENT).expect("a valid name");
    let default_environment =
        insert_environment(tx, who, &project, &create.default_environment, name)?;
    Ok(ProjectCreated {
        project: ProjectSummary {
            id: create.id.clone(),
            name: create.name.clone(),
        },
        environment: default_environment,
    })
}


pub(crate) fn create_environment(
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
    let environment = insert_environment(tx, who, &project, &create.id, create.name.clone())?;
    Ok(EnvironmentCreated { environment })
}

/// Insert an empty Environment.
pub(crate) fn insert_environment(
    tx: &mut dyn Tx,
    who: &Actor,
    project: &Project,
    id: &EnvironmentId,
    name: EnvironmentName,
) -> Result<EnvironmentSummary, RpcError> {
    let working = SavedEnvironmentIntent {
        version: 1,
        environment_slug: name.to_string(),
        services: Vec::new(),
        volumes: Vec::new(),
    };
    let revision = Revision(1);
    tx.execute(
        "INSERT INTO config_environment \
         (id, organization_id, project_id, name, working_revision, working) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        &[
            id.as_str().into(),
            who.organization.as_str().into(),
            project.id.as_str().into(),
            name.as_str().into(),
            scope::revision_param(revision)?.into(),
            serde_json::to_string(&working)
                .expect("Working State is JSON")
                .as_str()
                .into(),
        ],
    )?;
    Ok(EnvironmentSummary {
        id: id.clone(),
        project: project.name.clone(),
        name,
        revision,
    })
}

pub(crate) fn rename_project(
    tx: &mut dyn Tx,
    who: &Actor,
    rename: &RenameProject,
) -> Result<ProjectSummary, RpcError> {
    let project = scope::project(tx, who, Some(&rename.project))?;
    if rename.name != project.name {
        let taken = tx.query(
            "SELECT id FROM config_project WHERE organization_id = ?1 AND name = ?2",
            &[
                who.organization.as_str().into(),
                rename.name.as_str().into(),
            ],
        )?;
        if !taken.is_empty() {
            return Err(error::conflict(
                format!("A Project named {} already exists", rename.name),
                json!({ "project": rename.name }),
            ));
        }
        tx.execute(
            "UPDATE config_project SET name = ?1 WHERE id = ?2",
            &[rename.name.as_str().into(), project.id.as_str().into()],
        )?;
    }
    Ok(ProjectSummary {
        id: project.id,
        name: rename.name.clone(),
    })
}
