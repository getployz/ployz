//! What outlives creation: listing Projects and a Project's Environments,
//! choosing its Default Environment, and forgetting an Organization's
//! configuration. Removing an Environment or a Project is [`crate::removal`].

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::deployment::{self, DeploymentSummary};
use crate::error;
use crate::id::{
    DeploymentId, EnvironmentId, EnvironmentName, OrganizationId, ProjectId, ProjectName,
    VolumeName,
};
use crate::project::ProjectSummary;
use crate::scope::{self, EnvironmentRef};
use crate::storage::Tx;

/// List a Project's Environments.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentsQuery {
    /// The Project; omitted means the Organization's only Project.
    #[serde(default)]
    pub project: Option<ProjectName>,
}

/// A Project's Environments, by name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct EnvironmentsView {
    pub project: ProjectSummary,
    pub environments: Vec<EnvironmentListing>,
}

/// One Environment as `env ls` shows it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct EnvironmentListing {
    pub id: EnvironmentId,
    pub name: EnvironmentName,
    /// Whether it is the Project's Default Environment.
    pub default: bool,
    /// The Environment it is a Branch of, if it is one.
    pub parent: Option<EnvironmentName>,
    /// Its latest Deployment, when that removes it from the Servers.
    pub removal: Option<DeploymentSummary>,
    /// What a new Branch of it runs when it names no Setup Commands.
    pub branch_setup: Vec<crate::SetupCommand>,
}

/// Make an Environment its Project's Default Environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SetDefaultEnvironment {
    pub environment: EnvironmentRef,
}

/// List the Organization's Projects.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ProjectsQuery {}

/// The Organization's Projects, by name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ProjectsView {
    pub projects: Vec<ProjectListing>,
}

/// One Project as `project ls` shows it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ProjectListing {
    pub id: ProjectId,
    pub name: ProjectName,
    pub default_environment: EnvironmentName,
    /// Its Environments, by name.
    pub environments: Vec<EnvironmentName>,
}

/// The Organization whose configuration the Store forgot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct OrganizationRemoved {
    pub organization: OrganizationId,
}

pub(crate) fn environments(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &EnvironmentsQuery,
) -> Result<EnvironmentsView, RpcError> {
    let project = scope::project(tx, who, query.project.as_ref())?;
    let rows = tx.query(
        "SELECT e.id, e.name, p.name, e.branch_setup FROM config_environment e \
         LEFT JOIN config_environment_branch b ON b.environment_id = e.id \
         LEFT JOIN config_environment p ON p.id = b.parent_id \
         WHERE e.project_id = ?1 ORDER BY e.name",
        &[project.id.as_str().into()],
    )?;
    let mut environments = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row.parse::<EnvironmentId>(0, "Environment")?;
        let parent = row.parse_optional::<EnvironmentName>(2, "Environment")?;
        environments.push(EnvironmentListing {
            default: id == project.default_environment,
            name: row.parse::<EnvironmentName>(1, "Environment")?,
            parent,
            removal: crate::removal::latest(tx, &id)?,
            branch_setup: match row.optional_text(3)? {
                Some(_) => row.json(3, "Branch setup")?,
                None => Vec::new(),
            },
            id,
        });
    }
    Ok(EnvironmentsView {
        project: ProjectSummary {
            id: project.id,
            name: project.name,
        },
        environments,
    })
}

pub(crate) fn set_default(
    tx: &mut dyn Tx,
    who: &Actor,
    set: &SetDefaultEnvironment,
) -> Result<EnvironmentsView, RpcError> {
    let environment = scope::lock(tx, who, &set.environment)?;
    crate::removal::marked(tx, &environment)?;
    tx.execute(
        "UPDATE config_project SET default_environment_id = ?1 \
         WHERE organization_id = ?2 AND name = ?3",
        &[
            environment.summary.id.as_str().into(),
            who.organization.as_str().into(),
            environment.summary.project.as_str().into(),
        ],
    )?;
    environments(
        tx,
        who,
        &EnvironmentsQuery {
            project: Some(environment.summary.project),
        },
    )
}

