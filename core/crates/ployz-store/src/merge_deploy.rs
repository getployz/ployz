//! The `config_merge_deploy` table: what merges landed in an Environment that no
//! Deployment took yet, one row per merge commit into a branch.

use ployz_core::RpcError;

use crate::id::{EnvironmentId, RepositoryId};
use crate::storage::{Row, Tx};

/// The merge commit `commit` into `branch` of Git repository `repository_id`, landed in
/// `environment`.
#[derive(Clone, Copy)]
pub(crate) struct Landed<'a> {
    pub(crate) environment: &'a EnvironmentId,
    pub(crate) repository_id: RepositoryId,
    pub(crate) branch: &'a str,
    pub(crate) commit: &'a str,
}

/// The row of `landed`: its changed and removed Services.
pub(crate) fn get(tx: &mut dyn Tx, landed: Landed<'_>) -> Result<Option<Row>, RpcError> {
    Ok(tx
        .query(
            "SELECT services, removed_services FROM config_merge_deploy \
             WHERE environment_id = ?1 AND repository_id = ?2 AND branch = ?3 AND commit_sha = ?4",
            &[
                landed.environment.as_str().into(),
                landed.repository_id.into(),
                landed.branch.into(),
                landed.commit.into(),
            ],
        )?
        .into_iter()
        .next())
}

/// Keep `services` and `removed` for `landed`, after the merges already kept for its branch.
pub(crate) fn put(
    tx: &mut dyn Tx,
    organization: &str,
    landed: Landed<'_>,
    services: &str,
    removed: &str,
) -> Result<(), RpcError> {
    tx.execute(
        "INSERT INTO config_merge_deploy \
         (environment_id, repository_id, branch, commit_sha, seq, organization_id, services, \
          removed_services) \
         SELECT ?1, ?2, ?3, ?4, COALESCE(MAX(seq), 0) + 1, ?5, ?6, ?7 FROM config_merge_deploy \
         WHERE environment_id = ?1 AND repository_id = ?2 AND branch = ?3 \
         ON CONFLICT (environment_id, repository_id, branch, commit_sha) \
         DO UPDATE SET services = excluded.services, \
         removed_services = excluded.removed_services",
        &[
            landed.environment.as_str().into(),
            landed.repository_id.into(),
            landed.branch.into(),
            landed.commit.into(),
            organization.into(),
            services.into(),
            removed.into(),
        ],
    )?;
    Ok(())
}

/// Every merge into `branch` kept for `environment`, oldest first: changed Services,
/// removed Services, merge commit.
pub(crate) fn on_branch(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    repository_id: RepositoryId,
    branch: &str,
) -> Result<Vec<Row>, RpcError> {
    tx.query(
        "SELECT services, removed_services, commit_sha FROM config_merge_deploy \
         WHERE environment_id = ?1 AND repository_id = ?2 AND branch = ?3 ORDER BY seq",
        &[
            environment.as_str().into(),
            repository_id.into(),
            branch.into(),
        ],
    )
}

/// Every merge kept for `environment`: changed Services, removed Services, Git
/// repository, branch, merge commit.
pub(crate) fn of_environment(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Vec<Row>, RpcError> {
    tx.query(
        "SELECT services, removed_services, repository_id, branch, commit_sha \
         FROM config_merge_deploy WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )
}

/// Keep only `services` and `removed` of `landed`.
pub(crate) fn replace(
    tx: &mut dyn Tx,
    landed: Landed<'_>,
    services: &str,
    removed: &str,
) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_merge_deploy SET services = ?5, removed_services = ?6 \
         WHERE environment_id = ?1 AND repository_id = ?2 AND branch = ?3 \
         AND commit_sha = ?4",
        &[
            landed.environment.as_str().into(),
            landed.repository_id.into(),
            landed.branch.into(),
            landed.commit.into(),
            services.into(),
            removed.into(),
        ],
    )?;
    Ok(())
}

/// Forget `landed`.
pub(crate) fn forget(tx: &mut dyn Tx, landed: Landed<'_>) -> Result<(), RpcError> {
    tx.execute(
        "DELETE FROM config_merge_deploy WHERE environment_id = ?1 \
         AND repository_id = ?2 AND branch = ?3 AND commit_sha = ?4",
        &[
            landed.environment.as_str().into(),
            landed.repository_id.into(),
            landed.branch.into(),
            landed.commit.into(),
        ],
    )?;
    Ok(())
}
