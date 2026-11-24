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

/// How long a deploy at a terminal waits for someone to approve it.
const WAIT: Duration = Duration::from_secs(10 * 60);

/// Whether a deploy Cloud answered with a link waits for the approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Approval {
    Waits,
    /// Under `--json` or `--detach`, or for a deploy this CLI signed: Cloud's answer as it is.
    AtOnce,
}

impl Approval {
    pub(super) const fn of(json: bool, detach: bool, signed: bool) -> Self {
        if json || detach || signed {
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

    fn waiting(&self) -> String {
        format!(
            "Nothing deployed yet. It waits for approval at {}.",
            self.url
        )
    }
}

/// Where a proposal stands, as one look reads it.
#[derive(Debug)]
enum Stand {
    Waiting,
    Denied(Option<String>),
    Admitted(Box<DeploymentSummary>),
}

/// The Deployment `id` once someone approves the proposal Cloud answered `refused` with;
/// any other refusal, or one `approval` doesn't wait for, as it came.
pub(super) fn admitted(
    store: &Store,
    refused: StoreCallError,
    id: &DeploymentId,
    approval: Approval,
) -> Result<DeploymentSummary, StoreCallError> {
    let (Some(linked), Backend::Cloud(runtime, credential), Approval::Waits) =
        (Linked::of(&refused), store.backend(), approval)
    else {
        return Err(refused);
    };
    crate::ui::note(format_args!("Approve this deploy at {}", linked.url));
    crate::ui::note_inline("Waiting for approval… ");
    let interrupted = crate::cancellation::interrupted()
        .map_err(|error| StoreCallError::Stopped(Error::from(error)))?;
    let ended = runtime.block_on(async {
        tokio::select! {
            () = interrupted.cancelled() => None,
            ended = settled(WAIT, async || stand(credential, &linked, id).await) => Some(ended),
        }
    });
    crate::ui::note(match &ended {
        Some(Ok(Stand::Admitted(_))) => "approved.",
        Some(Ok(Stand::Denied(_))) => "denied.",
        _ => "",
    });
    match ended {
        Some(Ok(Stand::Admitted(deployment))) => Ok(*deployment),
        Some(Ok(Stand::Denied(reason))) => Err(StoreCallError::Stopped(denied(reason))),
        Some(Ok(Stand::Waiting)) => Err(linked.not_yet(Error::conflict(linked.waiting()))),
        Some(Err(error)) => Err(linked.not_yet(Error::from(error).context(linked.waiting()))),
        None => Err(linked.not_yet(Error::conflict(linked.waiting()).interrupted())),
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

/// Look until the proposal is no longer waiting, or `within` passes.
async fn settled(
    within: Duration,
    mut look: impl AsyncFnMut() -> Result<Stand, StoreCallError>,
) -> Result<Stand, StoreCallError> {
    let until = tokio::time::Instant::now() + within;
    loop {
        let stand = look().await?;
        if !matches!(stand, Stand::Waiting) || tokio::time::Instant::now() >= until {
            return Ok(stand);
        }
        tokio::time::sleep(FOLLOW_POLL).await;
    }
}

/// The proposal as Cloud has it now, and once approved, its Deployment if the Store admitted it.
async fn stand(
    credential: &Credential,
    linked: &Linked,
    id: &DeploymentId,
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
    let path = format!("proposals/{}", linked.proposal_id);
    let read: Read = account::refusable(credential, Method::GET, &path, None).await?;
    match read.proposal.status {
        Status::Waiting => Ok(Stand::Waiting),
        Status::Denied => Ok(Stand::Denied(read.proposal.reason)),
        Status::Approved => {
            let query = DeploymentQuery { id: id.clone() };
            match StoreClient::Cloud(credential, Via::Cli)
                .read(&query, &Trusted::default())
                .await
            {
                Ok(view) => Ok(Stand::Admitted(Box::new(view.deployment))),
                Err(StoreCallError::Refused(refused)) if refused.code == RpcErrorCode::NotFound => {
                    Ok(Stand::Waiting)
                }
                Err(error) => Err(error),
            }
        }
    }
}

#[cfg(test)]
#[path = "linked_tests.rs"]
mod tests;
