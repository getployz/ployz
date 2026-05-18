use std::collections::BTreeMap;

use mvp_identity::VisibleNodes;

use crate::{
    CleanupStatus, DeployCommandResult, DeployError, DeployId, DeployOutcome, DeployResult,
    DrainStatus, PhaseId, PhasePolicy, ServingCommitId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseState {
    Planned,
    Preparing,
    Ready,
    Committed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CommitBoundary {
    BeforeIrreversible,
    AfterIrreversible { phase: PhaseId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DeployLifecycle {
    Running,
    Terminal(DeployOutcome),
}

#[derive(Debug, Clone)]
pub struct DeployStateMachine {
    deploy_id: DeployId,
    phases: BTreeMap<PhaseId, PhaseState>,
    visible_nodes: VisibleNodes,
    serving_commit_id: Option<ServingCommitId>,
    commit_boundary: CommitBoundary,
    drain_status: DrainStatus,
    cleanup_status: CleanupStatus,
    lifecycle: DeployLifecycle,
}

impl DeployStateMachine {
    pub fn new(deploy_id: DeployId, phases: impl IntoIterator<Item = PhaseId>) -> Self {
        Self {
            deploy_id,
            phases: phases
                .into_iter()
                .map(|phase| (phase, PhaseState::Planned))
                .collect(),
            visible_nodes: VisibleNodes::new(Vec::new()),
            serving_commit_id: None,
            commit_boundary: CommitBoundary::BeforeIrreversible,
            drain_status: DrainStatus::NotStarted,
            cleanup_status: CleanupStatus::NotNeeded,
            lifecycle: DeployLifecycle::Running,
        }
    }

    pub fn record_visible_nodes(&mut self, visible_nodes: VisibleNodes) {
        self.visible_nodes = visible_nodes;
    }

    pub fn mark_preparing(&mut self, phase: PhaseId) -> DeployResult<()> {
        self.transition_phase(phase, PhaseState::Preparing)
    }

    pub fn mark_ready(&mut self, phase: PhaseId) -> DeployResult<()> {
        self.transition_phase(phase, PhaseState::Ready)
    }

    pub fn commit_phase(&mut self, phase: PhaseId, policy: PhasePolicy) -> DeployResult<()> {
        match self.phases.get(&phase) {
            Some(PhaseState::Ready) => {
                if policy.is_irreversible() {
                    self.commit_boundary = CommitBoundary::AfterIrreversible { phase };
                }
                self.phases.insert(phase, PhaseState::Committed);
                Ok(())
            }
            Some(_) | None => Err(DeployError::PhaseNotReady { phase }),
        }
    }

    pub fn block_after_irreversible(&mut self) -> DeployResult<()> {
        if !self.has_irreversible_commit() {
            self.lifecycle = DeployLifecycle::Terminal(DeployOutcome::FailedBeforeCommit);
            return Ok(());
        }
        self.lifecycle =
            DeployLifecycle::Terminal(DeployOutcome::DeployBlockedAfterIrreversiblePhase);
        Err(DeployError::BlockedAfterIrreversiblePhase)
    }

    #[must_use]
    pub fn has_irreversible_commit(&self) -> bool {
        matches!(
            self.commit_boundary,
            CommitBoundary::AfterIrreversible { .. }
        )
    }

    #[must_use]
    pub fn has_serving_commit(&self) -> bool {
        self.serving_commit_id.is_some()
    }

    pub fn commit_serving(&mut self, serving_commit_id: ServingCommitId) -> DeployResult<()> {
        if self.serving_commit_id.is_some() {
            return Err(DeployError::ServingCommitAlreadyExists);
        }
        self.serving_commit_id = Some(serving_commit_id);
        Ok(())
    }

    pub fn mark_drain_started(&mut self) {
        self.drain_status = DrainStatus::Started;
    }

    pub fn finish_cleanup(&mut self) -> DeployResult<DeployCommandResult> {
        if self.serving_commit_id.is_none() {
            return Err(DeployError::ServingCommitRequired);
        }
        self.drain_status = DrainStatus::Completed;
        self.cleanup_status = CleanupStatus::Done;
        self.lifecycle = DeployLifecycle::Terminal(DeployOutcome::DeployDone);
        self.result()
    }

    pub fn cleanup_pending(
        &mut self,
        cleanup_status: CleanupStatus,
    ) -> DeployResult<DeployCommandResult> {
        if self.serving_commit_id.is_none() {
            return Err(DeployError::ServingCommitRequired);
        }
        self.cleanup_status = cleanup_status;
        self.lifecycle = DeployLifecycle::Terminal(DeployOutcome::CleanupPending);
        self.result()
    }

    pub fn result(&self) -> DeployResult<DeployCommandResult> {
        let DeployLifecycle::Terminal(outcome) = &self.lifecycle else {
            return Err(DeployError::DeployStillRunning);
        };
        Ok(DeployCommandResult {
            deploy_id: self.deploy_id.clone(),
            outcome: outcome.clone(),
            visible_nodes: self.visible_nodes.clone(),
            serving_commit_id: self.serving_commit_id.clone(),
            drain_status: self.drain_status.clone(),
            cleanup_status: self.cleanup_status.clone(),
        })
    }

    fn transition_phase(&mut self, phase: PhaseId, next: PhaseState) -> DeployResult<()> {
        if !self.phases.contains_key(&phase) {
            return Err(DeployError::PhaseNotReady { phase });
        }
        self.phases.insert(phase, next);
        Ok(())
    }
}
