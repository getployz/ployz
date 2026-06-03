use std::cmp;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use ployz::PrimitiveFailure;
use ployz::operation::{
    OperationFailureCode, OperationId, OperationKind, OperationLease, OperationLeaseEpoch,
    OperationLedgerPort, OperationOwner, OperationProgressEvent, OperationStage,
    OperationSubmission, OperationSubmitOutcome, OperationTerminal,
};

use super::registry::OperationRegistry;
use super::registry::{OperationScript, OperationScriptStep};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OperationLeasePolicy {
    lease_ttl: Duration,
}

impl OperationLeasePolicy {
    #[cfg(test)]
    pub(crate) fn new(lease_ttl: Duration) -> Result<Self, PrimitiveFailure> {
        if lease_ttl.is_zero() {
            return Err(PrimitiveFailure::MalformedPayload);
        }
        Ok(Self { lease_ttl })
    }

    #[must_use]
    pub(crate) fn default() -> Self {
        Self {
            lease_ttl: Duration::from_secs(60),
        }
    }
}

#[derive(Clone)]
pub(crate) struct OperationRunner<L> {
    ledger: L,
    registry: OperationRegistry,
    owner: OperationOwner,
    lease_policy: OperationLeasePolicy,
    shutting_down: Arc<AtomicBool>,
    last_failure: Arc<Mutex<Option<OperationRunnerFailure>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperationRunnerFailure {
    operation: OperationId,
    source: PrimitiveFailure,
}

impl OperationRunnerFailure {
    #[must_use]
    pub(crate) fn operation(&self) -> &OperationId {
        &self.operation
    }

