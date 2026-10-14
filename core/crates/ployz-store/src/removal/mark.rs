//! The removal mark: the row `remove` writes, and `due`, the sweep and every
//! write that would build on its target read.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{DockerVolumeId, MachineId, RpcError};
use serde::{Deserialize, Serialize};

use super::{Residual, Target};
use crate::Actor;
use crate::error;
use crate::id::{DeploymentId, EnvironmentId, ProjectId, RemovalId, VolumeName};
use crate::storage::{Row, Tx};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum State {
    Removing,
    /// A tombstone: idle close and a repeated PR close leave its target be.
    Cancelled,
    Removed,
}

/// What a member's removal may delete, as its caller accepted it. Stored only,
/// never on the wire: `Everything` is the Store's own close.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Accepted {
    /// Exactly these Docker Volumes, each on its Server.
    Confirmed(BTreeSet<DockerVolumeId>),
    /// Every copy of these Volumes, wherever they turn up, unanswered Servers too.
    EveryCopy(BTreeSet<VolumeName>),
    Everything {
        because: Closed,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Closed {
    PullRequest,
    Idle,
    /// The Branch synced into its Parent and is not kept.
    Synced,
}

/// What Cloud saw when it last admitted this member: a re-admission needs
/// something to differ.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Evidence {
    pub(crate) deployment: DeploymentId,
    pub(crate) servers: Option<u32>,
    pub(crate) unanswered: BTreeSet<MachineId>,
}

#[derive(Clone, Debug)]
pub(crate) struct Mark {
    pub(crate) id: RemovalId,
    pub(crate) target: Target,
    pub(crate) target_name: String,
    pub(crate) by: String,
    pub(crate) at: i64,
    pub(crate) retried: i64,
    pub(crate) state: State,
    pub(crate) accepted: BTreeMap<EnvironmentId, Accepted>,
    pub(crate) members: Vec<EnvironmentId>,
    pub(crate) evidence: BTreeMap<EnvironmentId, Evidence>,
    pub(crate) residuals: Vec<Residual>,
}

impl Mark {
    pub(crate) fn new(
        who: &Actor,
        (target, target_name): (Target, String),
        members: Vec<EnvironmentId>,
        accepted: BTreeMap<EnvironmentId, Accepted>,
    ) -> Self {
        let at = crate::deployment::now();
        Self {
            id: RemovalId::parse(uuid::Uuid::new_v4().to_string())
                .expect("a new UUID is a removal ID"),
            target,
            target_name,
            by: who
                .principal
                .as_ref()
                .map_or_else(|| "system".to_owned(), |principal| principal.to_string()),
            at,
            retried: at,
            state: State::Removing,
            accepted,
            members,
            evidence: BTreeMap::new(),
            residuals: Vec::new(),
        }
    }
}

const COLUMNS: &str = "id, environment_id, project_id, target_name, by, at, retried, state, \
     accepted, members, evidence, residuals";

fn read(row: &Row) -> Result<Mark, RpcError> {
    let target = match (
        row.parse_optional::<EnvironmentId>(1, "Removal")?,
        row.parse_optional::<ProjectId>(2, "Removal")?,
    ) {
        (Some(environment), None) => Target::Environment(environment),
        (None, Some(project)) => Target::Project(project),
        _ => return Err(error::corrupt("Removal target")),
    };
    Ok(Mark {
        id: row.parse(0, "Removal")?,
        target,
        target_name: row.text(3)?.to_owned(),
        by: row.text(4)?.to_owned(),
        at: row.int(5)?,
        retried: row.int(6)?,
        state: serde_json::from_value(serde_json::Value::String(row.text(7)?.to_owned()))
            .map_err(|_| error::corrupt("Removal state"))?,
        accepted: row.json(8, "Removal")?,
        members: row.json(9, "Removal")?,
        evidence: row.json(10, "Removal")?,
        residuals: row.json(11, "Removal")?,
    })
}