pub(crate) fn projects(tx: &mut dyn Tx, who: &Actor) -> Result<ProjectsView, RpcError> {
    let rows = tx.query(
        "SELECT p.id, p.name, d.name, e.name FROM config_project p \
         JOIN config_environment d ON d.id = p.default_environment_id \
         JOIN config_environment e ON e.project_id = p.id \
         WHERE p.organization_id = ?1 ORDER BY p.name, e.name",
        &[who.organization.as_str().into()],
    )?;
    let mut projects: Vec<ProjectListing> = Vec::new();
    for row in rows {
        let id = row.parse::<ProjectId>(0, "Project")?;
        let environment = row.parse::<EnvironmentName>(3, "Project")?;
        match projects.last_mut() {
            Some(project) if project.id == id => project.environments.push(environment),
            _ => projects.push(ProjectListing {
                id,
                name: row.parse::<ProjectName>(1, "Project")?,
                default_environment: row.parse::<EnvironmentName>(2, "Project")?,
                environments: vec![environment],
            }),
        }
    }
    Ok(ProjectsView { projects })
}

pub(crate) fn remove_organization(
    tx: &mut dyn Tx,
    who: &Actor,
) -> Result<OrganizationRemoved, RpcError> {
    let projects = projects(tx, who)?.projects;
    if let Some(first) = projects.first() {
        let names: Vec<_> = projects
            .iter()
            .map(|project| project.name.to_string())
            .collect();
        return Err(error::conflict(
            format!(
                "This Organization still has Projects ({}). Remove them first",
                names.join(", ")
            ),
            json!({
                "projects": names,
                "next": crate::removal::next("remove", json!({ "project": first.name })),
            }),
        ));
    }
    for table in [
        "config_create",
        "config_branch",
        "config_check_suite",
        "config_build_order",
        "config_pull_request",
    ] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE organization_id = ?1"),
            &[who.organization.as_str().into()],
        )?;
    }
    Ok(OrganizationRemoved {
        organization: who.organization.clone(),
    })
}

/// A Volume Applied State holds: a Deploy put it on the Servers, so they may hold
/// its data.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
pub struct AppliedVolume {
    pub project: ProjectName,
    pub environment: EnvironmentName,
    pub volume: VolumeName,
}

/// Every Volume Applied State holds in `who`'s Organization, sorted: exactly what
/// [`forget_cluster`] lets go of.
pub(crate) fn applied_volumes(
    tx: &mut dyn Tx,
    who: &Actor,
) -> Result<Vec<AppliedVolume>, RpcError> {
    let rows = tx.query(
        "SELECT p.name, e.name, a.node FROM config_applied a \
         JOIN config_environment e ON e.id = a.environment_id \
         JOIN config_project p ON p.id = e.project_id \
         WHERE a.organization_id = ?1 AND a.node_type = 'volume'",
        &[who.organization.as_str().into()],
    )?;
    let mut volumes = rows
        .iter()
        .map(|row| {
            let node: ployz_core::config::SavedVolumeIntent = row.json(2, "Applied Volume")?;
            Ok(AppliedVolume {
                project: row.parse(0, "Applied Volume")?,
                environment: row.parse(1, "Applied Volume")?,
                volume: VolumeName::try_from(node.name)
                    .map_err(|_| error::corrupt("Applied Volume"))?,
            })
        })
        .collect::<Result<Vec<_>, RpcError>>()?;
    volumes.sort();
    Ok(volumes)
}

/// Forget what `who`'s Organization's Servers held once its Cluster is gone (see
/// [`crate::SystemEvent::ClusterForgotten`]): cancel every Deployment that might still
/// run, and let go of Applied State, each Environment's Namespace and its Volumes'
/// fixed storage. Returns the cancelled Deployments.
pub(crate) fn forget_cluster(tx: &mut dyn Tx, who: &Actor) -> Result<Vec<DeploymentId>, RpcError> {
    let organization = who.organization.as_str();
    let environments = tx
        .query(
            "SELECT id FROM config_environment WHERE organization_id = ?1",
            &[organization.into()],
        )?
        .iter()
        .map(|row| row.parse::<EnvironmentId>(0, "Environment"))
        .collect::<Result<Vec<_>, RpcError>>()?;
    // Every Deployment write locks its Environment; so does this, against a concurrent admission.
    scope::lock_all(tx, environments)?;
    let cancelled = deployment::cancel_in_flight(tx, who)?;
    for table in ["config_applied", "config_namespace"] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE organization_id = ?1"),
            &[organization.into()],
        )?;
    }
    tx.execute(
        "DELETE FROM config_volume_storage WHERE environment_id IN \
         (SELECT id FROM config_environment WHERE organization_id = ?1)",
        &[organization.into()],
    )?;
    Ok(cancelled)
}
