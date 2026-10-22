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

/// The Environment a protection write changed, and its Project's Environments after it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ProtectedView {
    pub environment: EnvironmentId,
    #[serde(flatten)]
    #[ts(flatten)]
    pub view: EnvironmentsView,
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
    /// Its removal from the Servers: asked and waiting for the sweep, or its
    /// latest Deployment when that removes it. None when neither.
    pub removal: Option<EnvironmentRemoval>,
    /// Whether it is a PR Environment whose pull request merged.
    pub merged: bool,
    /// What a new Branch of it runs when it names no Setup Commands.
    pub branch_setup: Vec<crate::SetupCommand>,
    /// Whether it refuses removal until it is unprotected.
    pub protected: bool,
    /// How many times it was protected: an approval to unprotect it holds for one
    /// protection, so protecting it again asks again.
    pub protections: u32,
}

/// Where an Environment's removal from the Servers stands.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnvironmentRemoval {
    /// Asked, and waiting for the sweep to start the Deployment that removes it.
    Pending {
        because: RemovedBecause,
        /// When it was asked, in Unix seconds.
        #[ts(type = "number")]
        since: i64,
    },
    /// The Deployment that removes it, running or ended.
    Deployment(Box<DeploymentSummary>),
}

/// Why an Environment is being removed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RemovedBecause {
    /// Someone removed it, or its Project.
    Removed,
    /// Its pull request closed.
    PullRequest,
    /// It went unused for too long.
    Idle,
    /// It synced into its Parent and wasn't kept.
    Synced,
}

/// Make an Environment its Project's Default Environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SetDefaultEnvironment {
    pub environment: EnvironmentRef,
}

/// Protect a Project's own Environment, or unprotect it: a Protected Environment
/// refuses removal on its own.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SetEnvironmentProtected {
    #[serde(default)]
    pub environment: EnvironmentRef,
    pub protected: bool,
    /// The Environment and protection Cloud read when it decided whether a human must
    /// say yes to an agent's unprotect: refused `conflict` when `environment` now names
    /// another Environment or another write changed its protection since.
    /// None applies to whatever protection is there, as a person deciding directly does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub seen: Option<SeenProtection>,
}

/// An Environment's protection as a reader saw it: which Environment, whether it was
/// Protected, and in which protection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SeenProtection {
    pub environment: EnvironmentId,
    pub protected: bool,
    pub protections: u32,
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
        "SELECT e.id, e.name, p.name, e.branch_setup, e.protected, e.protections FROM config_environment e \
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
            removal: crate::removal::listed(tx, &id)?,
            merged: merged(tx, who, &id)?,
            branch_setup: match row.optional_text(3)? {
                Some(_) => row.json(3, "Branch setup")?,
                None => Vec::new(),
            },
            protected: row.int(4)? != 0,
            protections: u32::try_from(row.int(5)?).map_err(|_| error::corrupt("Environment"))?,
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

/// Whether `id` is a PR Environment whose pull request merged.
fn merged(tx: &mut dyn Tx, who: &Actor, id: &EnvironmentId) -> Result<bool, RpcError> {
    let Some(pull_request) = crate::pull_request::of(tx, id)? else {
        return Ok(false);
    };
    Ok(
        crate::pull_request::facts(tx, who, pull_request.repository_id, pull_request.number)?
            .is_some_and(|facts| facts.merge_commit.is_some()),
    )
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

pub(crate) fn set_protected(
    tx: &mut dyn Tx,
    who: &Actor,
    set: &SetEnvironmentProtected,
) -> Result<ProtectedView, RpcError> {
    let environment = scope::lock(tx, who, &set.environment)?;
    let summary = &environment.summary;
    if let Some(seen) = &set.seen {
        unchanged(tx, &environment, seen)?;
    }
    // A removal under way never re-reads protection, so protecting what it takes off
    // would promise what it can't keep.
    if set.protected {
        crate::removal::marked(tx, &environment)?;
    }
    let kind = if crate::pull_request::of(tx, &summary.id)?.is_some() {
        Some("a PR Environment")
    } else if !tx
        .query(
            "SELECT parent_id FROM config_environment_branch WHERE environment_id = ?1",
            &[summary.id.as_str().into()],
        )?
        .is_empty()
    {
        Some("a Branch")
    } else {
        None
    };
    if let Some(kind) = kind {
        return Err(error::unsupported(
            format!(
                "{} is {kind}: only a Project's own Environments can be Protected",
                summary.name
            ),
            json!({ "project": summary.project, "environment": summary.name }),
        ));
    }
    tx.execute(
        if set.protected {
            "UPDATE config_environment SET protections = protections + 1 - protected, protected = 1 \
             WHERE id = ?1"
        } else {
            "UPDATE config_environment SET protected = 0 WHERE id = ?1"
        },
        &[summary.id.as_str().into()],
    )?;
    let view = environments(
        tx,
        who,
        &EnvironmentsQuery {
            project: Some(environment.summary.project),
        },
    )?;
    Ok(ProtectedView {
        environment: environment.summary.id,
        view,
    })
}

/// Refuse `conflict` unless `environment` is the one `seen` and its protection is still
/// what was seen: a human's yes, or no ask at all, was decided on that one protection.
fn unchanged(
    tx: &mut dyn Tx,
    environment: &scope::Environment,
    seen: &SeenProtection,
) -> Result<(), RpcError> {
    let summary = &environment.summary;
    if summary.id != seen.environment {
        return Err(error::conflict(
            format!(
                "{} isn't the Environment this was decided on; read it and ask again",
                summary.name
            ),
            json!({ "project": summary.project, "environment": summary.name }),
        ));
    }
    let rows = tx.query(
        "SELECT protected, protections FROM config_environment WHERE id = ?1",
        &[summary.id.as_str().into()],
    )?;
    let row = rows.first().ok_or_else(|| error::corrupt("Environment"))?;
    let now = SeenProtection {
        environment: summary.id.clone(),
        protected: row.int(0)? != 0,
        protections: u32::try_from(row.int(1)?).map_err(|_| error::corrupt("Environment"))?,
    };
    if now == *seen {
        return Ok(());
    }
    Err(error::conflict(
        format!(
            "{}'s protection changed since this was decided; read it and ask again",
            summary.name
        ),
        json!({
            "project": summary.project,
            "environment": summary.name,
            "protected": now.protected,
            "protections": now.protections,
        }),
    ))
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
        "config_sync_receipt",
        "config_removal_mark",
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
