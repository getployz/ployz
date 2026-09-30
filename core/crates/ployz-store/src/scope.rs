//! Resolving which Project and Environment a request means, and loading and saving
//! an Environment's Working State document.

use std::collections::BTreeMap;

use ployz_core::config::{
    EnvironmentNodeType, SavedEnvironmentIntent, SavedServiceIntent, SavedVolumeIntent,
    parse_environment_intent,
};
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::error;
use crate::id::{EnvironmentId, EnvironmentName, ProjectId, ProjectName, Revision, VolumeName};
use crate::storage::Tx;

/// Which Environment a request addresses. An omitted Project means the
/// Organization's only Project; an omitted Environment means the Project's
/// Default Environment.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
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
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
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
    /// The name of each node a Branch uses live, by lineage: its variables
    /// reference them by these names.
    pub(crate) live: BTreeMap<String, String>,
}

impl Environment {
    /// Every Service name a variable may reference, by lineage: its own Services
    /// and the nodes it uses live.
    pub(crate) fn names(&self) -> BTreeMap<String, String> {
        let mut names = self.live.clone();
        names.extend(
            self.working
                .services
                .iter()
                .map(|service| (service.lineage_id.clone(), service.slug.clone())),
        );
        names
    }

    /// Refuse with `conflict` unless Working State is still at `expect`.
    pub(crate) fn expect(&self, expect: Option<Revision>) -> Result<(), RpcError> {
        match expect {
            Some(expect) if expect != self.summary.revision => Err(error::conflict(
                format!(
                    "Working State moved from revision {expect} to {}",
                    self.summary.revision
                ),
                json!({ "revision": self.summary.revision }),
            )),
            _ => Ok(()),
        }
    }

    /// The Service named `name` in Working State.
    pub(crate) fn service(&self, name: &ServiceName) -> Result<&SavedServiceIntent, RpcError> {
        let services = &self.working.services;
        services
            .iter()
            .find(|service| service.slug == name.as_str())
            .ok_or_else(|| no_service(name, &self.summary.name, &self.working))
    }

    /// The Service named `name` in Working State, to change.
    pub(crate) fn service_mut(
        &mut self,
        name: &ServiceName,
    ) -> Result<&mut SavedServiceIntent, RpcError> {
        let index = self
            .working
            .services
            .iter()
            .position(|service| service.slug == name.as_str())
            .ok_or_else(|| no_service(name, &self.summary.name, &self.working))?;
        Ok(self
            .working
            .services
            .get_mut(index)
            .expect("position is in bounds"))
    }
}

impl Environment {
    /// The Volume named `name` in Working State, to change.
    pub(crate) fn volume_mut(
        &mut self,
        name: &VolumeName,
    ) -> Result<&mut SavedVolumeIntent, RpcError> {
        let index = self
            .working
            .volumes
            .iter()
            .position(|volume| volume.name == name.as_str())
            .ok_or_else(|| {
                self.volume(name)
                    .err()
                    .unwrap_or_else(|| error::corrupt("Volume"))
            })?;
        Ok(self
            .working
            .volumes
            .get_mut(index)
            .expect("position is in bounds"))
    }

    /// The Volume named `name` in Working State.
    pub(crate) fn volume(&self, name: &VolumeName) -> Result<&SavedVolumeIntent, RpcError> {
        self.working
            .volumes
            .iter()
            .find(|volume| volume.name == name.as_str())
            .ok_or_else(|| {
                let names = self
                    .working
                    .volumes
                    .iter()
                    .map(|volume| volume.name.as_str())
                    .collect::<Vec<_>>();
                error::choices(
                    format!(
                        "No Volume named {name} in Environment {}",
                        self.summary.name
                    ),
                    name.as_str(),
                    names.iter().copied(),
                )
            })
    }
}

/// `service` names no Service in Working State: list the ones it could mean.
pub(crate) fn no_service(
    service: &ServiceName,
    environment: &EnvironmentName,
    working: &SavedEnvironmentIntent,
) -> RpcError {
    let names = working
        .services
        .iter()
        .map(|service| service.slug.as_str())
        .collect::<Vec<_>>();
    error::choices(
        format!("No Service named {service} in Environment {environment}"),
        service.as_str(),
        names.iter().copied(),
    )
}

/// One node of an Environment, as its own rows store it beside the Environment.
#[derive(Clone, Copy)]
pub(crate) enum Node<'a> {
    Service(&'a SavedServiceIntent),
    Volume(&'a SavedVolumeIntent),
}

impl Node<'_> {
    pub(crate) const fn node_type(self) -> &'static str {
        match self {
            Self::Service(_) => "service",
            Self::Volume(_) => "volume",
        }
    }

    pub(crate) fn document(self) -> String {
        match self {
            Self::Service(service) => serde_json::to_string(service),
            Self::Volume(volume) => serde_json::to_string(volume),
        }
        .expect("a node is JSON")
    }
}

/// Capture `node`'s Node Introduction in `environment`: it never changes after.
pub(crate) fn introduce(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    node: Node<'_>,
) -> Result<(), RpcError> {
    let id = match node {
        Node::Service(service) => service.id.as_str(),
        Node::Volume(volume) => volume.resource_id.as_str(),
    };
    tx.execute(
        "INSERT INTO config_node_introduction \
         (environment_id, node_id, organization_id, node_type, node) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        &[
            environment.as_str().into(),
            id.into(),
            who.organization.as_str().into(),
            node.node_type().into(),
            node.document().as_str().into(),
        ],
    )?;
    Ok(())
}

