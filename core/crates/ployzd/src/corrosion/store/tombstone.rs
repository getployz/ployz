//! The `cluster` rows a forget leaves behind, and the guards that read them.
//!
//! Both key spellings are replicated and read by daemons of every release, so they
//! are only ever added to.

use ployz_core::MachineId;
use serde_json::json;

use crate::corrosion::Statement;

/// A forgotten Machine's tombstone in the `cluster` table, keyed `forgotten:<identity>` by
/// the Management Identity its row named. Its value is the forgotten Machine's id. A row
/// that named no identity leaves one keyed `forgotten-id:<id>` instead, outside the
/// `forgotten:` prefix older daemons' guards read; only SQL writes and reads that one.
pub(in crate::corrosion) struct Tombstone;

const FORGET_INSERT: &str = "INSERT INTO cluster (key, value) SELECT ";
const IDENTITY_OF_ROW: &str = "json_extract(info, '$.runtime.management_identity')";

impl Tombstone {
    const IDENTITY_PREFIX: &'static str = "forgotten:";
    const ID_PREFIX: &'static str = "forgotten-id:";

    /// The SQL condition that holds when no tombstone of either kind forgets the id
    /// `id`, an expression of the statement it joins. A tombstone keyed by an identity
    /// is found by its value, so every key under the `forgotten:` prefix counts.
    pub(super) fn not_forgotten_sql(id: &str) -> String {
        format!(
            "NOT EXISTS (SELECT 1 FROM cluster WHERE key LIKE '{}%' AND value = {id}) \
             AND NOT EXISTS (SELECT 1 FROM cluster WHERE key = '{}' || {id})",
            Self::IDENTITY_PREFIX,
            Self::ID_PREFIX,
        )
    }

    /// The inserts that tombstone `machine_id`'s row, run before its removal: one
    /// writes the identity-keyed tombstone when the row names an identity, the other
    /// the id-keyed one when it names none.
    pub(super) fn forget(machine_id: &MachineId) -> [Statement; 2] {
        let insert = |key: String, condition: &str| {
            Statement::new(
                format!(
                    "{FORGET_INSERT}{key}, id \
                     FROM machines \
                     WHERE id = ? AND {IDENTITY_OF_ROW} {condition} \
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value"
                ),
                [json!(machine_id)],
            )
        };
        [
            insert(
                format!("'{}' || {IDENTITY_OF_ROW}", Self::IDENTITY_PREFIX),
                "IS NOT NULL",
            ),
            insert(format!("'{}' || id", Self::ID_PREFIX), "IS NULL"),
        ]
    }
}
