//! Resolving which Project and Environment a request means, and loading and saving
//! an Environment's Working State document.

use ployz_core::config::{SavedEnvironmentIntent, SavedServiceIntent, parse_environment_intent};
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::Actor;
use crate::error;
use crate::id::{EnvironmentId, EnvironmentName, ProjectId, ProjectName, Revision};
use crate::storage::Tx;

/// Which Environment a request addresses. An omitted Project means the
/// Organization's only Project; an omitted Environment means the Project's
/// Default Environment.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentRef {
    /// The Project, by name.
    #[serde(default)]
    pub project: Option<ProjectName>,
    /// The Environment, by name within the Project.
    #[serde(default)]
    pub environment: Option<EnvironmentName>,
}

/// An Environment as every result names it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentSummary {
    /// Its durable identity.
    pub id: EnvironmentId,
    /// The Project it belongs to.
    pub project: ProjectName,
    /// Its name within the Project.
    pub name: EnvironmentName,
    /// Working State's revision after the request.
    pub revision: Revision,
}

pub(crate) struct Project {
    pub(crate) id: ProjectId,
    pub(crate) name: ProjectName,
    pub(crate) default_environment: EnvironmentId,
}

/// An Environment with its Working State, loaded inside one transaction.
pub(crate) struct Environment {
    pub(crate) summary: EnvironmentSummary,
    pub(crate) working: SavedEnvironmentIntent,
}

impl Environment {
    /// The Service named `name` in Working State.
    pub(crate) fn service(&self, name: &ServiceName) -> Result<&SavedServiceIntent, RpcError> {
        let services = &self.working.services;
        services
            .iter()
            .find(|service| service.slug == name.as_str())
            .ok_or_else(|| self.no_service(name))
    }

    /// The Service named `name` in Working State, to change.
    pub(crate) fn service_mut(
        &mut self,
        name: &ServiceName,
    ) -> Result<&mut SavedServiceIntent, RpcError> {
        // Search, then borrow mutably: returning a borrow from a search that can fail
        // would keep `self` borrowed for the error path too.
        self.service(name)?;
        Ok(self
            .working
            .services
            .iter_mut()
            .find(|service| service.slug == name.as_str())
            .expect("found above"))
    }

    fn no_service(&self, name: &ServiceName) -> RpcError {
        let services = &self.working.services;
        error::not_found(
            format!(
                "No Service named {name} in Environment {}",
                self.summary.name
            ),
            json!({ "services": services.iter().map(|service| &service.slug).collect::<Vec<_>>() }),
        )
    }
}

pub(crate) fn project(
    tx: &mut dyn Tx,
    who: &Actor,
    name: Option<&ProjectName>,
) -> Result<Project, RpcError> {
    let organization = who.organization.as_str();
    let rows = match name {
        Some(name) => tx.query(
            "SELECT id, name, default_environment_id FROM config_project \
             WHERE organization_id = ?1 AND name = ?2",
            &[organization.into(), name.as_str().into()],
        )?,
        None => tx.query(
            "SELECT id, name, default_environment_id FROM config_project \
             WHERE organization_id = ?1 ORDER BY name",
            &[organization.into()],
        )?,
    };
    match rows.as_slice() {
        [row] => Ok(Project {
            id: stored(ProjectId::parse(row.text(0)?))?,
            name: stored(ProjectName::parse(row.text(1)?))?,
            default_environment: stored(EnvironmentId::parse(row.text(2)?))?,
        }),
        [] => Err(match name {
            Some(name) => error::not_found(format!("No Project named {name}"), json!({})),
            None => error::not_found(
                "This Organization has no Project yet",
                json!({ "next": "ployz project new NAME" }),
            ),
        }),
        rows => Err(error::ambiguous(
            "Name a Project: this Organization has more than one",
            json!({ "projects": rows.iter().map(|row| row.text(1)).collect::<Result<Vec<_>, _>>()? }),
        )),
    }
}

/// Resolve and load an Environment without locking it.
pub(crate) fn environment(
    tx: &mut dyn Tx,
    who: &Actor,
    at: &EnvironmentRef,
) -> Result<Environment, RpcError> {
    let (project, id) = resolve(tx, who, at)?;
    load(tx, project, &id)
}

/// Resolve, lock and load an Environment for a write. Concurrent writers to the same
/// Environment wait here until this transaction ends, so they apply in turn to the
/// latest Working State. The lock is a row update, portable to every adapter, taken
/// before the load so that under Postgres the load sees any writer that committed first.
pub(crate) fn lock(
    tx: &mut dyn Tx,
    who: &Actor,
    at: &EnvironmentRef,
) -> Result<Environment, RpcError> {
    let (project, id) = resolve(tx, who, at)?;
    tx.execute(
        "UPDATE config_environment SET working_revision = working_revision WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    load(tx, project, &id)
}

/// Which Environment `at` names, without reading its Working State.
fn resolve(
    tx: &mut dyn Tx,
    who: &Actor,
    at: &EnvironmentRef,
) -> Result<(ProjectName, EnvironmentId), RpcError> {
    let project = project(tx, who, at.project.as_ref())?;
    let Some(name) = &at.environment else {
        return Ok((project.name, project.default_environment));
    };
    let rows = tx.query(
        "SELECT id FROM config_environment WHERE project_id = ?1 AND name = ?2",
        &[project.id.as_str().into(), name.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Err(error::not_found(
            format!("No Environment named {name} in Project {}", project.name),
            json!({ "next": format!("ployz env new {name} --project {}", project.name) }),
        ));
    };
    Ok((project.name, stored(EnvironmentId::parse(row.text(0)?))?))
}

fn load(
    tx: &mut dyn Tx,
    project: ProjectName,
    id: &EnvironmentId,
) -> Result<Environment, RpcError> {
    let rows = tx.query(
        "SELECT name, working_revision, working FROM config_environment WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    let row = rows.first().ok_or_else(|| error::corrupt("Environment"))?;
    let working = serde_json::from_str(row.text(2)?)
        .ok()
        .and_then(|value| parse_environment_intent(value).ok())
        .ok_or_else(|| error::corrupt("Working State"))?;
    Ok(Environment {
        summary: EnvironmentSummary {
            id: id.clone(),
            project,
            name: stored(EnvironmentName::parse(row.text(0)?))?,
            revision: Revision(u64::try_from(row.int(1)?).map_err(|_| error::corrupt("revision"))?),
        },
        working,
    })
}

/// Persist changed Working State as the next revision. The document is validated
/// whole first, so the Store never holds one it cannot read back.
pub(crate) fn save_working(tx: &mut dyn Tx, environment: &mut Environment) -> Result<(), RpcError> {
    let document = serde_json::to_value(&environment.working).expect("Working State is JSON");
    environment.working = parse_environment_intent(document)
        .map_err(|error| error::invalid(error.message, json!({ "path": error.path })))?;
    environment.summary.revision = environment.summary.revision.next();
    tx.execute(
        "UPDATE config_environment SET working_revision = ?1, working = ?2 WHERE id = ?3",
        &[
            revision_param(environment.summary.revision)?.into(),
            serde_json::to_string(&environment.working)
                .expect("Working State is JSON")
                .as_str()
                .into(),
            environment.summary.id.as_str().into(),
        ],
    )?;
    Ok(())
}

pub(crate) fn revision_param(revision: Revision) -> Result<i64, RpcError> {
    i64::try_from(revision.0).map_err(|_| error::internal("Working State revision overflowed"))
}

fn stored<T>(value: Result<T, RpcError>) -> Result<T, RpcError> {
    value.map_err(|_| error::corrupt("identity"))
}
