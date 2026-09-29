//! What outlives creation: listing Projects and a Project's Environments,
//! choosing its Default Environment, and removing an Environment, a Project, or
//! an Organization's configuration.
//!
//! Removal has one path. An Environment that ever ran is first removed from the
//! Servers by a removal Deployment (`Admit { remove }`), which ships the empty
//! Environment under the same destructive review, cancellation and retry as any
//! Deployment. Once that applied, or if nothing ever ran, [`RemoveEnvironment`]
//! deletes it from the Store. Neither touches the Default Environment or an
//! Environment with Branches, and nothing branches from one being removed. A
//! Project goes the same way, one Environment at a time, Branches before their
//! Parents and its Default Environment last ([`guard_removal`]); then
//! [`RemoveProject`] deletes it all at once.

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::command::ProjectSummary;
use crate::deployment::{self, DeploymentStatus, DeploymentSummary};
use crate::error;
use crate::id::{EnvironmentId, EnvironmentName, OrganizationId, ProjectId, ProjectName};
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

/// Delete a Project and every Environment of it from the Store. Refused while
/// any of them may still run on the Servers; `details.environment` names the one
/// to take off them next.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RemoveProject {
    pub project: ProjectName,
}

/// The Project a removal deleted, with its Environments in the order they went.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ProjectRemoved {
    pub project: ProjectSummary,
    pub environments: Vec<EnvironmentName>,
}

/// Forget an Organization's configuration once it has no Project: what it
/// created and what Cloud observed of its repositories. Cloud's own Organization
/// removal runs it; it isn't reachable over HTTPS.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RemoveOrganization {}

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
    if let Some(running) = on_servers(tx, id)? {
        return Err(still_on_servers(&environment.summary.name, &running));
    }
    purge(tx, id)?;
    Ok(EnvironmentRemoved {
        environment: environment.summary,
    })
}

/// The Deployment through which `environment` may still run on the Servers: one
/// in flight, or the last that ran unless it was a removal that applied. None
/// when nothing ever ran or its removal applied.
fn on_servers(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<DeploymentSummary>, RpcError> {
    let history = deployment::history(tx, environment, i64::MAX)?;
    if let Some(running) = history
        .iter()
        .find(|deployment| deployment.status.in_flight())
    {
        return Ok(Some(running.clone()));
    }
    Ok(history
        .into_iter()
        .find(|deployment| deployment.runner.is_some())
        .filter(|ran| !(ran.remove && ran.status == DeploymentStatus::Applied)))
}

/// Refuse to delete `name` while `deployment` may run it: `details.deployed` when
/// a removal Deployment is what it needs next.
fn still_on_servers(name: &EnvironmentName, deployment: &DeploymentSummary) -> RpcError {
    if deployment.status.in_flight() {
        return error::conflict(
            format!(
                "Deployment #{} of {name} hasn't ended: wait for it or cancel it first",
                deployment.number
            ),
            json!({ "deployment": deployment.id, "environment": name }),
        );
    }
    error::conflict(
        format!("{name} may still run on the Servers: remove it from them first"),
        json!({ "deployed": true, "deployment": deployment.id, "environment": name }),
    )
}

/// Refuse to take an Environment off the Servers while a Branch of it may still
/// run there, using it live; or, for the Default Environment, while any other
/// Environment of its Project but its Parents may. So the Default comes off last,
/// when its whole Project goes.
pub(crate) fn guard_removal(tx: &mut dyn Tx, environment: &Environment) -> Result<(), RpcError> {
    let summary = &environment.summary;
    let project = project_of(tx, &summary.id)?;
    let members = members(tx, &project.id)?;
    let branches = members
        .iter()
        .filter(|member| member.parent.as_ref() == Some(&summary.id));
    let running = still_running(tx, branches)?;
    if let Some(first) = running.first() {
        return Err(error::conflict(
            format!(
                "{} has Branches that may still run ({}). Remove them first",
                summary.name,
                running.join(", ")
            ),
            json!({
                "branches": running,
                "next": format!("ployz env rm {first} --confirm {first} --project {}", summary.project),
            }),
        ));
    }
    if summary.id != project.default_environment {
        return Ok(());
    }
    let parents = ancestors(&members, &summary.id);
    let others = members
        .iter()
        .filter(|member| member.id != summary.id && !parents.contains(&member.id));
    let running = still_running(tx, others)?;
    if !running.is_empty() {
        return Err(error::conflict(
            format!(
                "{} is the Default Environment: it comes off the Servers only after the rest of \
                 Project {} ({})",
                summary.name,
                summary.project,
                running.join(", ")
            ),
            json!({
                "environments": running,
                "next": format!("ployz env default ENV --project {}", summary.project),
            }),
        ));
    }
    Ok(())
}

/// The names of `members` that may still run on the Servers.
fn still_running<'a>(
    tx: &mut dyn Tx,
    members: impl Iterator<Item = &'a Member>,
) -> Result<Vec<String>, RpcError> {
    let mut running = Vec::new();
    for member in members {
        if on_servers(tx, &member.id)?.is_some() {
            running.push(member.name.to_string());
        }
    }
    Ok(running)
}

