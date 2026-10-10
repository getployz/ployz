//! Offering a PR Environment's changes to one of its Destinations.

use ployz_core::config::redact_environment_intent;

use super::*;
use crate::branch::{Guard, SyncChanges, Synced, SyncedWhen};
use crate::id::SyncId;
use crate::pull_request::PullRequest;
use crate::{SealingKey, teardown};

/// Offer a PR Environment's picked rows to one of its Destinations, replacing its
/// offer there. The values given for its secrets are sealed into the offer.
pub(crate) fn sync(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &SealingKey,
    request: &SyncChanges,
    (from, into, pr): (Environment, Environment, PullRequest),
) -> Result<Synced, RpcError> {
    ready(tx, &from, &pr)?;
    let sync = Move::sync(tx, &from, &into)?;
    let checked = sync.check(tx, &into, Guard::Sync(&request.version))?;
    let sides = [&from.working, &into.working];
    let (picks, values) = branch::picks(checked.rows(), &sides, request)?;
    if picks.is_empty() {
        return Err(error::conflict(
            format!("Nothing to sync into {}", into.summary.name),
            json!({ "version": checked.version() }),
        ));
    }
    let values = branch::sealed(sealing, checked.rows(), &sides, &picks, &values)?;
    // Refuse what couldn't land, such as a new Service's variable without it.
    checked
        .plan()
        .apply(&picks, &values)
        .map_err(branch::config)?;
    let from_names = from.names();
    let mut kept = Vec::new();
    for row in checked.rows().iter().filter(|row| picks.contains(&row.id)) {
        kept.push(Pick {
            at: branch::named(&sides, &row.id).ok_or_else(|| error::corrupt("Sync row"))?,
            reviewed: row.into.clone(),
            from: branch::shown(&from.working, &from_names, &row.id, &row.from),
        });
    }
    let rows = kept.iter().map(|pick| pick.at.clone()).collect();
    let stored = Stored {
        picks: kept,
        carried: Carried::of(tx, &from.summary.id, &sync.from)?,
        from: redact_environment_intent(sync.from.clone()),
        base: redact_environment_intent(sync.base.clone()),
        hostnames: sync.hostnames.clone(),
        environment: from.summary.clone(),
    };
    tx.execute(
        "DELETE FROM config_proposal WHERE environment_id = ?1 AND offered IS NOT NULL \
         AND (source_environment_id = ?2 OR (repository_id = ?3 AND number = ?4))",
        &[
            into.summary.id.as_str().into(),
            from.summary.id.as_str().into(),
            pr.repository_id.into(),
            pr.number.into(),
        ],
    )?;
    let id = ProposalId::parse(uuid::Uuid::new_v4().to_string())?;
    let sync_id = SyncId::parse(id.as_str())?;
    tx.execute(
        "INSERT INTO config_proposal (id, organization_id, environment_id, \
         source_environment_id, repository_id, number, source_name, source_revision, \
         first_sync, last_sync, offered) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?1, ?1, ?9)",
        &[
            id.as_str().into(),
            who.organization.as_str().into(),
            into.summary.id.as_str().into(),
            from.summary.id.as_str().into(),
            pr.repository_id.into(),
            pr.number.into(),
            from.summary.name.as_str().into(),
            crate::scope::revision_param(from.summary.revision)?.into(),
            Offer::encode(&stored, &values).as_str().into(),
        ],
    )?;
    branch::keep_receipt(tx, who, (&into.summary.id, &from), &id, &sync_id)?;
    Ok(Synced {
        sync: sync_id,
        from: from.summary,
        into: into.summary,
        when: SyncedWhen::AtMerge {
            conditional_sync: ConditionalSync {
                id: ConditionalSyncId::parse(id.as_str())?,
                pull_request: pr.number,
                rows,
                state: ConditionalSyncState::Standing,
            },
        },
    })
}

/// Refuse an offer while the pull request is closed or its PR Environment is going.
fn ready(tx: &mut dyn Tx, from: &Environment, pr: &PullRequest) -> Result<(), RpcError> {
    if !pr.open {
        return Err(error::conflict(
            format!("PR #{} is closed", pr.number),
            json!({}),
        ));
    }
    if pull_request::closing(tx, &from.summary.id)? {
        return Err(error::conflict(
            format!("{} is closing", from.summary.name),
            json!({}),
        ));
    }
    if let Some(removal) = teardown::removing(tx, &from.summary.id)? {
        return Err(teardown::being_removed(from, &removal));
    }
    Ok(())
}
