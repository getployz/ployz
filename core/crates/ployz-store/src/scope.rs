//! Resolving which Project and Environment a request means, and loading and saving
//! an Environment's Working State document.

use ployz_core::config::{SavedEnvironmentIntent, parse_environment_intent};
use ployz_core::{Namespace, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::Actor;
use crate::error;
use crate::id::{EnvironmentId, EnvironmentName, ProjectId, ProjectName, Revision};
use crate::storage::{Row, Tx};

/// Which Environment a request addresses. An omitted Project means the
/// Organization's only Project; an omitted Environment means the Project's
/// Default Environment.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentRef {
    #[serde(default)]
    pub project: Option<ProjectName>,
    #[serde(default)]
    pub environment: Option<EnvironmentName>,
}

/// An Environment as every result names it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentSummary {
    pub id: EnvironmentId,
    pub project: ProjectName,
    pub name: EnvironmentName,
    pub namespace: Namespace,
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
            None => error::not_found("This Organization has no Project yet", json!({})),
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
    let project = project(tx, who, at.project.as_ref())?;
    let columns = "SELECT id, name, namespace, working_revision, working FROM config_environment";
    let rows = match &at.environment {
        Some(name) => tx.query(
            &format!("{columns} WHERE project_id = ?1 AND name = ?2"),
            &[project.id.as_str().into(), name.as_str().into()],
        )?,
        None => tx.query(
            &format!("{columns} WHERE id = ?1"),
            &[project.default_environment.as_str().into()],
        )?,
    };
    let Some(row) = rows.first() else {
        let name = at
            .environment
            .as_ref()
            .map_or("default", EnvironmentName::as_str);
        return Err(error::not_found(
            format!("No Environment named {name} in Project {}", project.name),
            json!({}),
        ));
    };
    loaded(project.name, row)
}

/// Resolve, lock and load an Environment for a write. Concurrent writers to the same
/// Environment wait here until this transaction ends, so they apply in turn to the
/// latest Working State. The lock is a row update, portable to every adapter.
pub(crate) fn lock(
    tx: &mut dyn Tx,
    who: &Actor,
    at: &EnvironmentRef,
) -> Result<Environment, RpcError> {
    let found = environment(tx, who, at)?;
    tx.execute(
        "UPDATE config_environment SET working_revision = working_revision WHERE id = ?1",
        &[found.summary.id.as_str().into()],
    )?;
    // Reload: under Postgres another writer may have committed while this one waited.
    let rows = tx.query(
        "SELECT id, name, namespace, working_revision, working FROM config_environment WHERE id = ?1",
        &[found.summary.id.as_str().into()],
    )?;
    let row = rows.first().ok_or_else(|| error::corrupt("Environment"))?;
    loaded(found.summary.project, row)
}

fn loaded(project: ProjectName, row: &Row) -> Result<Environment, RpcError> {
    let working = serde_json::from_str(row.text(4)?)
        .ok()
        .and_then(|value| parse_environment_intent(value).ok())
        .ok_or_else(|| error::corrupt("Working State"))?;
    Ok(Environment {
        summary: EnvironmentSummary {
            id: stored(EnvironmentId::parse(row.text(0)?))?,
            project,
            name: stored(EnvironmentName::parse(row.text(1)?))?,
            namespace: Namespace::parse(row.text(2)?).map_err(|_| error::corrupt("Namespace"))?,
            revision: Revision(u64::try_from(row.int(3)?).map_err(|_| error::corrupt("revision"))?),
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
    environment.summary.revision = Revision(environment.summary.revision.0 + 1);
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
