//! An Environment's lifecycle past creation: listing a Project's Environments,
//! choosing its Default Environment, and removing one.
//!
//! Removal has one path. An Environment that ever ran is first removed from the
//! Servers by a removal Deployment (`Admit { remove }`), which ships the empty
//! Environment under the same destructive review, cancellation and retry as any
//! Deployment. Once that applied, or if nothing ever ran, [`RemoveEnvironment`]
//! deletes it from the Store. Neither touches the Default Environment or an
//! Environment with Branches, and nothing branches from one being removed.

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::command::ProjectSummary;
use crate::deployment::{self, DeploymentStatus, DeploymentSummary};
use crate::error;
use crate::id::{EnvironmentId, EnvironmentName, ProjectName};
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary};
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
}

/// Make an Environment its Project's Default Environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SetDefaultEnvironment {
    pub environment: EnvironmentRef,
}

/// Delete an Environment from the Store: its Working and Saved State, Deployments
/// and credentials. Refused while anything of it may run on the Servers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RemoveEnvironment {
    pub environment: EnvironmentRef,
}

/// The Environment a removal deleted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct EnvironmentRemoved {
    pub environment: EnvironmentSummary,
}

pub(crate) fn environments(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &EnvironmentsQuery,
) -> Result<EnvironmentsView, RpcError> {
    let project = scope::project(tx, who, query.project.as_ref())?;
    let rows = tx.query(
        "SELECT e.id, e.name, COALESCE(p.name, '') FROM config_environment e \
         LEFT JOIN config_environment_branch b ON b.environment_id = e.id \
         LEFT JOIN config_environment p ON p.id = b.parent_id \
         WHERE e.project_id = ?1 ORDER BY e.name",
        &[project.id.as_str().into()],
    )?;
    let mut environments = Vec::with_capacity(rows.len());
    for row in rows {
        let corrupt = |_| error::corrupt("Environment");
        let id = EnvironmentId::parse(row.text(0)?).map_err(corrupt)?;
        let parent = match row.text(2)? {
            "" => None,
            name => Some(EnvironmentName::parse(name).map_err(corrupt)?),
        };
        environments.push(EnvironmentListing {
            default: id == project.default_environment,
            name: EnvironmentName::parse(row.text(1)?).map_err(corrupt)?,
            parent,
            removal: removal(tx, &id)?,
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
    if let Some(removal) = removing(tx, &environment.summary.id)? {
        return Err(being_removed(&environment, &removal));
    }
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

pub(crate) fn remove(
    tx: &mut dyn Tx,
    who: &Actor,
    remove: &RemoveEnvironment,
) -> Result<EnvironmentRemoved, RpcError> {
    let environment = scope::lock(tx, who, &remove.environment)?;
    guard(tx, &environment)?;
    let id = &environment.summary.id;
    let history = deployment::history(tx, id, i64::MAX)?;
    if let Some(running) = history
        .iter()
        .find(|deployment| deployment.status.in_flight())
    {
        return Err(error::conflict(
            format!(
                "Deployment #{} of {} hasn't ended: wait for it or cancel it first",
                running.number, environment.summary.name
            ),
            json!({ "deployment": running.id }),
        ));
    }
    // What ran last decides what the Servers may hold: nothing, if nothing ever ran
    // or the last run was a removal that applied.
    let ran = history
        .iter()
        .find(|deployment| deployment.runner.is_some());
    if let Some(ran) = ran
        && !(ran.remove && ran.status == DeploymentStatus::Applied)
    {
        return Err(error::conflict(
            format!(
                "{} may still run on the Servers: remove it from them first",
                environment.summary.name
            ),
            json!({ "deployed": true, "deployment": ran.id }),
        ));
    }
    purge(tx, id)?;
    Ok(EnvironmentRemoved {
        environment: environment.summary,
    })
}

/// Refuse to remove the Default Environment, or one that has Branches: they use
/// it live and update from it.
pub(crate) fn guard(tx: &mut dyn Tx, environment: &Environment) -> Result<(), RpcError> {
    let summary = &environment.summary;
    let default = tx.query(
        "SELECT name FROM config_project WHERE default_environment_id = ?1",
        &[summary.id.as_str().into()],
    )?;
    if !default.is_empty() {
        return Err(error::conflict(
            format!(
                "{} is the Default Environment. Choose another Default Environment first",
                summary.name
            ),
            json!({ "next": format!("ployz env default ENV --project {}", summary.project) }),
        ));
    }
    let branches = tx.query(
        "SELECT e.name FROM config_environment_branch b JOIN config_environment e ON e.id = b.environment_id \
         WHERE b.parent_id = ?1 ORDER BY e.name",
        &[summary.id.as_str().into()],
    )?;
    if let Some(first) = branches.first() {
        let names = branches
            .iter()
            .map(|row| row.text(0).map(str::to_owned))
            .collect::<Result<Vec<_>, _>>()?;
        return Err(error::conflict(
            format!(
                "{} has Branches ({}). Remove them first",
                summary.name,
                names.join(", ")
            ),
            json!({
                "branches": names,
                "next": format!("ployz env rm {} --project {}", first.text(0)?, summary.project),
            }),
        ));
    }
    Ok(())
}

/// The removal `environment` is going through: its latest Deployment, when that
/// removes it and may have or may yet run.
pub(crate) fn removing(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<DeploymentSummary>, RpcError> {
    Ok(removal(tx, environment)?.filter(|removal| {
        !matches!(
            removal.status,
            DeploymentStatus::Failed | DeploymentStatus::Cancelled
        )
    }))
}

/// Refuse to build on an Environment being removed.
pub(crate) fn being_removed(environment: &Environment, removal: &DeploymentSummary) -> RpcError {
    error::conflict(
        format!(
            "{} is being removed (Deployment #{}, {})",
            environment.summary.name,
            removal.number,
            serde_json::to_value(removal.status)
                .ok()
                .and_then(|status| status.as_str().map(str::to_owned))
                .unwrap_or_default()
        ),
        json!({ "deployment": removal.id }),
    )
}

fn removal(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<DeploymentSummary>, RpcError> {
    Ok(deployment::history(tx, environment, 1)?
        .into_iter()
        .next()
        .filter(|latest| latest.remove))
}

/// Delete every row of `environment`, children before what they reference.
pub(crate) fn purge(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<(), RpcError> {
    tx.execute(
        "DELETE FROM config_build WHERE deployment_id IN \
         (SELECT id FROM config_deployment WHERE environment_id = ?1)",
        &[environment.as_str().into()],
    )?;
    for table in [
        "config_applied",
        "config_deployment",
        "config_namespace",
        "config_saved",
        "config_node_introduction",
        "config_registry_credential",
        "config_build_receipt",
        "config_service_policy",
        "config_waiting_deploy",
        "config_pr_environment",
        "config_environment_branch",
    ] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE environment_id = ?1"),
            &[environment.as_str().into()],
        )?;
    }
    tx.execute(
        "DELETE FROM config_environment WHERE id = ?1",
        &[environment.as_str().into()],
    )?;
    Ok(())
}