fn state_text(state: State) -> &'static str {
    match state {
        State::Removing => "removing",
        State::Cancelled => "cancelled",
        State::Removed => "removed",
    }
}

/// `target`'s live mark or its tombstone.
pub(crate) fn of(tx: &mut dyn Tx, target: &Target) -> Result<Option<Mark>, RpcError> {
    let (column, id) = match target {
        Target::Environment(id) => ("environment_id", id.as_str()),
        Target::Project(id) => ("project_id", id.as_str()),
    };
    tx.query(
        &format!(
            "SELECT {COLUMNS} FROM config_removal_mark WHERE {column} = ?1 AND state <> 'removed'"
        ),
        &[id.into()],
    )?
    .first()
    .map(read)
    .transpose()
}

/// The live mark that names `environment`, itself or its Project.
pub(crate) fn naming(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<Mark>, RpcError> {
    tx.query(
        &format!(
            "SELECT {COLUMNS} FROM config_removal_mark WHERE state = 'removing' AND \
             (environment_id = ?1 OR project_id = \
              (SELECT project_id FROM config_environment WHERE id = ?1))"
        ),
        &[environment.as_str().into()],
    )?
    .first()
    .map(read)
    .transpose()
}

/// Every live mark of `who`'s Organization, oldest first.
pub(crate) fn live(tx: &mut dyn Tx, who: &Actor) -> Result<Vec<Mark>, RpcError> {
    tx.query(
        &format!(
            "SELECT {COLUMNS} FROM config_removal_mark \
             WHERE organization_id = ?1 AND state = 'removing' ORDER BY at, id"
        ),
        &[who.organization.as_str().into()],
    )?
    .iter()
    .map(read)
    .collect()
}

/// What `who`'s finished removal `id` left on old servers.
pub(crate) fn residuals(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &RemovalId,
) -> Result<Vec<Residual>, RpcError> {
    tx.query(
        "SELECT residuals FROM config_removal_mark \
         WHERE organization_id = ?1 AND id = ?2 AND state = 'removed'",
        &[who.organization.as_str().into(), id.as_str().into()],
    )?
    .first()
    .ok_or_else(|| error::not_found("No finished removal with that ID", serde_json::json!({})))?
    .json(0, "Removal")
}

pub(crate) fn save(tx: &mut dyn Tx, who: &Actor, mark: &Mark) -> Result<(), RpcError> {
    let (environment, project) = match &mark.target {
        Target::Environment(id) => (Some(id.as_str()), None),
        Target::Project(id) => (None, Some(id.as_str())),
    };
    tx.execute(
        &format!(
            "INSERT INTO config_removal_mark (organization_id, {COLUMNS}) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
             ON CONFLICT (id) DO UPDATE SET retried = excluded.retried, state = excluded.state, \
             accepted = excluded.accepted, evidence = excluded.evidence, \
             residuals = excluded.residuals"
        ),
        &[
            who.organization.as_str().into(),
            mark.id.as_str().into(),
            environment.into(),
            project.into(),
            mark.target_name.as_str().into(),
            mark.by.as_str().into(),
            mark.at.into(),
            mark.retried.into(),
            state_text(mark.state).into(),
            text(&mark.accepted).as_str().into(),
            text(&mark.members).as_str().into(),
            text(&mark.evidence).as_str().into(),
            text(&mark.residuals).as_str().into(),
        ],
    )?;
    Ok(())
}

/// Drop `environment`'s tombstone: a reopen, a push or a new `remove` brings it back.
pub(crate) fn clear_tombstone(tx: &mut dyn Tx, target: &Target) -> Result<(), RpcError> {
    let (column, id) = match target {
        Target::Environment(id) => ("environment_id", id.as_str()),
        Target::Project(id) => ("project_id", id.as_str()),
    };
    tx.execute(
        &format!("DELETE FROM config_removal_mark WHERE {column} = ?1 AND state = 'cancelled'"),
        &[id.into()],
    )?;
    Ok(())
}

fn text(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("stored documents are JSON")
}