    #[must_use]
    pub(crate) fn source(&self) -> &PrimitiveFailure {
        &self.source
    }
}

impl<L> OperationRunner<L>
where
    L: OperationLedgerPort + Clone + Send + Sync + 'static,
{
    #[must_use]
    pub(crate) fn new(
        ledger: L,
        registry: OperationRegistry,
        owner: OperationOwner,
        lease_policy: OperationLeasePolicy,
        shutting_down: Arc<AtomicBool>,
    ) -> Self {
        Self {
            ledger,
            registry,
            owner,
            lease_policy,
            shutting_down,
            last_failure: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) async fn submit(
        &self,
        submission: OperationSubmission,
    ) -> Result<OperationId, PrimitiveFailure> {
        self.ensure_not_shutting_down()?;
        let implementation = self
            .registry
            .get(submission.kind())
            .ok_or(PrimitiveFailure::OperationStateConflict)?;
        let now = SystemTime::now();
        let epoch = OperationLeaseEpoch::new(1)?;
        let lease = self.lease(epoch, now)?;
        let initial_stage = OperationStage::parse("submitted")?;
        let submit = self
            .ledger
            .submit(&submission, &lease, &initial_stage, now)
            .await?;
        let operation = submit.record().operation().clone();
        if submit.outcome() == OperationSubmitOutcome::Created {
            let runner = self.clone();
            let script = implementation.script(&submission);
            let submitted = submission.clone();
            tokio::spawn(async move {
                if let Err(source) = runner.run_script(submitted.clone(), epoch, script).await {
                    if runner.is_shutting_down() {
                        return;
                    }
                    runner.record_failure(submitted.operation(), source);
                    if let Err(interrupt) = runner
                        .interrupt_if_authorized(submitted.operation(), epoch, SystemTime::now())
                        .await
                    {
                        runner.record_failure(submitted.operation(), interrupt);
                    }
                }
            });
        }
        Ok(operation)
    }

    #[must_use]
    pub(crate) fn supports(&self, kind: &OperationKind) -> bool {
        self.registry.contains(kind)
    }

    #[must_use]
    pub(crate) fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
    }

    #[must_use]
    pub(crate) fn last_failure(&self) -> Option<OperationRunnerFailure> {
        self.last_failure
            .lock()
            .ok()
            .and_then(|failure| failure.clone())
    }

    async fn run_script(
        &self,
        submission: OperationSubmission,
        epoch: OperationLeaseEpoch,
        script: OperationScript,
    ) -> Result<(), PrimitiveFailure> {
        for step in script.steps() {
            self.ensure_not_shutting_down()?;
            match step {
                OperationScriptStep::Stage(stage) => {
                    self.refresh_and_append_stage(
                        submission.operation(),
                        stage.clone(),
                        epoch,
                        SystemTime::now(),
                    )
                    .await?;
                }
                OperationScriptStep::Delay(duration) => {
                    self.sleep_with_lease_refresh(submission.operation(), epoch, *duration)
                        .await?;
                }
            }
        }
        self.ensure_not_shutting_down()?;
        self.ledger
            .write_terminal(
                submission.operation(),
                script.terminal(),
                &self.owner,
                epoch,
                SystemTime::now(),
            )
            .await?;
        Ok(())
    }

    async fn sleep_with_lease_refresh(
        &self,
        operation: &OperationId,
        epoch: OperationLeaseEpoch,
        duration: Duration,
    ) -> Result<(), PrimitiveFailure> {
        let mut remaining = duration;
        while !remaining.is_zero() {
            let sleep_for = cmp::min(remaining, self.renewal_interval());
            tokio::time::sleep(sleep_for).await;
            remaining = remaining.saturating_sub(sleep_for);
            self.ensure_not_shutting_down()?;
            self.refresh_lease(operation, epoch, SystemTime::now())
                .await?;
        }
        Ok(())
    }

    async fn refresh_and_append_stage(
        &self,
        operation: &OperationId,
        stage: OperationStage,
        epoch: OperationLeaseEpoch,
        now: SystemTime,
    ) -> Result<(), PrimitiveFailure> {
        self.ensure_not_shutting_down()?;
        self.refresh_lease(operation, epoch, now).await?;
        self.ledger
            .append_event_kind(
                operation,
                &OperationProgressEvent::stage(stage),
                &self.owner,
                epoch,
                now,
            )
            .await
            .map(|_| ())
    }

    async fn refresh_lease(
        &self,
        operation: &OperationId,
        epoch: OperationLeaseEpoch,
        now: SystemTime,
    ) -> Result<(), PrimitiveFailure> {
        self.ensure_not_shutting_down()?;
        let lease = self.lease(epoch, now)?;
        self.ledger
            .refresh_lease(operation, &self.owner, epoch, &lease)
            .await
    }

    async fn interrupt_if_authorized(
        &self,
        operation: &OperationId,
        epoch: OperationLeaseEpoch,
        now: SystemTime,
    ) -> Result<(), PrimitiveFailure> {
        self.ensure_not_shutting_down()?;
        self.ledger
            .write_terminal(
                operation,
                &OperationTerminal::interrupted(OperationFailureCode::parse("runner-error")?),
                &self.owner,
                epoch,
                now,
            )
            .await
            .map(|_| ())
    }

    #[must_use]
    fn renewal_interval(&self) -> Duration {
        cmp::max(self.lease_policy.lease_ttl / 2, Duration::from_millis(1))
    }

    fn ensure_not_shutting_down(&self) -> Result<(), PrimitiveFailure> {
        if self.is_shutting_down() {
            Err(PrimitiveFailure::OperationInterrupted)
        } else {
            Ok(())
        }
    }

    fn record_failure(&self, operation: &OperationId, source: PrimitiveFailure) {
        if let Ok(mut failure) = self.last_failure.lock() {
            *failure = Some(OperationRunnerFailure {
                operation: operation.clone(),
                source,
            });
        }
    }

    fn lease(
        &self,
        epoch: OperationLeaseEpoch,
        renewed_at: SystemTime,
    ) -> Result<OperationLease, PrimitiveFailure> {
        let expires_at = renewed_at
            .checked_add(self.lease_policy.lease_ttl)
            .ok_or(PrimitiveFailure::MalformedPayload)?;
        OperationLease::new(self.owner.clone(), epoch, renewed_at, expires_at)
    }
}
