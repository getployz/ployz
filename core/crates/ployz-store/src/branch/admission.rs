//! What a draft that includes pull requests may publish: only what merged here.
//!
//! Save and Deploy end a draft's included proposals ([`consume`]), and only a
//! [`Ready`] lets them: [`ready`] builds it when every pull request the draft
//! includes merged into a branch the Environment deploys. Offers are not in the
//! draft, so nothing here touches them.

use ployz_core::RpcError;
use serde_json::json;

use crate::error;
use crate::id::{EnvironmentId, ProposalId, PullRequestNumber, RepositoryId};
use crate::pull_request::{self, PullRequestRef, Readiness};
use crate::review::Review;
use crate::storage::Tx;

/// A pull request a draft includes, and whether it merged here.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct Gated {
    pub(crate) proposal: ProposalId,
    pub(crate) source_revision: u64,
    pub(crate) number: PullRequestNumber,
    pub(crate) readiness: Readiness,
}

/// The pull requests `environment`'s draft includes, by proposal.
pub(crate) fn gated(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<Vec<Gated>, RpcError> {
    let rows = tx.query(
        "SELECT id, source_revision, repository_id, number FROM config_proposal \
         WHERE environment_id = ?1 AND offered IS NULL AND number IS NOT NULL ORDER BY id",
        &[environment.as_str().into()],
    )?;
    let mut gated = Vec::with_capacity(rows.len());
    for row in &rows {
        let reference = PullRequestRef {
            repository_id: row.number::<RepositoryId>(2, "proposal")?,
            number: row.number(3, "proposal")?,
        };
        gated.push(Gated {
            proposal: row.parse(0, "proposal")?,
            source_revision: row.number(1, "proposal")?,
            number: reference.number,
            readiness: pull_request::readiness(tx, environment, &reference)?,
        });
    }
    gated.sort();
    Ok(gated)
}

/// The suffix a review's version takes when its draft includes pull requests: it
/// changes when one merges, closes or is removed, so a Save reviewed before is stale.
pub(crate) fn version_gate(gated: &[Gated]) -> Option<String> {
    if gated.is_empty() {
        return None;
    }
    let text = gated
        .iter()
        .map(|gated| {
            format!(
                "{}\u{0}{}\u{0}{}",
                gated.proposal,
                gated.source_revision,
                gated.readiness.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    let head = digest.as_ref().get(..4).unwrap_or_default();
    Some(format!("g{}", hex::encode(head)))
}

/// A draft whose included pull requests all merged here: only [`ready`] makes one.
pub(crate) struct Ready {
    environment: EnvironmentId,
}

/// The draft `review` reviewed may be saved or deployed: refused, naming the pull
/// request, while one it includes hasn't merged into a branch this Environment deploys.
pub(crate) fn ready(review: &Review) -> Result<Ready, RpcError> {
    if let Some(waits) = review
        .gated
        .iter()
        .find(|gated| gated.readiness != Readiness::Ready)
    {
        let number = waits.number;
        let message = match waits.readiness {
            Readiness::Elsewhere => {
                format!("#{number} merged elsewhere: Remove it to save the rest.")
            }
            Readiness::Closed => format!("#{number} closed unmerged: Remove it to save the rest."),
            Readiness::Open | Readiness::Ready => {
                format!("#{number} isn't merged: Remove it to save the rest.")
            }
        };
        return Err(error::conflict(
            message,
            json!({
                "proposal": waits.proposal,
                "waits_on": number,
                "readiness": waits.readiness,
            }),
        ));
    }
    Ok(Ready {
        environment: review.view.environment.id.clone(),
    })
}

/// Save or Deploy of a [`Ready`] draft ends its included proposals: what they
/// brought is the draft's own now. True when it had any.
pub(crate) fn consume(tx: &mut dyn Tx, ready: Ready) -> Result<bool, RpcError> {
    end_included(tx, &ready.environment)
}

/// A whole Discard ends the draft's included proposals. True when it had any.
pub(crate) fn forget_included(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<bool, RpcError> {
    end_included(tx, environment)
}

fn end_included(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<bool, RpcError> {
    // The arrivals are the draft's own now. One a later Sync refreshed isn't that
    // Sync's alone: no Undo of it after.
    tx.execute(
        "UPDATE config_sync_arrival SET proposal_id = NULL, source = NULL, \
         sync_id = CASE WHEN proposal_id IN (SELECT id FROM config_proposal \
         WHERE environment_id = ?1 AND first_sync <> last_sync) THEN NULL ELSE sync_id END \
         WHERE environment_id = ?1 AND proposal_id IN (SELECT id FROM config_proposal \
         WHERE environment_id = ?1 AND offered IS NULL)",
        &[environment.as_str().into()],
    )?;
    let ended = tx.execute(
        "DELETE FROM config_proposal WHERE environment_id = ?1 AND offered IS NULL",
        &[environment.as_str().into()],
    )?;
    Ok(ended > 0)
}
