use std::future::Future;
use std::pin::Pin;

use mvp_bus::FactKey;
use mvp_commands::{
    Command, CommandCompensationFuture, CommandContext, CommandName, CommandStepFuture, IntentId,
    PhaseTransition, PhasedCommand, run_phased,
};
use mvp_identity::VisibleNodes;
use mvp_projection::FactSource;
use mvp_routing::{ProjectionCatchUp, RoutingError, ServingCommitPlan, ServingFactWriter};
use serde::{Deserialize, Serialize};

use crate::{
    EnvironmentBranchFact, EnvironmentBranchId, EnvironmentCommandId, EnvironmentEpoch,
    EnvironmentError, EnvironmentHeadCandidate, EnvironmentHeadFact, EnvironmentId,
    EnvironmentPromoteDecisionFact, EnvironmentResult, EnvironmentRollbackDecisionFact,
    EnvironmentRouteRef, EnvironmentVolumeRef, environment_branch_fact_key,
    environment_branch_fact_payload, environment_head_fact_key, environment_head_fact_payload,
    environment_promote_decision_fact_key, environment_promote_decision_fact_payload,
    environment_rollback_decision_fact_key, environment_rollback_decision_fact_payload,
    require_expected_environment_epoch,
};
use mvp_bus::BusSession;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenEnvironmentFact {
    pub key: FactKey,
}

pub trait EnvironmentFactWriter: Send + Sync {
    fn write_head<'a>(
        &'a self,
        fact: EnvironmentHeadFact,
    ) -> Pin<Box<dyn Future<Output = EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>>;

    fn write_branch<'a>(
        &'a self,
        fact: EnvironmentBranchFact,
    ) -> Pin<Box<dyn Future<Output = EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>>;

    fn write_promote_decision<'a>(
        &'a self,
        fact: EnvironmentPromoteDecisionFact,
    ) -> Pin<Box<dyn Future<Output = EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>>;

    fn write_rollback_decision<'a>(
        &'a self,
        fact: EnvironmentRollbackDecisionFact,
    ) -> Pin<Box<dyn Future<Output = EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>>;
}

