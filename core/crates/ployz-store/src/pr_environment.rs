//! `config_pr_environment`: which Environment is the PR Environment of which pull
//! request. A leaf below `branch` and `pull_request`, so a Branch can name its pull
//! request without reaching into the lifecycle that makes and closes them.

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::id::{EnvironmentId, PullRequestNumber, RepositoryId};
use crate::removal::mark;
use crate::storage::{Row, Tx};

/// A pull request whose GitHub check Cloud publishes again from
/// [`crate::Query::PullRequest`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PullRequestRef {
    pub repository_id: RepositoryId,
    pub number: PullRequestNumber,
}

/// The pull request (repository ID, number) Environment `id` is the PR Environment of.
pub(crate) fn of(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<Option<PullRequestRef>, RpcError> {
    let rows = tx.query(
        "SELECT repository_id, number FROM config_pr_environment WHERE environment_id = ?1",
        &[id.as_str().into()],
    )?;
    rows.first().map(pull_request_ref).transpose()
}

/// A pull request's repository and number, the first two columns of `row`.
pub(crate) fn pull_request_ref(row: &Row) -> Result<PullRequestRef, RpcError> {
    Ok(PullRequestRef {
        repository_id: row.number(0, "pull request")?,
        number: row.number(1, "pull request")?,
    })
}

/// A derived table `r` of the PR Environments not being removed, to join a pull
/// request's number to the preview it has now: its `id`, `repository_id`, `number`,
/// `project_id`, `name` and `working_revision`. A pull request reopened while its old
/// preview closes has this one and the old one's row.
pub(crate) fn live_previews() -> String {
    format!(
        "(SELECT q.environment_id AS id, q.repository_id, q.number, \
          v.project_id, v.name, v.working_revision \
          FROM config_pr_environment q JOIN config_environment v ON v.id = q.environment_id \
          JOIN config_environment_branch b ON b.environment_id = q.environment_id \
          WHERE {}) r",
        mark::unmarked("b.environment_id")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Cell;

    #[test]
    fn a_pull_request_ref_keeps_its_wire_form() {
        let row = Row(vec![Cell::Int(11), Cell::Int(5)]);
        let pr = pull_request_ref(&row).unwrap();
        assert_eq!(
            serde_json::to_string(&pr).unwrap(),
            r#"{"repository_id":11,"number":5}"#
        );
    }
}
