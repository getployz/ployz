//! The removal mark: the row `remove` writes, and `due`, the sweep and every
//! write that would build on its target read.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::authority::TeardownApproval;
use ployz_core::{DockerVolumeId, MachineId, RpcError};
use serde::{Deserialize, Serialize};

use super::{Residual, Target};
use crate::Actor;
use crate::error;
use crate::id::{DeploymentId, EnvironmentId, ProjectId, RemovalId, VolumeName};
use crate::storage::{Row, Tx};

/// Where a mark a write can still change stands. A finished mark reads `removed`
/// only through [`finish`], and no query loads it back as a [`Mark`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum State {
    Removing,
    /// A tombstone: idle close and a repeated PR close leave its target be.
    Cancelled,
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
    /// The teardown the person who asked signed, which each removal Deployment the mark
    /// admits carries to every Server.
    pub(crate) teardown: Option<TeardownApproval>,
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
            teardown: None,
        }
    }
}

const COLUMNS: &str = "id, environment_id, project_id, target_name, by, at, retried, state, \
     accepted, members, evidence, residuals, teardown";

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
        teardown: row
            .optional_text(12)?
            .map(|text| serde_json::from_str(text).map_err(|_| error::corrupt("Teardown")))
            .transpose()?,
    })
}

fn state_text(state: State) -> &'static str {
    match state {
        State::Removing => "removing",
        State::Cancelled => "cancelled",
    }
}

/// What [`finish`] writes. It is no [`State`]: no query loads it back as a [`Mark`].
const REMOVED: &str = "removed";

/// A SQL condition that holds when no live removal mark names the Environment whose ID
/// `environment_column` reads, as its own target. A Project's mark does not count: the
/// callers that splice this in leave Branches of a removing Project be. Splice it into
/// a `WHERE` and keep the alias `m` free.
pub(crate) fn unmarked(environment_column: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM config_removal_mark m \
         WHERE m.environment_id = {environment_column} AND m.state = '{}')",
        state_text(State::Removing)
    )
}

/// A SQL scalar counting the marks that hold the Environment whose ID
/// `environment_column` reads: its own mark in any state but `removed`, a tombstone
/// included, and the live mark of its Project. The idle rule reads it where
/// [`unmarked`] would not do, because a tombstone and a Project removal also keep a
/// Branch from closing for sitting idle. Keep the aliases `m` and `e` free.
pub(crate) fn marks_holding(environment_column: &str) -> String {
    format!(
        "(SELECT COUNT(*) FROM config_removal_mark m \
         WHERE (m.environment_id = {environment_column} AND m.state <> '{REMOVED}') \
         OR (m.state = '{removing}' AND m.project_id = \
          (SELECT e.project_id FROM config_environment e WHERE e.id = {environment_column})))",
        removing = state_text(State::Removing)
    )
}

/// `target`'s live mark or its tombstone.
pub(crate) fn of(tx: &mut dyn Tx, target: &Target) -> Result<Option<Mark>, RpcError> {
    let (column, id) = match target {
        Target::Environment(id) => ("environment_id", id.as_str()),
        Target::Project(id) => ("project_id", id.as_str()),
    };
    tx.query(
        &format!(
            "SELECT {COLUMNS} FROM config_removal_mark WHERE {column} = ?1 AND state <> '{REMOVED}'"
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
            "SELECT {COLUMNS} FROM config_removal_mark WHERE state = '{}' AND \
             (environment_id = ?1 OR project_id = \
              (SELECT project_id FROM config_environment WHERE id = ?1))",
            state_text(State::Removing)
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
             WHERE organization_id = ?1 AND state = '{}' ORDER BY at, id",
            state_text(State::Removing)
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
        &format!(
            "SELECT residuals FROM config_removal_mark \
             WHERE organization_id = ?1 AND id = ?2 AND state = '{REMOVED}'"
        ),
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
    let teardown = mark.teardown.as_ref().map(text);
    tx.execute(
        &format!(
            "INSERT INTO config_removal_mark (organization_id, {COLUMNS}) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
             ON CONFLICT (id) DO UPDATE SET retried = excluded.retried, state = excluded.state, \
             accepted = excluded.accepted, evidence = excluded.evidence, \
             residuals = excluded.residuals, teardown = excluded.teardown"
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
            teardown.as_deref().into(),
        ],
    )?;
    Ok(())
}

/// Whether [`finish`] moved the mark.
#[must_use]
pub(crate) enum Finished {
    /// It read `removing`, and now reads `removed`.
    Moved,
    /// It no longer reads `removing`: a cancel committed after the caller read it.
    NotRemoving,
}

/// Move live mark `id` to `removed` with what its removal left on old servers. Only
/// a `removing` mark moves, so a tombstone a cancel committed since the caller read
/// the mark stays one.
pub(crate) fn finish(
    tx: &mut dyn Tx,
    id: &RemovalId,
    residuals: &[Residual],
) -> Result<Finished, RpcError> {
    let moved = tx.execute(
        &format!(
            "UPDATE config_removal_mark SET state = '{REMOVED}', residuals = ?2 \
             WHERE id = ?1 AND state = '{}'",
            state_text(State::Removing)
        ),
        &[id.as_str().into(), text(&residuals).as_str().into()],
    )?;
    Ok(if moved == 0 {
        Finished::NotRemoving
    } else {
        Finished::Moved
    })
}

/// Drop `environment`'s tombstone: a reopen, a push or a new `remove` brings it back.
pub(crate) fn clear_tombstone(tx: &mut dyn Tx, target: &Target) -> Result<(), RpcError> {
    let (column, id) = match target {
        Target::Environment(id) => ("environment_id", id.as_str()),
        Target::Project(id) => ("project_id", id.as_str()),
    };
    tx.execute(
        &format!(
            "DELETE FROM config_removal_mark WHERE {column} = ?1 AND state = '{}'",
            state_text(State::Cancelled)
        ),
        &[id.into()],
    )?;
    Ok(())
}

/// Delete `environment`'s live mark, leaving no tombstone, so a close that came before a
/// merge is undone.
pub(crate) fn drop_live(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<(), RpcError> {
    tx.execute(
        &format!(
            "DELETE FROM config_removal_mark WHERE environment_id = ?1 AND state = '{}'",
            state_text(State::Removing)
        ),
        &[environment.as_str().into()],
    )?;
    Ok(())
}

fn text(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("stored documents are JSON")
}