/// Rows of `(node, node_type)`, as one document shaped like `like`: Node
/// Introductions, or Applied State (`what`, when one is unreadable).
pub(crate) fn nodes(
    rows: &[crate::storage::Row],
    like: &SavedEnvironmentIntent,
    what: &str,
) -> Result<SavedEnvironmentIntent, RpcError> {
    let mut intent = crate::review::empty(&like.environment_slug);
    let corrupt = |_| error::corrupt(what);
    for row in rows {
        let node_type: EnvironmentNodeType =
            serde_json::from_value(json!(row.text(1)?)).map_err(corrupt)?;
        let node = row.text(0)?;
        match node_type {
            EnvironmentNodeType::Service => {
                intent
                    .services
                    .push(serde_json::from_str(node).map_err(corrupt)?);
            }
            EnvironmentNodeType::Volume => {
                intent
                    .volumes
                    .push(serde_json::from_str(node).map_err(corrupt)?);
            }
        }
    }
    Ok(intent)
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
            id: row.parse::<ProjectId>(0, "identity")?,
            name: row.parse::<ProjectName>(1, "identity")?,
            default_environment: row.parse::<EnvironmentId>(2, "identity")?,
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

/// The Project Environment `id` belongs to.
pub(crate) fn project_of(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<Project, RpcError> {
    let rows = tx.query(
        "SELECT p.id, p.name, p.default_environment_id FROM config_project p \
         JOIN config_environment e ON e.project_id = p.id WHERE e.id = ?1",
        &[id.as_str().into()],
    )?;
    let row = rows.first().ok_or_else(|| error::corrupt("Environment"))?;
    Ok(Project {
        id: row.parse(0, "Project")?,
        name: row.parse(1, "Project")?,
        default_environment: row.parse(2, "Project")?,
    })
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
    lock_all(tx, [id.clone()])?;
    load(tx, project, &id)
}

/// Lock Environments `ids` for a write that touches several, strictly in ID order,
/// so writers that share any of them wait in the same order and never deadlock.
/// Locking one again later in the same transaction changes nothing.
pub(crate) fn lock_all(
    tx: &mut dyn Tx,
    ids: impl IntoIterator<Item = EnvironmentId>,
) -> Result<(), RpcError> {
    let ids: std::collections::BTreeSet<EnvironmentId> = ids.into_iter().collect();
    for id in ids {
        tx.execute(
            "UPDATE config_environment SET working_revision = working_revision WHERE id = ?1",
            &[id.as_str().into()],
        )?;
    }
    Ok(())
}

/// Lock and load Environment `id` of `who`'s Organization, as [`lock`] does.
pub(crate) fn lock_id(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &EnvironmentId,
) -> Result<Environment, RpcError> {
    lock_all(tx, [id.clone()])?;
    owned(tx, who, id)
}

/// Load Environments `a` and `b` of `who`'s Organization, in that order; with
/// `lock`, lock both first, in ID order.
pub(crate) fn load_pair(
    tx: &mut dyn Tx,
    who: &Actor,
    (a, b): (&EnvironmentId, &EnvironmentId),
    lock: bool,
) -> Result<(Environment, Environment), RpcError> {
    if lock {
        lock_all(tx, [a.clone(), b.clone()])?;
    }
    Ok((owned(tx, who, a)?, owned(tx, who, b)?))
}

/// Load Environment `id` of `who`'s Organization without locking it.
fn owned(tx: &mut dyn Tx, who: &Actor, id: &EnvironmentId) -> Result<Environment, RpcError> {
    let rows = tx.query(
        "SELECT p.name FROM config_environment e JOIN config_project p ON p.id = e.project_id \
         WHERE e.id = ?1 AND e.organization_id = ?2",
        &[id.as_str().into(), who.organization.as_str().into()],
    )?;
    let project = rows
        .first()
        .ok_or_else(|| error::not_found("No such Environment", json!({})))?
        .parse::<ProjectName>(0, "identity")?;
    load(tx, project, id)
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
    Ok((project.name, row.parse::<EnvironmentId>(0, "identity")?))
}

/// Load an Environment by its ID, without locking it.
pub(crate) fn load_by_id(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<Environment, RpcError> {
    let rows = tx.query(
        "SELECT p.name FROM config_environment e \
         JOIN config_project p ON p.id = e.project_id WHERE e.id = ?1",
        &[id.as_str().into()],
    )?;
    let project = rows
        .first()
        .ok_or_else(|| error::corrupt("Environment"))?
        .parse::<ProjectName>(0, "identity")?;
    load(tx, project, id)
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
    let working = row.intent(2, "Working State")?;
    let live = crate::branch::live_names(tx, id, &working)?;
    Ok(Environment {
        summary: EnvironmentSummary {
            id: id.clone(),
            project,
            name: row.parse::<EnvironmentName>(0, "identity")?,
            revision: Revision(row.number(1, "revision")?),
        },
        working,
        live,
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
