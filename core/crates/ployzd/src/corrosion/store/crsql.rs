//! Reads of cr-sqlite's and Corrosion's own tables: which Machines this store has held,
//! and how far replication has got.
//!
//! These tables (`machines__crsql_pks`, `crsql_db_versions`, `__corro_bookkeeping_gaps`,
//! `__corro_seq_bookkeeping`) belong to the Corrosion image, not to `schema.sql`, and every
//! query on them is here. They were checked against the image [`IMAGE`](crate::corrosion::service::IMAGE) pins, so a bump of
//! that digest must re-run the `#[ignore]`d integration tests in `corrosion::integration_tests`
//! before it ships. The test double stands the tables in with `crsql_stand_ins` in `store_tests`.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::MachineId;
use serde_json::json;

use super::{ReplicatedStore, actor_id, text};
use crate::corrosion::{Error, Statement};

impl ReplicatedStore {
    /// This Machine's own Corrosion actor, which only this Machine's writes advance.
    pub(crate) async fn local_actor(&self) -> Result<String, Error> {
        let query = self
            .api
            .query(Statement::new("SELECT crsql_site_id() AS site_id", []))
            .await?;
        match query.rows(["site_id"])?.as_slice() {
            [[actor]] => actor_id(actor),
            _ => Err(Error::Protocol("invalid local actor row".into())),
        }
    }

    /// Whether this store has ever held a row for a Machine other than `own`. A removed
    /// row keeps its cr-sqlite key, so only a store created afresh, before gossip
    /// reaches it, reads `false` once another Machine exists.
    pub(crate) async fn held_another_machine(&self, own: &MachineId) -> Result<bool, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT EXISTS (SELECT 1 FROM machines__crsql_pks WHERE id != ?) AS held",
                [json!(own)],
            ))
            .await?;
        match query.rows(["held"])?.as_slice() {
            [[held]] if held.as_i64() == Some(0) => Ok(false),
            [[held]] if held.as_i64() == Some(1) => Ok(true),
            _ => Err(Error::Protocol("invalid held-another-Machine row".into())),
        }
    }

    /// Which of `ids` name a Machine whose row this store held and then deleted, as `server
    /// rm` does: a deleted row keeps its cr-sqlite key.
    pub(crate) async fn removed_machines(
        &self,
        ids: &[MachineId],
    ) -> Result<BTreeSet<MachineId>, Error> {
        if ids.is_empty() {
            return Ok(BTreeSet::new());
        }
        let query = self
            .api
            .query(Statement::new(
                format!(
                    "SELECT id FROM machines__crsql_pks WHERE id IN ({}) \
                     AND NOT EXISTS (SELECT 1 FROM machines WHERE machines.id = machines__crsql_pks.id)",
                    vec!["?"; ids.len()].join(", "),
                ),
                ids.iter().map(|id| json!(id)),
            ))
            .await?;
        query
            .rows(["id"])?
            .into_iter()
            .map(|[id]| Ok(MachineId::parse(text(&id, "removed Machine ID")?)?))
            .collect()
    }

    /// Each actor's highest version this store holds with no hole below it: a catch-up
    /// target.
    pub async fn version(&self) -> Result<BTreeMap<String, i64>, Error> {
        let query = self.api.query(Statement::new(VERSION_QUERY, [])).await?;
        query
            .rows(["site_id", "db_version"])?
            .into_iter()
            .map(|[actor, version]| {
                let actor = actor_id(&actor)?;
                let version = version
                    .as_i64()
                    .ok_or_else(|| Error::Protocol("invalid actor version".into()))?;
                Ok((actor, version))
            })
            .collect()
    }

    /// Checks replication bookkeeping through the target in one SQLite snapshot.
    /// Superseded versions can complete before replacement data arrives; callers
    /// requiring a particular record or condition must still verify it.
    ///
    /// # Errors
    /// Returns an error for invalid targets, failed store queries, or malformed responses.
    pub(crate) async fn has_reached_version(
        &self,
        target: &BTreeMap<String, i64>,
    ) -> Result<bool, Error> {
        let mut placeholders = Vec::with_capacity(target.len());
        let mut params = Vec::with_capacity(target.len() * 2);
        for (actor, version) in target {
            let bytes = hex::decode(actor.replace('-', ""))
                .map_err(|_| Error::InvalidCatchUpTarget("invalid actor ID"))?;
            if bytes.len() != 16 || *version < 0 {
                return Err(Error::InvalidCatchUpTarget(
                    "actor must be 16 bytes and version must be nonnegative",
                ));
            }
            if *version == 0 {
                continue;
            }
            placeholders.push("(?, ?)");
            params.extend([json!(bytes), json!(version)]);
        }
        if params.is_empty() {
            return Ok(true);
        }
        let query = format!(
            "WITH target(actor_id, version) AS (VALUES {})
             SELECT NOT EXISTS (
                 SELECT 1 FROM target
                 LEFT JOIN crsql_db_versions AS current ON current.site_id = target.actor_id
                 WHERE COALESCE(current.db_version, 0) < target.version
                 OR EXISTS (
                     SELECT 1 FROM __corro_bookkeeping_gaps
                     WHERE actor_id = target.actor_id AND start <= target.version
                 )
                 OR EXISTS (
                     SELECT 1 FROM __corro_seq_bookkeeping
                     WHERE site_id = target.actor_id AND db_version <= target.version
                 )
             ) AS reached",
            placeholders.join(", ")
        );
        let result = self.api.query(Statement::new(query, params)).await?;
        match result.rows(["reached"])?.as_slice() {
            [[value]] if value.as_i64() == Some(0) => Ok(false),
            [[value]] if value.as_i64() == Some(1) => Ok(true),
            _ => Err(Error::Protocol("invalid replication status row".into())),
        }
    }
}

/// A dead actor's hole is shared by every live store and never fills, so a target past
/// it could never be reached (#3611).
pub(in crate::corrosion) const VERSION_QUERY: &str = "SELECT site_id, db_version FROM (
     SELECT current.site_id, MIN(
         current.db_version,
         COALESCE((SELECT MIN(start) FROM __corro_bookkeeping_gaps
                   WHERE actor_id = current.site_id) - 1, current.db_version),
         COALESCE((SELECT MIN(db_version) FROM __corro_seq_bookkeeping
                   WHERE site_id = current.site_id) - 1, current.db_version)
     ) AS db_version
     FROM crsql_db_versions AS current
 ) WHERE db_version > 0";
