//! Prepared deployment execution and its owned progress stream.
use super::{Prep, PreparedDeploy, RunningDeploy, Session, closed};
use crate::deploy::DeployPreview;
use ployz_core::{DeployEvent, DeployOutcome, ExecutionError, RpcError};
use std::ops::Deref;
use tokio::sync::{Mutex, mpsc};

/// Who runs Image Cleanup for a confirmed Deploy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ImageCleanup {
    /// Clean up after the Outcome, before the Deploy finishes.
    #[default]
    Auto,
    /// The caller runs [`Session::prune_images`] with the prune targets.
    Manual,
}

impl std::str::FromStr for ImageCleanup {
    type Err = RpcError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::Auto),
            "manual" => Ok(Self::Manual),
            other => Err(RpcError::invalid_argument(format!(
                "imageCleanup must be \"auto\" or \"manual\", not {other:?}"
            ))),
        }
    }
}

impl Session {
    /// Remove superseded build images from each target Machine. Per-Machine failures
    /// are results, never errors.
    ///
    /// # Errors
    /// Returns when the session is closed.
    pub async fn prune_images(
        &self,
        targets: &[ployz_core::PruneTarget],
    ) -> Result<ployz_core::ImageCleanupReport, RpcError> {
        let client = self.client()?;
        self.until_closed(async { Ok(crate::image::prune_images(&client, targets).await) })
            .await
    }
}

impl PreparedDeploy {
    /// Private completed Build evidence for a later preparation; not runtime truth.
    #[must_use]
    pub fn build_receipts(
        &self,
    ) -> &std::collections::BTreeMap<ployz_core::ServiceName, super::preparation::BuildReceipt>
    {
        &self.build_receipts
    }

    /// Informational preview; execution remains bound to this prepared handle.
    #[must_use]
    pub fn preview(&self) -> &DeployPreview {
        self.preview.preview()
    }

    /// True when this preview planned no operations.
    #[must_use]
    pub fn noop(&self) -> bool {
        self.preview.noop()
    }

    /// Machines and repositories whose superseded build images Image Cleanup removes.
    #[must_use]
    pub fn prune_targets(&self) -> &[ployz_core::PruneTarget] {
        &self.prune_targets
    }

    /// Release an unconfirmed preparation and its retained images.
    pub fn close(&self) {
        let mut prep = self.prep.lock().expect("prepared deploy lock");
        if matches!(*prep, Prep::Open(_)) {
            *prep = Prep::Closed;
        }
    }

    /// Execute these operations. Illegal once this preview confirmed or closed.
    ///
    /// # Errors
    ///
    /// Returns when this preview already confirmed or closed, or when the session is closed.
    pub fn confirm(&self) -> Result<RunningDeploy, RpcError> {
        self.confirm_with_log_id(None, ImageCleanup::Auto)
    }

    /// Execute with caller-owned log correlation, without changing the planned configuration.
    /// Automatic Image Cleanup runs after the Outcome, whatever it is, and its report is
    /// the last event; it never changes the Outcome.
    /// # Errors
    /// Returns the same admission and session errors as confirm.
    pub fn confirm_with_log_id(
        &self,
        deployment_id: Option<ployz_core::DeploymentLogId>,
        cleanup: ImageCleanup,
    ) -> Result<RunningDeploy, RpcError> {
        self.confirm_approved(
            deployment_id,
            cleanup,
            crate::deploy::RuleProofs::default(),
            Vec::new(),
        )
    }

