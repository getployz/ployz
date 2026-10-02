//! Values a Destination holds for the secrets a pull request's Conditional Sync
//! brings, until the merge.

use super::*;
use crate::SealingKey;
use crate::scope::EnvironmentRef;
use crate::variables::{VariableKey, validate_text};

/// Hold a Destination's value for a secret a pull request's Conditional Sync brings
/// it: it lands with the merge. Refused unless the pull request has a standing
/// Conditional Sync there that brings that secret.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct HoldSecret {
    /// The Destination.
    #[serde(default)]
    pub environment: EnvironmentRef,
    pub pull_request: PullRequestNumber,
    /// The pull request's repository: needed only when pull requests of two
    /// repositories with this number bring the secret.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub repository: Option<RepositoryId>,
    /// The secret's row, as the Sync view gives it.
    pub row: RowId,
    /// The value, sealed at once and never shown back.
    pub value: String,
}

/// A value held for a pull request's merge.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SecretHeld {
    /// The Destination.
    pub environment: EnvironmentSummary,
    pub pull_request: PullRequestNumber,
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
    let asked =
        |repository: RepositoryId| request.repository.is_none_or(|asked| asked == repository);
    let mut rows = Vec::new();
    for stand in tx.query(
        "SELECT repository_id, saved FROM config_conditional_sync \
         WHERE environment_id = ?1 AND number = ?2 AND state = 'standing' ORDER BY saved_at, id",
        &[into.summary.id.as_str().into(), number.into()],
    )? {
        let repository: RepositoryId = stand.number(0, "Conditional Sync")?;
        if asked(repository) {
            rows.push((repository, stand.json::<Stored>(1, "Conditional Sync")?));
        }
    }
    if rows.is_empty() {
        let mut prs = Vec::new();
        for pr in tx.query(
            "SELECT p.repository_id, e.name FROM config_pr_environment p JOIN config_environment e \
             ON e.id = p.environment_id WHERE p.organization_id = ?1 AND p.number = ?2 \
             ORDER BY e.name",
            &[who.organization.as_str().into(), number.into()],
        )? {
            let repository: RepositoryId = pr.number(0, "PR Environment")?;
            if asked(repository) {
                prs.push(pr.text(1)?.to_owned());
            }
        }
        let from = match prs.as_slice() {
            [] => "PR_ENV",
            [only] => only.as_str(),
            _ => {
                return Err(error::choices(
                    format!(
                        "#{number} of more than one repository has a PR Environment: name the repository"
                    ),
                    "",
                    prs.iter().map(String::as_str),
                ));
            }
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
    let row = &request.row;
    let mut found = Vec::new();
    let mut brought = Vec::new();
    for (repository, stored) in rows {
        for pick in &stored.picks {
            if pick.reviewed.is_secret() || cell_at(&stored.from, &pick.row).is_secret() {
                brought.push(pick.row.to_string());
                if pick.row == *row {
                    found.push((repository, pick.at.label()));
                }
            }
        }
    }
    let Some((repository, label)) = found.first().cloned() else {
        return Err(error::choices(
            format!(
                "#{number} brings no secret {row} into {}",
                into.summary.name
            ),
            &row.to_string(),
            brought.iter().map(String::as_str),
        ));
    };
    if found.len() > 1 {
        let repositories: Vec<RepositoryId> =
            found.iter().map(|(repository, _)| *repository).collect();
        return Err(error::invalid(
            format!("#{number} of more than one repository brings {label}: name the repository"),
            json!({ "valid_children": repositories }),
        ));
    }
    if request.value.is_empty() {
        return Err(error::invalid(
            format!("{label}: a secret needs a value"),
            json!({ "row": row }),
        ));
    }
    let key = row.at();
    let key = key
        .strip_prefix("variables.")
        .ok_or_else(|| error::corrupt("Conditional Sync"))?;
    validate_text(&VariableKey::parse(key)?, &request.value)?;
    let cell = Cell::Secret {
        fingerprint: sealing.fingerprint(&request.value),
        sealed: Some(sealing.seal(&request.value)),
    };
    keep(tx, who, &into.summary.id, (repository, number), row, &cell)?;
    Ok(SecretHeld {
        environment: into.summary,
        pull_request: number,
        row: row.clone(),
    })
}

/// Hold sealed `cell` in `into` for `row` until pull request `number`'s merge.
pub(super) fn keep(
    tx: &mut dyn Tx,
    who: &Actor,
    into: &EnvironmentId,
    (repository, number): (RepositoryId, PullRequestNumber),
    row: &RowId,
    cell: &Cell,
) -> Result<(), RpcError> {
    tx.execute(
        "INSERT INTO config_held_secret (environment_id, repository_id, number, lineage, at, \
         organization_id, value) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT (environment_id, repository_id, number, lineage, at) \
         DO UPDATE SET value = excluded.value",
        &[
            into.as_str().into(),
            repository.into(),
            number.into(),
            row.lineage().into(),
            row.at().as_str().into(),
            who.organization.as_str().into(),
            branch::json_of(cell).as_str().into(),
        ],
    )?;
    Ok(())
}

/// The values held in `into` for pull request `number`'s merge, by row.
pub(crate) fn held(
    tx: &mut dyn Tx,
    into: &EnvironmentId,
    repository_id: RepositoryId,
    number: PullRequestNumber,
) -> Result<BTreeMap<RowId, Cell>, RpcError> {
    tx.query(
        "SELECT lineage, at, value FROM config_held_secret \
         WHERE environment_id = ?1 AND repository_id = ?2 AND number = ?3",
        &[into.as_str().into(), repository_id.into(), number.into()],
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

/// Forget the values held in `into` for pull request `number`'s merge.
pub(super) fn forget(
    tx: &mut dyn Tx,
    into: &EnvironmentId,
    repository_id: RepositoryId,
    number: PullRequestNumber,
) -> Result<(), RpcError> {
    tx.execute(
        "DELETE FROM config_held_secret \
         WHERE environment_id = ?1 AND repository_id = ?2 AND number = ?3",
        &[into.as_str().into(), repository_id.into(), number.into()],
    )?;
    Ok(())
}
