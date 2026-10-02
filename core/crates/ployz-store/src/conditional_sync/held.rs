//! Secrets a Conditional Sync brings by name only, and the Destination's values for
//! them, held until the merge.

use super::*;
use crate::SealingKey;
use crate::settings::SettingPath;

/// Hold a Destination's value for a secret a pull request's Conditional Sync brings
/// it by name only: it lands with the merge. Refused unless the pull request has a
/// standing Conditional Sync there that brings that secret.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct HoldSecret {
    /// The Destination.
    #[serde(default)]
    pub environment: EnvironmentRef,
    pub pull_request: PullRequestNumber,
    /// `SERVICE.env.KEY`, as the Sync names it.
    pub path: SettingPath,
    /// The value, sealed at once and never shown back.
    pub value: String,
}

/// A value held for a pull request's merge.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SecretHeld {
    /// The Destination.
    pub environment: EnvironmentSummary,
    pub pull_request: PullRequestNumber,
    pub path: SettingPath,
}

/// A secret a Conditional Sync brings.
struct Brought {
    lineage: String,
    key: String,
    /// `SERVICE.env.KEY`.
    label: String,
}

/// A Destination's value held for a pull request's merge.
pub(super) struct Held {
    lineage: String,
    key: String,
    value: SavedVariableValue,
    fingerprint: String,
}

pub(crate) fn hold(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &SealingKey,
    request: &HoldSecret,
) -> Result<SecretHeld, RpcError> {
    let into = scope::lock(tx, who, &request.environment)?;
    let number = request.pull_request;
    let rows = tx.query(
        "SELECT repository_id, saved FROM config_conditional_sync \
         WHERE environment_id = ?1 AND number = ?2 AND state = 'standing' ORDER BY saved_at, id",
        &[into.summary.id.as_str().into(), number.into()],
    )?;
    if rows.is_empty() {
        let pr = tx.query(
            "SELECT e.name FROM config_pr_environment p JOIN config_environment e \
             ON e.id = p.environment_id WHERE p.organization_id = ?1 AND p.number = ?2",
            &[who.organization.as_str().into(), number.into()],
        )?;
        let from = match pr.first() {
            Some(row) => row.text(0)?.to_owned(),
            None => "PR_ENV".to_owned(),
        };
        return Err(error::invalid(
            format!(
                "#{number} has no Conditional Sync into {}: sync it there first",
                into.summary.name
            ),
            json!({ "next": format!(
                "ployz env sync --to {} --project {} --env {from}",
                into.summary.name, into.summary.project
            ) }),
        ));
    }
    let path = request.path.to_string();
    let mut found = None;
    let mut labels = Vec::new();
    for row in rows {
        let repository: RepositoryId = row.number(0, "Conditional Sync")?;
        for secret in brought(&row.json::<Stored>(1, "Conditional Sync")?) {
            labels.push(secret.label.clone());
            if found.is_none() && secret.label == path {
                found = Some((repository, secret));
            }
        }
    }
    let Some((repository, secret)) = found else {
        return Err(error::choices(
            format!(
                "#{number} brings no secret {path} into {}",
                into.summary.name
            ),
            &path,
            labels.iter().map(String::as_str),
        ));
    };
    if request.value.is_empty() {
        return Err(error::invalid(
            format!("{path}: a secret needs a value"),
            json!({}),
        ));
    }
    crate::variables::validate_text(
        &crate::variables::VariableKey::parse(&secret.key)?,
        &request.value,
    )?;
    let value = SavedVariableValue::Secret {
        encrypted_value: Some(sealing.seal(&request.value)),
    };
    tx.execute(
        "INSERT INTO config_held_secret (environment_id, repository_id, number, lineage, variable, \
         organization_id, value, fingerprint) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT (environment_id, repository_id, number, lineage, variable) \
         DO UPDATE SET value = excluded.value, fingerprint = excluded.fingerprint",
        &[
            into.summary.id.as_str().into(),
            repository.into(),
            number.into(),
            secret.lineage.as_str().into(),
            secret.key.as_str().into(),
            who.organization.as_str().into(),
            serde_json::to_string(&value)
                .expect("a variable value is JSON")
                .as_str()
                .into(),
            sealing.fingerprint(&request.value).as_str().into(),
        ],
    )?;
    Ok(SecretHeld {
        environment: into.summary,
        pull_request: number,
        path: request.path.clone(),
    })
}