    /// As [`Self::confirm_with_log_id`], handing every Server the signed versions of each
    /// Service, or each Service's proof under an automatic deploy rule. A Server creates
    /// no Container that reaches data without one.
    /// # Errors
    /// Returns the same admission and session errors as confirm.
    pub(crate) fn confirm_approved(
        &self,
        deployment_id: Option<ployz_core::DeploymentLogId>,
        cleanup: ImageCleanup,
        rules: crate::deploy::RuleProofs,
        versions: Vec<ployz_core::authority::Signed<ployz_core::authority::ServiceVersion>>,
    ) -> Result<RunningDeploy, RpcError> {
        let session = Session {
            inner: self.session.upgrade().ok_or_else(closed)?,
        };
        if session.inner.cancel.is_cancelled() {
            return Err(closed());
        }
        let client = session.client()?;
        let retained = self.prep.lock().expect("prepared deploy lock").confirm()?;
        let cancel = session.inner.cancel.child_token();
        let (tx, rx) = mpsc::unbounded_channel();
        let free = retained
            .iter()
            .filter_map(|built| Some((built.name.clone(), built.free.clone()?)))
            .collect();
        let client = crate::deploy::Approved::new(
            client,
            crate::deploy::Approvals::signed(versions, rules).with_free(free),
        );
        let preview = self.preview.clone();
        let token = cancel.clone();
        let session_cancel = session.inner.cancel.clone();
        let prune_targets = match cleanup {
            ImageCleanup::Auto => self.prune_targets.clone(),
            ImageCleanup::Manual => Vec::new(),
        };
        let join = tokio::spawn(async move {
            let _retained = retained;
            let events = tx.clone();
            let outcome = tokio::select! {
                biased;
                () = session_cancel.cancelled() => return Err(RpcError::unavailable("session closed; in-flight Deploy outcome may be uncertain")),
                outcome = client.confirm(&preview, deployment_id, &token, Some(tx)) => outcome,
            };
            if !token.is_cancelled() {
                session_cancel
                    .run_until_cancelled(clean_up(&client.client, &prune_targets, &events))
                    .await;
            }
            Ok(outcome)
        });
        Ok(RunningDeploy {
            cancel,
            events: Mutex::new(Some(rx)),
            join: Mutex::new(Some(join)),
        })
    }
}

impl Prep {
    /// Confirm an open preparation, handing over the images it holds.
    fn confirm(&mut self) -> Result<Vec<crate::build::BuiltService>, RpcError> {
        match std::mem::replace(self, Self::Confirmed) {
            Self::Open(retained) => Ok(retained),
            Self::Confirmed => Err(RpcError::invalid_argument(
                "this Deploy Preview already confirmed",
            )),
            Self::Closed => {
                *self = Self::Closed;
                Err(RpcError::invalid_argument("this Deploy Preview was closed"))
            }
        }
    }
}

/// Automatic Image Cleanup: report it as the last event, after the Outcome.
async fn clean_up(
    client: &crate::connect::Client,
    targets: &[ployz_core::PruneTarget],
    events: &mpsc::UnboundedSender<DeployEvent>,
) {
    if targets.is_empty() {
        return;
    }
    let report = crate::image::prune_images(client, targets).await;
    let _ = events.send(DeployEvent::ImagesPruned { report });
}

impl Deref for PreparedDeploy {
    type Target = DeployPreview;

    fn deref(&self) -> &Self::Target {
        &self.preview
    }
}

impl RunningDeploy {
    /// Cancel this Deploy. The outcome is a failed Deploy with `cancelled`.
    pub fn abort(&self) {
        self.cancel.cancel();
    }

    /// Next progress or outcome event, or `None` when the stream ended.
    pub async fn next(&self) -> Option<DeployEvent> {
        let mut guard = self.events.lock().await;
        let rx = guard.as_mut()?;
        if let Some(event) = rx.recv().await {
            return Some(event);
        }
        *guard = None;
        None
    }

    /// Wait for the Deploy Outcome. Progress events are still produced.
    ///
    /// # Errors
    /// Returns unavailable when session closure interrupts execution; mutations may have completed.
    /// Rejects a second await.
    pub async fn finished(&self) -> Result<DeployOutcome<ExecutionError>, RpcError> {
        self.join_if_pending()
            .await?
            .ok_or_else(|| RpcError::invalid_argument("Deploy outcome already awaited"))
    }

    /// Join the task if the caller still owns its handle.
    pub(super) async fn join_if_pending(
        &self,
    ) -> Result<Option<DeployOutcome<ExecutionError>>, RpcError> {
        let Some(handle) = self.join.lock().await.take() else {
            return Ok(None);
        };
        let outcome = handle
            .await
            .map_err(|_| RpcError::internal("Deploy task failed"))?;
        while self.next().await.is_some() {}
        outcome.map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::RpcErrorCode;

    #[tokio::test]
    async fn a_second_finished_is_a_typed_error() {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(tx);
        let running = RunningDeploy {
            cancel: tokio_util::sync::CancellationToken::new(),
            events: Mutex::new(Some(rx)),
            join: Mutex::new(Some(tokio::spawn(async {
                Ok(DeployOutcome::Success {
                    completed: Vec::new(),
                    services: Vec::new(),
                })
            }))),
        };
        assert!(running.finished().await.is_ok());
        let second = running.finished().await.unwrap_err();
        assert_eq!(second.code, RpcErrorCode::InvalidArgument);
    }
}
