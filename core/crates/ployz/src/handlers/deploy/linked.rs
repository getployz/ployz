//! A Deploy this CLI can't sign waits on a person's passkey. Cloud answers it with a
//! proposal and the link where someone approves it; at a terminal the deploy follows that
//! proposal until it becomes its Deployment, is denied, or [`WAIT`] runs out. The
//! proposal outlives the wait.

use std::time::Duration;

use ployz_core::RpcErrorCode;
use ployz_store::{DeploymentId, DeploymentQuery, DeploymentSummary, Trusted};
use reqwest::Method;
use serde::Deserialize;

use super::super::store::{Backend, Store};
use super::{Error, FOLLOW_POLL};
use crate::cloud::account::{self, Credential, StoreCallError, StoreClient};
use crate::cloud::http::Via;
use crate::coded::Coded;

/// How long a deploy at a terminal waits for someone to approve it.
const WAIT: Duration = Duration::from_secs(10 * 60);

/// Whether a deploy Cloud answered with a link waits for the approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Approval {
    Waits,
    /// Under `--json` or `--detach`, or from a CLI that holds a key: Cloud's answer as it is.
    AtOnce,
}

impl Approval {
    pub(super) const fn of(json: bool, detach: bool, keyed: bool) -> Self {
        if json || detach || keyed {
            Self::AtOnce
        } else {
            Self::Waits
        }
    }
}

/// Cloud's answer to a Deploy that waits on a person.
#[derive(Debug, Deserialize)]
struct Linked {
    proposal_id: String,
    url: String,
}

impl Linked {
    fn of(error: &StoreCallError) -> Option<Self> {
        let StoreCallError::Refused(refused) = error else {
            return None;
        };
        (refused.code == RpcErrorCode::Conflict)
            .then(|| serde_json::from_value(refused.details.clone()).ok())
            .flatten()
    }

    fn not_yet(&self, error: Error) -> StoreCallError {
        let error = error
            .with_detail("proposal_id", self.proposal_id.clone().into())
            .with_detail("url", self.url.clone().into());
        StoreCallError::Stopped(error)
    }

    /// What the command answers once the wait ends with `last`, and `lost` when Cloud
    /// stopped answering first.
    fn ended(
        &self,
        last: Stand,
        lost: Option<StoreCallError>,
    ) -> Result<DeploymentSummary, StoreCallError> {
        let left = self.left(&last);
        match (last, lost) {
            (Stand::Admitted(deployment), _) => Ok(*deployment),
            (Stand::Denied(reason), _) => Err(StoreCallError::Stopped(denied(reason))),
            (_, None) => Err(self.not_yet(Error::conflict(left))),
            (_, Some(error)) => Err(self.not_yet(Error::from(error).context(left))),
        }
    }

    /// Where a deploy the wait left behind stands, and its link.
    fn left(&self, last: &Stand) -> String {
        match last {
            Stand::Approved => format!("Approved at {}, but not started yet.", self.url),
            Stand::Waiting | Stand::Denied(_) | Stand::Admitted(_) => format!(
                "Nothing deployed yet. It waits for approval at {}.",
                self.url
            ),
        }
    }
}

/// Where a proposal stands, as the last look Cloud answered read it.
#[derive(Debug)]
enum Stand {
    Waiting,
    /// Approved, and the Store hasn't admitted its Deployment yet.
    Approved,
    Denied(Option<String>),
    Admitted(Box<DeploymentSummary>),
}

impl Stand {
    const fn open(&self) -> bool {
        matches!(self, Self::Waiting | Self::Approved)
    }
}

