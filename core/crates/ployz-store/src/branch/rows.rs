//! The `config_environment_branch` table: one row per Branch, naming its Parent,
//! whether it is kept, what its Setup Commands are and what it was made with.

use ployz_core::RpcError;
use ployz_core::config::SavedEnvironmentIntent;

use super::Setup;
use crate::id::EnvironmentId;
use crate::storage::{self, Tx};

/// A Branch's row.
pub(crate) struct Row {
    pub(crate) parent: EnvironmentId,
    pub(crate) kept: bool,
    pub(super) setup: Vec<Setup>,
}

pub(crate) fn row(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<Option<Row>, RpcError> {
    let rows = tx.query(
        "SELECT parent_id, kept, setup FROM config_environment_branch WHERE environment_id = ?1",
        &[id.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(Row {
        parent: row.parse::<EnvironmentId>(0, "Branch")?,
        kept: row.int(1)? != 0,
        setup: row.json(2, "Branch")?,
    }))
}

/// What Branch `id` was made with; none for a root.
pub(crate) fn made_with(
    tx: &mut dyn Tx,
    id: &EnvironmentId,
) -> Result<Option<SavedEnvironmentIntent>, RpcError> {
    tx.query(
        "SELECT made_with FROM config_environment_branch WHERE environment_id = ?1",
        &[id.as_str().into()],
    )?
    .first()
    .map(|row| row.intent(0, "Branch"))
    .transpose()
}

pub(super) fn insert(
    tx: &mut dyn Tx,
    organization: &str,
    id: &EnvironmentId,
    parent: &EnvironmentId,
    kept: bool,
    made_with: &str,
    setup: &[Setup],
) -> Result<(), RpcError> {
    tx.execute(
        "INSERT INTO config_environment_branch (environment_id, organization_id, parent_id, kept, made_with, setup) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        &[
            id.as_str().into(),
            organization.into(),
            parent.as_str().into(),
            i64::from(kept).into(),
            made_with.into(),
            serde_json::to_string(setup)
                .expect("Setup Commands are JSON")
                .as_str()
                .into(),
        ],
    )?;
    Ok(())
}

pub(super) fn set_kept(tx: &mut dyn Tx, id: &EnvironmentId, kept: bool) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_environment_branch SET kept = ?1 WHERE environment_id = ?2",
        &[i64::from(kept).into(), id.as_str().into()],
    )?;
    Ok(())
}

pub(super) fn set_setup(
    tx: &mut dyn Tx,
    id: &EnvironmentId,
    setup: &[Setup],
) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_environment_branch SET setup = ?1 WHERE environment_id = ?2",
        &[
            serde_json::to_string(setup)
                .expect("Setup Commands are JSON")
                .as_str()
                .into(),
            id.as_str().into(),
        ],
    )?;
    Ok(())
}

/// What one Branch row was made with, as read: it can be replaced while still as read.
pub(crate) struct MadeWith {
    environment: String,
    pub(crate) intent: SavedEnvironmentIntent,
    text: String,
}

impl MadeWith {
    fn of(row: &storage::Row) -> Result<Self, RpcError> {
        Ok(Self {
            environment: row.text(0)?.to_owned(),
            intent: row.intent(1, "Branch")?,
            text: row.text(1)?.to_owned(),
        })
    }

    /// Replace what the Branch was made with by `new`, unless it moved since it was read.
    pub(crate) fn replace(
        &self,
        tx: &mut dyn Tx,
        new: &SavedEnvironmentIntent,
    ) -> Result<(), RpcError> {
        tx.execute(
            "UPDATE config_environment_branch SET made_with = ?2 \
             WHERE environment_id = ?1 AND made_with = ?3",
            &[
                self.environment.as_str().into(),
                super::document(new).as_str().into(),
                self.text.as_str().into(),
            ],
        )?;
        Ok(())
    }
}

/// What `environment` and the Branches directly below it were made with.
pub(crate) fn made_with_around(
    tx: &mut dyn Tx,
    environment: &str,
) -> Result<Vec<MadeWith>, RpcError> {
    tx.query(
        "SELECT environment_id, made_with FROM config_environment_branch \
         WHERE environment_id = ?1 OR parent_id = ?1",
        &[environment.into()],
    )?
    .iter()
    .map(MadeWith::of)
    .collect()
}

/// What Branch `environment` was made with; none for a root.
pub(crate) fn made_with_of(
    tx: &mut dyn Tx,
    environment: &str,
) -> Result<Option<MadeWith>, RpcError> {
    tx.query(
        "SELECT environment_id, made_with FROM config_environment_branch WHERE environment_id = ?1",
        &[environment.into()],
    )?
    .first()
    .map(MadeWith::of)
    .transpose()
}

/// The Environment Branch `environment` comes from; none for a root.
pub(crate) fn parent_of(tx: &mut dyn Tx, environment: &str) -> Result<Option<String>, RpcError> {
    tx.query(
        "SELECT parent_id FROM config_environment_branch WHERE environment_id = ?1",
        &[environment.into()],
    )?
    .first()
    .map(|row| row.text(0).map(str::to_owned))
    .transpose()
}

/// The Environment Branch `environment` comes from and what it was made with.
pub(crate) fn parent_and_made_with(
    tx: &mut dyn Tx,
    environment: &str,
) -> Result<Option<(String, SavedEnvironmentIntent)>, RpcError> {
    tx.query(
        "SELECT parent_id, made_with FROM config_environment_branch WHERE environment_id = ?1",
        &[environment.into()],
    )?
    .first()
    .map(|row| Ok((row.text(0)?.to_owned(), row.intent(1, "Branch")?)))
    .transpose()
}

/// The Branches directly below `parent`.
pub(crate) fn children(tx: &mut dyn Tx, parent: &str) -> Result<Vec<String>, RpcError> {
    tx.query(
        "SELECT environment_id FROM config_environment_branch WHERE parent_id = ?1",
        &[parent.into()],
    )?
    .iter()
    .map(|row| Ok(row.text(0)?.to_owned()))
    .collect()
}
