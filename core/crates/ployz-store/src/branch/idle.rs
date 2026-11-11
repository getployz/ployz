//! When a Branch closes for sitting idle, and the rows the sweep reads to close it.
//! Only the facts live here: what closing does to a Branch is the sweep's to decide.

use ployz_core::RpcError;

use crate::id::{EnvironmentId, OrganizationId};
use crate::removal::mark;
use crate::storage::{Row, Tx};

/// A Branch closes after this long without a Deployment.
const IDLE: i64 = 7 * 24 * 60 * 60;

/// What the idle rule reads of each Branch: `environment_id`, kept, whether a
/// removal names it (or a cancelled one did), its latest admission (-1 for none),
/// its Branches and whether it is a Default.
fn idle_facts() -> String {
    format!(
        "SELECT b.environment_id, b.kept, {}, \
         (SELECT COALESCE(MAX(d.admitted), -1) FROM config_deployment d WHERE d.environment_id = b.environment_id), \
         (SELECT COUNT(*) FROM config_environment_branch c WHERE c.parent_id = b.environment_id), \
         (SELECT COUNT(*) FROM config_project p WHERE p.default_environment_id = b.environment_id) \
         FROM config_environment_branch b",
        mark::marks_holding("b.environment_id")
    )
}

/// When the sweep closes a Branch for sitting idle, from an [`idle_facts`] row: a
/// week after its latest Deployment. Never when it is kept, being removed or its
/// removal was cancelled, never
/// deployed, a Parent or the Default Environment.
fn idle_close(row: &Row) -> Result<Option<i64>, RpcError> {
    let (kept, marked, admitted) = (row.int(1)?, row.int(2)?, row.int(3)?);
    let held = kept != 0 || marked != 0 || admitted < 0 || row.int(4)? != 0 || row.int(5)? != 0;
    Ok((!held).then_some(admitted + IDLE))
}

/// When a Branch closes for sitting idle, in seconds since the Unix epoch.
pub(crate) fn closes_at(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<Option<i64>, RpcError> {
    let rows = tx.query(
        &format!("{} WHERE b.environment_id = ?1", idle_facts()),
        &[id.as_str().into()],
    )?;
    rows.first().map_or(Ok(None), idle_close)
}

/// A Branch and when it closes for sitting idle: none when it never does.
pub(crate) struct Deadline {
    pub(crate) environment: EnvironmentId,
    pub(crate) closes_at: Option<i64>,
}

/// Every Branch of `organization` with its idle deadline, in Environment ID order.
pub(crate) fn deadlines(
    tx: &mut dyn Tx,
    organization: &OrganizationId,
) -> Result<Vec<Deadline>, RpcError> {
    let rows = tx.query(
        &format!(
            "{} WHERE b.organization_id = ?1 ORDER BY b.environment_id",
            idle_facts()
        ),
        &[organization.as_str().into()],
    )?;
    rows.iter()
        .map(|row| {
            Ok(Deadline {
                environment: row.parse(0, "Environment ID")?,
                closes_at: idle_close(row)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Cell;

    /// An [`idle_facts`] row: kept, marks holding it, latest admission, Branches, Defaults.
    fn facts(kept: i64, marks: i64, admitted: i64, branches: i64, defaults: i64) -> Row {
        Row(vec![
            Cell::Text("00000000-0000-4000-8000-000000000020".to_owned()),
            Cell::Int(kept),
            Cell::Int(marks),
            Cell::Int(admitted),
            Cell::Int(branches),
            Cell::Int(defaults),
        ])
    }

    #[test]
    fn a_branch_closes_a_week_after_its_latest_deployment_unless_something_holds_it() {
        let week = 7 * 24 * 60 * 60;
        assert_eq!(
            idle_close(&facts(0, 0, 100, 0, 0)).unwrap(),
            Some(100 + week)
        );
        assert_eq!(idle_close(&facts(1, 0, 100, 0, 0)).unwrap(), None, "kept");
        assert_eq!(idle_close(&facts(0, 1, 100, 0, 0)).unwrap(), None, "marked");
        assert_eq!(
            idle_close(&facts(0, 0, -1, 0, 0)).unwrap(),
            None,
            "never deployed"
        );
        assert_eq!(
            idle_close(&facts(0, 0, 100, 2, 0)).unwrap(),
            None,
            "a Parent"
        );
        assert_eq!(
            idle_close(&facts(0, 0, 100, 0, 1)).unwrap(),
            None,
            "a Default"
        );
    }
}
