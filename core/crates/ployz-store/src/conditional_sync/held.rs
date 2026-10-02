//! Values a Destination holds for the secrets a pull request's Conditional Sync
//! brings, until the merge.

use super::*;
use crate::SealingKey;
use crate::scope::EnvironmentRef;

/// Hold a Destination's value for a secret a pull request's Conditional Sync brings
/// it: it lands with the merge. Refused unless the pull request has a standing
/// Conditional Sync there that brings that secret; held for each repository whose
/// pull request of this number brings it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct HoldSecret {
    /// The Destination.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// The pull request whose merge brings the secret.
    pub pull_request: PullRequestNumber,
    /// The secret's row.
    pub row: branch::RowRef,
    /// The value, sealed at once and never shown back.
    pub value: String,
}

/// A value held for a pull request's merge.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SecretHeld {
    /// The Destination.
    pub environment: EnvironmentSummary,
    /// The pull request it waits for.
    pub pull_request: PullRequestNumber,
    /// The secret's row.
    pub row: RowId,
}

pub(crate) fn hold(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &SealingKey,
    request: &HoldSecret,
) -> Result<SecretHeld, RpcError> {
    let into = scope::lock(tx, who, &request.environment)?;
    let number = request.pull_request;
    let mut rows = Vec::new();
    for stand in tx.query(
        "SELECT repository_id, stored FROM config_conditional_sync \
         WHERE environment_id = ?1 AND number = ?2 AND state = 'standing' ORDER BY synced_at, id",
        &[into.summary.id.as_str().into(), number.into()],
    )? {
        let repository: RepositoryId = stand.number(0, "Conditional Sync")?;
        rows.push((repository, stand.json::<Stored>(1, "Conditional Sync")?));
    }
    if rows.is_empty() {
        let prs = tx.query(
            "SELECT e.name FROM config_pr_environment p JOIN config_environment e \
             ON e.id = p.environment_id WHERE p.organization_id = ?1 AND p.number = ?2",
            &[who.organization.as_str().into(), number.into()],
        )?;
        let from = match prs.as_slice() {
            [only] => only.text(0)?,
            _ => "PR_ENV",
        };
        return Err(error::invalid(
            format!(
                "#{number} has no Conditional Sync into {}: sync it there first",
                into.summary.name
            ),
            json!({ "next": format!(
                "ployz env sync --to {} --at-merge --project {} --env {from}",
                into.summary.name, into.summary.project
            ) }),
        ));
    }
    let mut brought = Vec::new();
    for (repository, stored) in &rows {
        let from = Cells::of(&stored.from, &stored.hostnames.from);
        for pick in &stored.picks {
            if pick.reviewed.is_secret() || from.at(&pick.at.row).is_secret() {
                brought.push((*repository, pick.at.clone()));
            }
        }
    }
    let named: Vec<NamedRow> = brought.iter().map(|(_, at)| at.clone()).collect();
    let row = &branch::resolve_one(&request.row, &named)?;
    let found: Vec<&(RepositoryId, NamedRow)> =
        brought.iter().filter(|(_, at)| at.row == *row).collect();
    let Some((_, label)) = found.first() else {
        let rows: Vec<String> = named.iter().map(|at| at.row.to_string()).collect();
        return Err(error::choices(
            format!(
                "#{number} brings no secret {row} into {}",
                into.summary.name
            ),
            &row.to_string(),
            rows.iter().map(String::as_str),
        ));
    };
    let secret = branch::seal_secret(sealing, row, &label.to_string(), &request.value)?;
    for (repository, _) in found {
        let pr = PullRequestRef {
            repository_id: *repository,
            number,
        };
        keep(tx, who, &into.summary.id, &pr, row, &secret)?;
    }
    Ok(SecretHeld {
        environment: into.summary,
        pull_request: number,
        row: row.clone(),
    })
}

/// Hold `secret` in `into` for `row` until `pr`'s merge.
pub(super) fn keep(
    tx: &mut dyn Tx,
    who: &Actor,
    into: &EnvironmentId,
    pr: &PullRequestRef,
    row: &RowId,
    secret: &SealedSecret,
) -> Result<(), RpcError> {
    tx.execute(
        "INSERT INTO config_held_secret (environment_id, repository_id, number, lineage, at, \
         organization_id, value) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT (environment_id, repository_id, number, lineage, at) \
         DO UPDATE SET value = excluded.value",
        &[
            into.as_str().into(),
            pr.repository_id.into(),
            pr.number.into(),
            row.lineage().into(),
            row.at().to_string().as_str().into(),
            who.organization.as_str().into(),
            branch::json_of(secret).as_str().into(),
        ],
    )?;
    Ok(())
}

/// The values held in `into` for `pr`'s merge, by row.
pub(crate) fn held(
    tx: &mut dyn Tx,
    into: &EnvironmentId,
    pr: &PullRequestRef,
) -> Result<BTreeMap<RowId, SealedSecret>, RpcError> {
    tx.query(
        "SELECT lineage, at, value FROM config_held_secret \
         WHERE environment_id = ?1 AND repository_id = ?2 AND number = ?3",
        &[
            into.as_str().into(),
            pr.repository_id.into(),
            pr.number.into(),
        ],
    )?
    .iter()
    .map(|row| {
        Ok((
            branch::row_id(row.text(0)?, row.text(1)?)?,
            row.json(2, "held secret")?,
        ))
    })
    .collect()
}

/// Forget the values held in `into` for `pr`'s merge.
pub(super) fn forget(
    tx: &mut dyn Tx,
    into: &EnvironmentId,
    pr: &PullRequestRef,
) -> Result<(), RpcError> {
    tx.execute(
        "DELETE FROM config_held_secret \
         WHERE environment_id = ?1 AND repository_id = ?2 AND number = ?3",
        &[
            into.as_str().into(),
            pr.repository_id.into(),
            pr.number.into(),
        ],
    )?;
    Ok(())
}