pub trait EnvironmentVolumeForkParticipant: Send + Sync {
    fn fork_volume<'a>(
        &'a self,
        request: EnvironmentVolumeForkRequest,
    ) -> Pin<Box<dyn Future<Output = EnvironmentResult<EnvironmentVolumeForkReply>> + Send + 'a>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentVolumeForkRequest {
    pub command_id: EnvironmentCommandId,
    pub source_environment: EnvironmentId,
    pub branch_environment: EnvironmentId,
    pub source_volume: EnvironmentVolumeRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentVolumeForkReply {
    pub evidence: EnvironmentVolumeForkEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentVolumeForkEvidence {
    pub command_id: EnvironmentCommandId,
    pub source_environment: EnvironmentId,
    pub branch_environment: EnvironmentId,
    pub source_volume: EnvironmentVolumeRef,
    pub forked_volume: EnvironmentVolumeRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchEnvironmentRequest {
    pub command_id: EnvironmentCommandId,
    pub source_environment: EnvironmentId,
    pub expected_source_epoch: EnvironmentEpoch,
    pub branch_id: EnvironmentBranchId,
    pub branch_environment: EnvironmentId,
    pub branch_head: EnvironmentHeadFact,
    pub route_refs: Vec<EnvironmentRouteRef>,
    pub visible_nodes: VisibleNodes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchEnvironmentResult {
    pub branch: EnvironmentBranchFact,
    pub head: EnvironmentHeadFact,
    pub visible_nodes: VisibleNodes,
}

pub struct BranchEnvironmentCommand<'a, W, P> {
    source: &'a dyn FactSource,
    session: &'a BusSession,
    fact_writer: &'a W,
    fork_participant: &'a P,
}

impl<'a, W, P> BranchEnvironmentCommand<'a, W, P>
where
    W: EnvironmentFactWriter,
    P: EnvironmentVolumeForkParticipant,
{
    #[must_use]
    pub fn new(
        source: &'a dyn FactSource,
        session: &'a BusSession,
        fact_writer: &'a W,
        fork_participant: &'a P,
    ) -> Self {
        Self {
            source,
            session,
            fact_writer,
            fork_participant,
        }
    }

    pub async fn execute(
        &self,
        request: BranchEnvironmentRequest,
    ) -> EnvironmentResult<BranchEnvironmentResult> {
        let source_head = require_expected_environment_epoch(
            self.source,
            self.session,
            &request.source_environment,
            request.expected_source_epoch,
        )?;
        let mut forked_volume_refs = Vec::with_capacity(source_head.fact.volume_refs.len());
        for source_volume in &source_head.fact.volume_refs {
            let fork_request = EnvironmentVolumeForkRequest {
                command_id: request.command_id.clone(),
                source_environment: request.source_environment.clone(),
                branch_environment: request.branch_environment.clone(),
                source_volume: source_volume.clone(),
            };
            let reply = self
                .fork_participant
                .fork_volume(fork_request.clone())
                .await?;
            validate_fork_evidence(&fork_request, &reply.evidence)?;
            forked_volume_refs.push(reply.evidence.forked_volume);
        }
        let current = require_expected_environment_epoch(
            self.source,
            self.session,
            &request.source_environment,
            request.expected_source_epoch,
        )?;
        if !same_head_candidate(&current, &source_head) {
            return Err(EnvironmentError::StaleExpectedEpoch {
                environment: request.source_environment,
                expected: request.expected_source_epoch,
                actual: Some(current.fact.epoch),
            });
        }
        if request.branch_head.environment != request.branch_environment
            || request.branch_head.volume_refs != forked_volume_refs
            || request.branch_head.source_branch_id != Some(request.branch_id.clone())
        {
            return Err(EnvironmentError::DecisionHeadMismatch {
                key: environment_head_fact_key(
                    &request.branch_head.environment,
                    request.branch_head.epoch,
                )?,
            });
        }
        let branch = EnvironmentBranchFact {
            source_environment: request.source_environment,
            source_epoch: source_head.fact.epoch,
            source_head_id: source_head.fact.head_id,
            branch_id: request.branch_id,
            branch_environment: request.branch_environment,
            route_refs: request.route_refs,
            forked_volume_refs,
            visible_nodes: request.visible_nodes.clone(),
        };
        self.fact_writer.write_branch(branch.clone()).await?;
        self.fact_writer
            .write_head(request.branch_head.clone())
            .await?;
        Ok(BranchEnvironmentResult {
            branch,
            head: request.branch_head,
            visible_nodes: request.visible_nodes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromoteEnvironmentRequest {
    pub command_id: EnvironmentCommandId,
    pub environment: EnvironmentId,
    pub expected_environment_epoch: EnvironmentEpoch,
    pub branch_environment: EnvironmentId,
    pub expected_branch_epoch: EnvironmentEpoch,
    pub serving_commit: ServingCommitPlan,
    pub visible_nodes: VisibleNodes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEnvironmentPromote {
    decision: EnvironmentPromoteDecisionFact,
}

impl PendingEnvironmentPromote {
    #[must_use]
    pub fn decision(&self) -> &EnvironmentPromoteDecisionFact {
        &self.decision
    }
}

pub struct PromoteEnvironmentCommand<'a, W, S> {
    source: &'a dyn FactSource,
    session: &'a BusSession,
    fact_writer: &'a W,
    serving_writer: &'a S,
}

impl<'a, W, S> PromoteEnvironmentCommand<'a, W, S>
where
    W: EnvironmentFactWriter,
    S: ServingFactWriter,
{
    #[must_use]
    pub fn new(
        source: &'a dyn FactSource,
        session: &'a BusSession,
        fact_writer: &'a W,
        serving_writer: &'a S,
    ) -> Self {
        Self {
            source,
            session,
            fact_writer,
            serving_writer,
        }
    }

    pub async fn begin(
        &self,
        request: PromoteEnvironmentRequest,
    ) -> EnvironmentResult<PendingEnvironmentPromote> {
        let decision = self.prepare_promote_decision(&request)?;
        self.fact_writer
            .write_promote_decision(decision.clone())
            .await?;
        self.serving_writer
            .write_serving_commit(&request.serving_commit)
            .await
            .map_err(map_serving_error)?;
        Ok(PendingEnvironmentPromote { decision })
    }

    fn prepare_promote_decision(
        &self,
        request: &PromoteEnvironmentRequest,
    ) -> EnvironmentResult<EnvironmentPromoteDecisionFact> {
        let production = require_expected_environment_epoch(
            self.source,
            self.session,
            &request.environment,
            request.expected_environment_epoch,
        )?;
        let branch = require_expected_environment_epoch(
            self.source,
            self.session,
            &request.branch_environment,
            request.expected_branch_epoch,
        )?;
        let decision = EnvironmentPromoteDecisionFact {
            environment: request.environment.clone(),
            command_id: request.command_id.clone(),
            previous_head: production.fact.reference(),
            branch_head: branch.fact.reference(),
            target_serving_commit_id: request.serving_commit.serving_commit_id.clone(),
            target_volume_refs: branch.fact.volume_refs.clone(),
            visible_nodes: request.visible_nodes.clone(),
        };
        let current_production = require_expected_environment_epoch(
            self.source,
            self.session,
            &request.environment,
            request.expected_environment_epoch,
        )?;
        reject_if_head_changed(
            &request.environment,
            request.expected_environment_epoch,
            &production,
            &current_production,
        )?;
        let current_branch = require_expected_environment_epoch(
            self.source,
            self.session,
            &request.branch_environment,
            request.expected_branch_epoch,
        )?;
        reject_if_head_changed(
            &request.branch_environment,
            request.expected_branch_epoch,
            &branch,
            &current_branch,
        )?;
        Ok(decision)
    }

    pub async fn execute_phased(
        &self,
        cx: &CommandContext,
        request: PromoteEnvironmentRequest,
        catch_up: Option<ProjectionCatchUp>,
    ) -> EnvironmentResult<EnvironmentCommandResult> {
        run_phased(
            cx,
            &PromoteEnvironmentPhasedCommand {
                owner: self,
                request,
                catch_up,
            },
        )
        .await
    }

    pub async fn finalize(
        &self,
        pending: PendingEnvironmentPromote,
        catch_up: Option<ProjectionCatchUp>,
    ) -> EnvironmentResult<EnvironmentCommandResult> {
        self.finalize_promote_decision(pending.decision, catch_up)
            .await
    }

    async fn finalize_promote_decision(
        &self,
        decision: EnvironmentPromoteDecisionFact,
        catch_up: Option<ProjectionCatchUp>,
    ) -> EnvironmentResult<EnvironmentCommandResult> {
        let Some(catch_up) = catch_up else {
            return Ok(EnvironmentCommandResult::Pending {
                reason: EnvironmentServingPendingReason::ProjectionCatchUpMissing,
            });
        };
        if catch_up.serving_commit_id() != &decision.target_serving_commit_id {
            return Ok(EnvironmentCommandResult::Pending {
                reason: EnvironmentServingPendingReason::ProjectionCatchUpMismatch,
            });
        }
        let head = EnvironmentHeadFact {
            environment: decision.environment,
            epoch: decision.previous_head.epoch.next()?,
            head_id: environment_head_id_for_command(&decision.command_id),
            source_command_id: decision.command_id,
            serving_commit_id: decision.target_serving_commit_id,
            previous_head: Some(decision.previous_head),
            volume_refs: decision.target_volume_refs,
            source_branch_id: None,
        };
        self.fact_writer.write_head(head.clone()).await?;
        Ok(EnvironmentCommandResult::Complete {
            head: Box::new(head),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum PromoteEnvironmentPhase {
    Start,
    DecisionWritten {
        decision: Box<EnvironmentPromoteDecisionFact>,
        serving_commit: Box<ServingCommitPlan>,
    },
    ServingCommitWritten {
        decision: Box<EnvironmentPromoteDecisionFact>,
    },
    HeadWritten {
        head: Box<EnvironmentHeadFact>,
    },
}

struct PromoteEnvironmentPhasedCommand<'a, W, S> {
    owner: &'a PromoteEnvironmentCommand<'a, W, S>,
    request: PromoteEnvironmentRequest,
    catch_up: Option<ProjectionCatchUp>,
}

impl<W, S> Command for PromoteEnvironmentPhasedCommand<'_, W, S>
where
    W: EnvironmentFactWriter,
    S: ServingFactWriter,
{
    type Output = EnvironmentCommandResult;
    type Error = EnvironmentError;

    fn name(&self) -> CommandName {
        CommandName::parse("environment-promote").expect("static command name validates")
    }

    fn intent_id(&self) -> IntentId {
        IntentId::parse(self.request.command_id.as_str()).expect("command ids validate as intents")
    }
}

impl<W, S> PhasedCommand for PromoteEnvironmentPhasedCommand<'_, W, S>
where
    W: EnvironmentFactWriter,
    S: ServingFactWriter,
{
    type Phase = PromoteEnvironmentPhase;

    fn initial_phase(&self) -> Self::Phase {
        PromoteEnvironmentPhase::Start
    }

    fn step<'b>(
        &'b self,
        _cx: &'b CommandContext,
        phase: Self::Phase,
    ) -> CommandStepFuture<'b, Self::Phase, Self::Output, <Self as Command>::Error> {
        Box::pin(async move {
            match phase {
                PromoteEnvironmentPhase::Start => {
                    let decision = self.owner.prepare_promote_decision(&self.request)?;
                    self.owner
                        .fact_writer
                        .write_promote_decision(decision.clone())
                        .await?;
                    Ok(PhaseTransition::Continue(
                        PromoteEnvironmentPhase::DecisionWritten {
                            decision: Box::new(decision),
                            serving_commit: Box::new(self.request.serving_commit.clone()),
                        },
                    ))
                }
                PromoteEnvironmentPhase::DecisionWritten {
                    decision,
                    serving_commit,
                } => {
                    self.owner
                        .serving_writer
                        .write_serving_commit(&serving_commit)
                        .await
                        .map_err(map_serving_error)?;
                    Ok(PhaseTransition::Continue(
                        PromoteEnvironmentPhase::ServingCommitWritten { decision },
                    ))
                }
                PromoteEnvironmentPhase::ServingCommitWritten { decision } => {
                    match self
                        .owner
                        .finalize_promote_decision(*decision, self.catch_up.clone())
                        .await?
                    {
                        EnvironmentCommandResult::Pending { reason } => {
                            Ok(PhaseTransition::Done(EnvironmentCommandResult::Pending {
                                reason,
                            }))
                        }
                        EnvironmentCommandResult::Complete { head } => {
                            Ok(PhaseTransition::Continue(
                                PromoteEnvironmentPhase::HeadWritten { head },
                            ))
                        }
                    }
                }
                PromoteEnvironmentPhase::HeadWritten { head } => {
                    Ok(PhaseTransition::Done(EnvironmentCommandResult::Complete {
                        head,
                    }))
                }
            }
        })
    }

    fn compensate<'b>(
        &'b self,
        _cx: &'b CommandContext,
        _phase: Self::Phase,
    ) -> CommandCompensationFuture<'b, <Self as Command>::Error> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackEnvironmentRequest {
    pub command_id: EnvironmentCommandId,
    pub environment: EnvironmentId,
    pub expected_environment_epoch: EnvironmentEpoch,
    pub serving_commit: ServingCommitPlan,
    pub visible_nodes: VisibleNodes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEnvironmentRollback {
    decision: EnvironmentRollbackDecisionFact,
}

impl PendingEnvironmentRollback {
    #[must_use]
    pub fn decision(&self) -> &EnvironmentRollbackDecisionFact {
        &self.decision
    }
}

pub struct RollbackEnvironmentCommand<'a, W, S> {
    source: &'a dyn FactSource,
    session: &'a BusSession,
    fact_writer: &'a W,
    serving_writer: &'a S,
}

impl<'a, W, S> RollbackEnvironmentCommand<'a, W, S>
where
    W: EnvironmentFactWriter,
    S: ServingFactWriter,
{
    #[must_use]
    pub fn new(
        source: &'a dyn FactSource,
        session: &'a BusSession,
        fact_writer: &'a W,
        serving_writer: &'a S,
    ) -> Self {
        Self {
            source,
            session,
            fact_writer,
            serving_writer,
        }
    }

    pub async fn begin(
        &self,
        request: RollbackEnvironmentRequest,
    ) -> EnvironmentResult<PendingEnvironmentRollback> {
        let decision = self.prepare_rollback_decision(&request)?;
        self.fact_writer
            .write_rollback_decision(decision.clone())
            .await?;
        self.serving_writer
            .write_serving_commit(&request.serving_commit)
            .await
            .map_err(map_serving_error)?;
        Ok(PendingEnvironmentRollback { decision })
    }

    fn prepare_rollback_decision(
        &self,
        request: &RollbackEnvironmentRequest,
    ) -> EnvironmentResult<EnvironmentRollbackDecisionFact> {
        let current = require_expected_environment_epoch(
            self.source,
            self.session,
            &request.environment,
            request.expected_environment_epoch,
        )?;
        let current_head_key =
            environment_head_fact_key(&current.fact.environment, current.fact.epoch)?;
        let rollback_target = current.fact.previous_head.clone().ok_or_else(|| {
            EnvironmentError::RollbackTargetMissing {
                key: current_head_key.clone(),
            }
        })?;
        let decision = EnvironmentRollbackDecisionFact {
            environment: request.environment.clone(),
            command_id: request.command_id.clone(),
            current_head: current.fact.reference(),
            target_serving_commit_id: request.serving_commit.serving_commit_id.clone(),
            target_volume_refs: rollback_target.volume_refs.clone(),
            rollback_target,
            visible_nodes: request.visible_nodes.clone(),
        };
        let current_before_write = require_expected_environment_epoch(
            self.source,
            self.session,
            &request.environment,
            request.expected_environment_epoch,
        )?;
        reject_if_head_changed(
            &request.environment,
            request.expected_environment_epoch,
            &current,
            &current_before_write,
        )?;
        Ok(decision)
    }

    pub async fn execute_phased(
        &self,
        cx: &CommandContext,
        request: RollbackEnvironmentRequest,
        catch_up: Option<ProjectionCatchUp>,
    ) -> EnvironmentResult<EnvironmentCommandResult> {
        run_phased(
            cx,
            &RollbackEnvironmentPhasedCommand {
                owner: self,
                request,
                catch_up,
            },
        )
        .await
    }

    pub async fn finalize(
        &self,
        pending: PendingEnvironmentRollback,
        catch_up: Option<ProjectionCatchUp>,
    ) -> EnvironmentResult<EnvironmentCommandResult> {
        self.finalize_rollback_decision(pending.decision, catch_up)
            .await
    }

    async fn finalize_rollback_decision(
        &self,
        decision: EnvironmentRollbackDecisionFact,
        catch_up: Option<ProjectionCatchUp>,
    ) -> EnvironmentResult<EnvironmentCommandResult> {
        let Some(catch_up) = catch_up else {
            return Ok(EnvironmentCommandResult::Pending {
                reason: EnvironmentServingPendingReason::ProjectionCatchUpMissing,
            });
        };
        if catch_up.serving_commit_id() != &decision.target_serving_commit_id {
            return Ok(EnvironmentCommandResult::Pending {
                reason: EnvironmentServingPendingReason::ProjectionCatchUpMismatch,
            });
        }
        let head = EnvironmentHeadFact {
            environment: decision.environment,
            epoch: decision.current_head.epoch.next()?,
            head_id: environment_head_id_for_command(&decision.command_id),
            source_command_id: decision.command_id,
            serving_commit_id: decision.target_serving_commit_id,
            previous_head: Some(decision.rollback_target),
            volume_refs: decision.target_volume_refs,
            source_branch_id: None,
        };
        self.fact_writer.write_head(head.clone()).await?;
        Ok(EnvironmentCommandResult::Complete {
            head: Box::new(head),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum RollbackEnvironmentPhase {
    Start,
    DecisionWritten {
        decision: Box<EnvironmentRollbackDecisionFact>,
        serving_commit: Box<ServingCommitPlan>,
    },
    ServingCommitWritten {
        decision: Box<EnvironmentRollbackDecisionFact>,
    },
    HeadWritten {
        head: Box<EnvironmentHeadFact>,
    },
}

struct RollbackEnvironmentPhasedCommand<'a, W, S> {
    owner: &'a RollbackEnvironmentCommand<'a, W, S>,
    request: RollbackEnvironmentRequest,
    catch_up: Option<ProjectionCatchUp>,
}

impl<W, S> Command for RollbackEnvironmentPhasedCommand<'_, W, S>
where
    W: EnvironmentFactWriter,
    S: ServingFactWriter,
{
    type Output = EnvironmentCommandResult;
    type Error = EnvironmentError;

    fn name(&self) -> CommandName {
        CommandName::parse("environment-rollback").expect("static command name validates")
    }

    fn intent_id(&self) -> IntentId {
        IntentId::parse(self.request.command_id.as_str()).expect("command ids validate as intents")
    }
}

impl<W, S> PhasedCommand for RollbackEnvironmentPhasedCommand<'_, W, S>
where
    W: EnvironmentFactWriter,
    S: ServingFactWriter,
{
    type Phase = RollbackEnvironmentPhase;

    fn initial_phase(&self) -> Self::Phase {
        RollbackEnvironmentPhase::Start
    }

    fn step<'b>(
        &'b self,
        _cx: &'b CommandContext,
        phase: Self::Phase,
    ) -> CommandStepFuture<'b, Self::Phase, Self::Output, <Self as Command>::Error> {
        Box::pin(async move {
            match phase {
                RollbackEnvironmentPhase::Start => {
                    let decision = self.owner.prepare_rollback_decision(&self.request)?;
                    self.owner
                        .fact_writer
                        .write_rollback_decision(decision.clone())
                        .await?;
                    Ok(PhaseTransition::Continue(
                        RollbackEnvironmentPhase::DecisionWritten {
                            decision: Box::new(decision),
                            serving_commit: Box::new(self.request.serving_commit.clone()),
                        },
                    ))
                }
                RollbackEnvironmentPhase::DecisionWritten {
                    decision,
                    serving_commit,
                } => {
                    self.owner
                        .serving_writer
                        .write_serving_commit(&serving_commit)
                        .await
                        .map_err(map_serving_error)?;
                    Ok(PhaseTransition::Continue(
                        RollbackEnvironmentPhase::ServingCommitWritten { decision },
                    ))
                }
                RollbackEnvironmentPhase::ServingCommitWritten { decision } => {
                    match self
                        .owner
                        .finalize_rollback_decision(*decision, self.catch_up.clone())
                        .await?
                    {
                        EnvironmentCommandResult::Pending { reason } => {
                            Ok(PhaseTransition::Done(EnvironmentCommandResult::Pending {
                                reason,
                            }))
                        }
                        EnvironmentCommandResult::Complete { head } => {
                            Ok(PhaseTransition::Continue(
                                RollbackEnvironmentPhase::HeadWritten { head },
                            ))
                        }
                    }
                }
                RollbackEnvironmentPhase::HeadWritten { head } => {
                    Ok(PhaseTransition::Done(EnvironmentCommandResult::Complete {
                        head,
                    }))
                }
            }
        })
    }

    fn compensate<'b>(
        &'b self,
        _cx: &'b CommandContext,
        _phase: Self::Phase,
    ) -> CommandCompensationFuture<'b, <Self as Command>::Error> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentCommandResult {
    Complete {
        head: Box<EnvironmentHeadFact>,
    },
    Pending {
        reason: EnvironmentServingPendingReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentServingPendingReason {
    ProjectionCatchUpMissing,
    ProjectionCatchUpMismatch,
}

fn validate_fork_evidence(
    request: &EnvironmentVolumeForkRequest,
    evidence: &EnvironmentVolumeForkEvidence,
) -> EnvironmentResult<()> {
    if evidence.command_id == request.command_id
        && evidence.source_environment == request.source_environment
        && evidence.branch_environment == request.branch_environment
        && evidence.source_volume == request.source_volume
    {
        Ok(())
    } else {
        Err(EnvironmentError::DecisionHeadMismatch {
            key: FactKey::parse(format!(
                "/facts/environment/{}/branch/{}",
                request.source_environment.as_str(),
                request.command_id.as_str()
            ))?,
        })
    }
}

fn reject_if_head_changed(
    environment: &EnvironmentId,
    expected: EnvironmentEpoch,
    original: &EnvironmentHeadCandidate,
    current: &EnvironmentHeadCandidate,
) -> EnvironmentResult<()> {
    if same_head_candidate(original, current) {
        return Ok(());
    }
    Err(EnvironmentError::StaleExpectedEpoch {
        environment: environment.clone(),
        expected,
        actual: Some(current.fact.epoch),
    })
}

fn same_head_candidate(left: &EnvironmentHeadCandidate, right: &EnvironmentHeadCandidate) -> bool {
    left.key == right.key && left.content_hash == right.content_hash
}

fn map_serving_error(error: RoutingError) -> EnvironmentError {
    match error {
        RoutingError::ServingFactConflict { key } => EnvironmentError::ServingFactConflict { key },
        other => EnvironmentError::ServingWrite {
            message: other.to_string(),
        },
    }
}

fn environment_head_id_for_command(command_id: &EnvironmentCommandId) -> crate::EnvironmentHeadId {
    crate::EnvironmentHeadId::parse(format!("head-{}", command_id.as_str()))
        .expect("command ids validate as environment head id suffixes")
}

#[derive(Clone)]
pub struct BusEnvironmentFactWriter {
    bus: mvp_bus::BusActorHandle,
    session: BusSession,
}

impl BusEnvironmentFactWriter {
    #[must_use]
    pub fn new(bus: mvp_bus::BusActorHandle, session: BusSession) -> Self {
        Self { bus, session }
    }
}

impl EnvironmentFactWriter for BusEnvironmentFactWriter {
    fn write_head<'a>(
        &'a self,
        fact: EnvironmentHeadFact,
    ) -> Pin<Box<dyn Future<Output = EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = environment_head_fact_key(&fact.environment, fact.epoch)?;
            let payload = environment_head_fact_payload(&fact)?;
            write_environment_fact(&self.bus, &self.session, key, payload).await
        })
    }

    fn write_branch<'a>(
        &'a self,
        fact: EnvironmentBranchFact,
    ) -> Pin<Box<dyn Future<Output = EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = environment_branch_fact_key(&fact.source_environment, &fact.branch_id)?;
            let payload = environment_branch_fact_payload(&fact)?;
            write_environment_fact(&self.bus, &self.session, key, payload).await
        })
    }

    fn write_promote_decision<'a>(
        &'a self,
        fact: EnvironmentPromoteDecisionFact,
    ) -> Pin<Box<dyn Future<Output = EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = environment_promote_decision_fact_key(&fact.environment, &fact.command_id)?;
            let payload = environment_promote_decision_fact_payload(&fact)?;
            write_environment_fact(&self.bus, &self.session, key, payload).await
        })
    }

    fn write_rollback_decision<'a>(
        &'a self,
        fact: EnvironmentRollbackDecisionFact,
    ) -> Pin<Box<dyn Future<Output = EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = environment_rollback_decision_fact_key(&fact.environment, &fact.command_id)?;
            let payload = environment_rollback_decision_fact_payload(&fact)?;
            write_environment_fact(&self.bus, &self.session, key, payload).await
        })
    }
}

async fn write_environment_fact(
    bus: &mvp_bus::BusActorHandle,
    session: &BusSession,
    key: FactKey,
    payload: impl Into<mvp_bus::FactPayload>,
) -> EnvironmentResult<WrittenEnvironmentFact> {
    match bus
        .write_fact_payload(session, key.clone(), payload.into())
        .await?
    {
        mvp_bus::FactWriteOutcome::Inserted(_) | mvp_bus::FactWriteOutcome::AlreadyPresent(_) => {
            Ok(WrittenEnvironmentFact { key })
        }
        mvp_bus::FactWriteOutcome::Conflict(_) => Err(EnvironmentError::FactConflict { key }),
    }
}