/// One Environment of a Project, as teardown orders them.
struct Member {
    id: EnvironmentId,
    name: EnvironmentName,
    parent: Option<EnvironmentId>,
}

fn project_of(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<scope::Project, RpcError> {
    let rows = tx.query(
        "SELECT p.id, p.name, p.default_environment_id FROM config_project p \
         JOIN config_environment e ON e.project_id = p.id WHERE e.id = ?1",
        &[environment.as_str().into()],
    )?;
    let row = rows.first().ok_or_else(|| error::corrupt("Environment"))?;
    let corrupt = |_| error::corrupt("Project");
    Ok(scope::Project {
        id: ProjectId::parse(row.text(0)?).map_err(corrupt)?,
        name: ProjectName::parse(row.text(1)?).map_err(corrupt)?,
        default_environment: EnvironmentId::parse(row.text(2)?).map_err(corrupt)?,
    })
}

/// A Project's Environments with their Parents, in ID order.
fn members(tx: &mut dyn Tx, project: &ProjectId) -> Result<Vec<Member>, RpcError> {
    let rows = tx.query(
        "SELECT e.id, e.name, COALESCE(b.parent_id, '') FROM config_environment e \
         LEFT JOIN config_environment_branch b ON b.environment_id = e.id \
         WHERE e.project_id = ?1 ORDER BY e.id",
        &[project.as_str().into()],
    )?;
    rows.iter()
        .map(|row| {
            let corrupt = |_| error::corrupt("Environment");
            Ok(Member {
                id: EnvironmentId::parse(row.text(0)?).map_err(corrupt)?,
                name: EnvironmentName::parse(row.text(1)?).map_err(corrupt)?,
                parent: match row.text(2)? {
                    "" => None,
                    parent => Some(EnvironmentId::parse(parent).map_err(corrupt)?),
                },
            })
        })
        .collect()
}

/// The Parent of `environment`, its Parent's Parent, and so on.
fn ancestors(members: &[Member], environment: &EnvironmentId) -> Vec<EnvironmentId> {
    let mut chain = Vec::new();
    let mut at = environment;
    while let Some(parent) = members
        .iter()
        .find(|member| &member.id == at)
        .and_then(|member| member.parent.as_ref())
        .filter(|parent| !chain.contains(*parent))
    {
        chain.push(parent.clone());
        at = parent;
    }
    chain
}

/// The order a Project's Environments come off the Servers and out of the Store:
/// Branches before their Parents, and the Default Environment after every other
/// but its own Parents, as [`guard_removal`] requires.
fn teardown_order<'a>(members: &'a [Member], default: &EnvironmentId) -> Vec<&'a Member> {
    let parents = ancestors(members, default);
    let mut order: Vec<&Member> = members.iter().collect();
    order.sort_by_key(|member| {
        let class = if &member.id == default {
            1
        } else if parents.contains(&member.id) {
            2
        } else {
            0
        };
        (
            class,
            std::cmp::Reverse(ancestors(members, &member.id).len()),
        )
    });
    order
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
fn purge(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<(), RpcError> {
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
        let corrupt = |_| error::corrupt("Project");
        let id = ProjectId::parse(row.text(0)?).map_err(corrupt)?;
        let environment = EnvironmentName::parse(row.text(3)?).map_err(corrupt)?;
        match projects.last_mut() {
            Some(project) if project.id == id => project.environments.push(environment),
            _ => projects.push(ProjectListing {
                id,
                name: ProjectName::parse(row.text(1)?).map_err(corrupt)?,
                default_environment: EnvironmentName::parse(row.text(2)?).map_err(corrupt)?,
                environments: vec![environment],
            }),
        }
    }
    Ok(ProjectsView { projects })
}

pub(crate) fn remove_project(
    tx: &mut dyn Tx,
    who: &Actor,
    remove: &RemoveProject,
) -> Result<ProjectRemoved, RpcError> {
    let project = scope::project(tx, who, Some(&remove.project))?;
    let members = members(tx, &project.id)?;
    // Lock every Environment, in ID order, against a concurrent admission.
    for member in &members {
        scope::lock_id(tx, who, &member.id)?;
    }
    let order = teardown_order(&members, &project.default_environment);
    for member in &order {
        if let Some(running) = on_servers(tx, &member.id)? {
            return Err(still_on_servers(&member.name, &running));
        }
    }
    for member in &order {
        purge(tx, &member.id)?;
    }
    tx.execute(
        "DELETE FROM config_project WHERE id = ?1",
        &[project.id.as_str().into()],
    )?;
    Ok(ProjectRemoved {
        project: ProjectSummary {
            id: project.id,
            name: project.name,
        },
        environments: order
            .into_iter()
            .map(|member| member.name.clone())
            .collect(),
    })
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
                "next": format!("ployz project rm {} --confirm {}", first.name, first.name),
            }),
        ));
    }
    for table in ["config_create", "config_branch", "config_check_suite"] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE organization_id = ?1"),
            &[who.organization.as_str().into()],
        )?;
    }
    Ok(OrganizationRemoved {
        organization: who.organization.clone(),
    })
}