/// The secrets `stored` brings: those of its picked variables, or of the Services it
/// introduces.
fn brought(stored: &Stored) -> Vec<Brought> {
    let mut brought = Vec::new();
    for service in &stored.landing.from.services {
        let lineage = &service.lineage_id;
        for variable in &service.variables {
            let secret = matches!(
                variable.value,
                SavedVariableValue::Secret { .. } | SavedVariableValue::SecretWithoutValue
            );
            let picked = stored.picks.iter().any(|pick| {
                pick.key == format!("{lineage}:node")
                    || pick.key == format!("{lineage}:variables.{}", variable.key)
            });
            if secret && picked {
                brought.push(Brought {
                    lineage: lineage.clone(),
                    key: variable.key.clone(),
                    label: format!("{}.env.{}", service.slug, variable.key),
                });
            }
        }
    }
    brought
}

/// The secrets `stored` brings that `into` has no value of and none is held for,
/// as `SERVICE.env.KEY`.
pub(super) fn waiting(
    stored: &Stored,
    into: &SavedEnvironmentIntent,
    held: &[Held],
) -> Vec<String> {
    brought(stored)
        .into_iter()
        .filter(|secret| {
            let has = variable(into, &secret.lineage, &secret.key)
                .is_some_and(|own| matches!(own.value, SavedVariableValue::Secret { .. }));
            !has && !held
                .iter()
                .any(|held| held.lineage == secret.lineage && held.key == secret.key)
        })
        .map(|secret| secret.label)
        .collect()
}

fn variable<'a>(
    intent: &'a SavedEnvironmentIntent,
    lineage: &str,
    key: &str,
) -> Option<&'a SavedVariableIntent> {
    intent
        .services
        .iter()
        .find(|service| service.lineage_id == lineage)?
        .variables
        .iter()
        .find(|variable| variable.key == key)
}

/// The values held in `into` for pull request `number`'s merge.
pub(super) fn held(
    tx: &mut dyn Tx,
    into: &EnvironmentId,
    repository_id: RepositoryId,
    number: PullRequestNumber,
) -> Result<Vec<Held>, RpcError> {
    let rows = tx.query(
        "SELECT lineage, variable, value, fingerprint FROM config_held_secret \
         WHERE environment_id = ?1 AND repository_id = ?2 AND number = ?3",
        &[into.as_str().into(), repository_id.into(), number.into()],
    )?;
    let mut held = Vec::new();
    for row in rows {
        held.push(Held {
            lineage: row.text(0)?.to_owned(),
            key: row.text(1)?.to_owned(),
            value: row.json(2, "held secret")?,
            fingerprint: row.text(3)?.to_owned(),
        });
    }
    Ok(held)
}

/// Whether a value is held for row `key` (`LINEAGE:variables.KEY`).
pub(super) fn is_held(held: &[Held], key: &str) -> bool {
    held.iter()
        .any(|held| key == format!("{}:variables.{}", held.lineage, held.key))
}

/// Give each secret `intent` holds without a value the value held for it.
pub(super) fn fill(intent: &mut SavedEnvironmentIntent, held: &[Held]) {
    for held in held {
        let variable = intent
            .services
            .iter_mut()
            .find(|service| service.lineage_id == held.lineage)
            .and_then(|service| {
                service.variables.iter_mut().find(|variable| {
                    variable.key == held.key
                        && variable.value == SavedVariableValue::SecretWithoutValue
                })
            });
        if let Some(variable) = variable {
            variable.value = held.value.clone();
            variable.value_fingerprint.clone_from(&held.fingerprint);
        }
    }
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