/// The Deployment `id` once someone approves the proposal Cloud answered `refused` with.
/// Any other refusal comes back as it came; a link `approval` doesn't wait for comes back
/// as the command's failure, whose next step is the link.
pub(super) fn admitted(
    store: &Store,
    refused: StoreCallError,
    id: &DeploymentId,
    approval: Approval,
) -> Result<DeploymentSummary, StoreCallError> {
    let Some(linked) = Linked::of(&refused) else {
        return Err(refused);
    };
    let (Backend::Cloud(runtime, credential), Approval::Waits) = (store.backend(), approval) else {
        return Err(StoreCallError::Stopped(Error::from(refused)));
    };
    crate::ui::note(format_args!("Approve this deploy at {}", linked.url));
    crate::ui::note_inline("Waiting for approval… ");
    let interrupted = crate::cancellation::interrupted()
        .map_err(|error| StoreCallError::Stopped(Error::from(error)))?;
    let mut last = Stand::Waiting;
    let ended = runtime.block_on(async {
        tokio::select! {
            () = interrupted.cancelled() => None,
            lost = settled(WAIT, &mut last, async |last| stand(credential, &linked, id, last).await) => Some(lost),
        }
    });
    crate::ui::note(match &last {
        Stand::Admitted(_) => "approved.",
        Stand::Denied(_) => "denied.",
        Stand::Waiting | Stand::Approved => "",
    });
    match ended {
        Some(lost) => linked.ended(last, lost),
        None => Err(linked.not_yet(Error::conflict(linked.left(&last)).interrupted())),
    }
}

fn denied(reason: Option<String>) -> Error {
    let message = match reason.as_deref().map(str::trim) {
        Some(reason) if !reason.is_empty() => {
            let stop = if reason.ends_with('.') { "" } else { "." };
            format!("Denied: {reason}{stop} Nothing deployed.")
        }
        _ => "Denied. Nothing deployed.".to_owned(),
    };
    Error::conflict(message)
}

/// Look until the proposal is admitted or denied, or `within` passes, keeping in `last`
/// what the last answered look read. A look Cloud can't answer for now is tried again,
/// and the deadline cuts a look short as it does the pause between looks. Returns the
/// failure that ended the wait, or the last one when Cloud stopped answering.
async fn settled(
    within: Duration,
    last: &mut Stand,
    mut look: impl AsyncFnMut(&Stand) -> Result<Stand, StoreCallError>,
) -> Option<StoreCallError> {
    let until = tokio::time::Instant::now() + within;
    let mut lost = None;
    while tokio::time::Instant::now() < until {
        let Ok(looked) = tokio::time::timeout_at(until, look(last)).await else {
            break;
        };
        match looked {
            Ok(stand) => {
                // An approval is followed at once by a read of its Deployment.
                let approved = matches!(stand, Stand::Approved) && !matches!(last, Stand::Approved);
                *last = stand;
                lost = None;
                if !last.open() {
                    return None;
                }
                if approved {
                    continue;
                }
            }
            Err(error) if temporary(&error) => lost = Some(error),
            Err(error) => return Some(error),
        }
        tokio::time::sleep_until(until.min(tokio::time::Instant::now() + FOLLOW_POLL)).await;
    }
    lost
}

/// Whether Cloud may answer the same look if asked again: it was unreachable, slow or busy.
fn temporary(error: &StoreCallError) -> bool {
    match error {
        StoreCallError::Refused(refused) => refused.code == RpcErrorCode::Unavailable,
        StoreCallError::Cloud(error) => error.code() == RpcErrorCode::Unavailable,
        StoreCallError::Stopped(_) => false,
    }
}

/// One look past `last`: the proposal's decision, and once it is approved, first its
/// Deployment, which the Store admits.
async fn stand(
    credential: &Credential,
    linked: &Linked,
    id: &DeploymentId,
    last: &Stand,
) -> Result<Stand, StoreCallError> {
    #[derive(Deserialize)]
    struct Read {
        proposal: Proposal,
    }
    #[derive(Deserialize)]
    struct Proposal {
        status: Status,
        reason: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Status {
        Approved,
        Denied,
        #[serde(other)]
        Waiting,
    }
    if matches!(last, Stand::Approved) {
        let query = DeploymentQuery { id: id.clone() };
        match StoreClient::Cloud(credential, Via::Cli)
            .read(&query, &Trusted::default())
            .await
        {
            Ok(view) => return Ok(Stand::Admitted(Box::new(view.deployment))),
            // Not admitted yet, or denied when its step couldn't start: the proposal says.
            Err(StoreCallError::Refused(refused)) if refused.code == RpcErrorCode::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let path = format!("proposals/{}", linked.proposal_id);
    let read: Read = account::refusable(credential, Method::GET, &path, None).await?;
    Ok(match read.proposal.status {
        Status::Waiting => Stand::Waiting,
        Status::Denied => Stand::Denied(read.proposal.reason),
        Status::Approved => Stand::Approved,
    })
}

#[cfg(test)]
#[path = "linked_tests.rs"]
mod tests;
