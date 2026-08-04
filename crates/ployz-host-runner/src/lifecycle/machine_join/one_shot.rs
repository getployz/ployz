//! Transport-neutral sequencing for one-shot machine join.

use ployz_core::operation::FailureMessage;

use super::JoinToken;

/// The one-shot join input retained by Host Runner until execution finishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneShotMachineJoinPlan {
    token: JoinToken,
}

impl OneShotMachineJoinPlan {
    #[must_use]
    pub const fn new(token: JoinToken) -> Self {
        Self { token }
    }

    #[must_use]
    pub const fn token(&self) -> &JoinToken {
        &self.token
    }
}

/// The three boundaries a one-shot join crosses in strict order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OneShotMachineJoinStep {
    Redeem,
    StageArtifacts,
    Activate,
}

/// A typed terminal failure from one one-shot join attempt.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OneShotMachineJoinFailure {
    #[error("machine join redemption failed: {message}")]
    Redeem { message: FailureMessage },
    #[error("machine join artifact staging failed: {message}")]
    StageArtifacts { message: FailureMessage },
    #[error("machine join activation failed: {message}")]
    Activate { message: FailureMessage },
}

impl OneShotMachineJoinFailure {
    #[must_use]
    pub const fn step(&self) -> OneShotMachineJoinStep {
        match self {
            Self::Redeem { .. } => OneShotMachineJoinStep::Redeem,
            Self::StageArtifacts { .. } => OneShotMachineJoinStep::StageArtifacts,
            Self::Activate { .. } => OneShotMachineJoinStep::Activate,
        }
    }
}

/// Integrates the join door and machine-local activation without coupling Host
/// Runner to a control-plane transport or a concrete join-material schema.
///
/// The accepted and staged values make the phase order explicit: activation
/// cannot run without both successful redemption and artifact staging.
pub trait OneShotMachineJoinEffects {
    type AcceptedJoin;
    type StagedArtifacts;

    fn redeem_join_token(
        &mut self,
        token: &JoinToken,
    ) -> Result<Self::AcceptedJoin, FailureMessage>;

    fn stage_content_addressed_artifacts(
        &mut self,
        accepted: &Self::AcceptedJoin,
    ) -> Result<Self::StagedArtifacts, FailureMessage>;

    fn activate_join(
        &mut self,
        accepted: Self::AcceptedJoin,
        staged: Self::StagedArtifacts,
    ) -> Result<(), FailureMessage>;
}

/// Executes exactly one join attempt. Callers own operation evidence and the
/// concrete door transport; Host Runner owns only this bounded ordering seam.
pub fn execute_one_shot_machine_join(
    plan: OneShotMachineJoinPlan,
    effects: &mut impl OneShotMachineJoinEffects,
) -> Result<(), OneShotMachineJoinFailure> {
    let accepted = effects
        .redeem_join_token(plan.token())
        .map_err(|message| OneShotMachineJoinFailure::Redeem { message })?;
    let staged = effects
        .stage_content_addressed_artifacts(&accepted)
        .map_err(|message| OneShotMachineJoinFailure::StageArtifacts { message })?;
    effects
        .activate_join(accepted, staged)
        .map_err(|message| OneShotMachineJoinFailure::Activate { message })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FailAt {
        Never,
        Redeem,
        StageArtifacts,
        Activate,
    }

    struct RecordingEffects {
        fail_at: FailAt,
        calls: Vec<&'static str>,
    }

    impl RecordingEffects {
        fn new(fail_at: FailAt) -> Self {
            Self {
                fail_at,
                calls: Vec::new(),
            }
        }
    }

    impl OneShotMachineJoinEffects for RecordingEffects {
        type AcceptedJoin = &'static str;
        type StagedArtifacts = &'static str;

        fn redeem_join_token(
            &mut self,
            token: &JoinToken,
        ) -> Result<Self::AcceptedJoin, FailureMessage> {
            assert_eq!(token.as_str(), "pzjoin_secret");
            self.calls.push("redeem");
            if self.fail_at == FailAt::Redeem {
                return Err(failure("door refused token"));
            }
            Ok("accepted")
        }

        fn stage_content_addressed_artifacts(
            &mut self,
            accepted: &Self::AcceptedJoin,
        ) -> Result<Self::StagedArtifacts, FailureMessage> {
            assert_eq!(*accepted, "accepted");
            self.calls.push("stage");
            if self.fail_at == FailAt::StageArtifacts {
                return Err(failure("artifact hash mismatch"));
            }
            Ok("staged")
        }

        fn activate_join(
            &mut self,
            accepted: Self::AcceptedJoin,
            staged: Self::StagedArtifacts,
        ) -> Result<(), FailureMessage> {
            assert_eq!(accepted, "accepted");
            assert_eq!(staged, "staged");
            self.calls.push("activate");
            if self.fail_at == FailAt::Activate {
                return Err(failure("local activation failed"));
            }
            Ok(())
        }
    }

    #[test]
    fn one_shot_join_redeems_stages_then_activates() {
        let mut effects = RecordingEffects::new(FailAt::Never);

        execute_one_shot_machine_join(plan(), &mut effects).expect("join succeeds");

        assert_eq!(effects.calls, ["redeem", "stage", "activate"]);
    }

    #[test]
    fn redemption_failure_performs_no_local_work() {
        let mut effects = RecordingEffects::new(FailAt::Redeem);

        let error =
            execute_one_shot_machine_join(plan(), &mut effects).expect_err("redemption must fail");

        assert_eq!(error.step(), OneShotMachineJoinStep::Redeem);
        assert_eq!(effects.calls, ["redeem"]);
    }

    #[test]
    fn staging_failure_never_activates_partial_join_material() {
        let mut effects = RecordingEffects::new(FailAt::StageArtifacts);

        let error =
            execute_one_shot_machine_join(plan(), &mut effects).expect_err("staging must fail");

        assert_eq!(error.step(), OneShotMachineJoinStep::StageArtifacts);
        assert_eq!(effects.calls, ["redeem", "stage"]);
    }

    #[test]
    fn activation_failure_is_typed_after_successful_staging() {
        let mut effects = RecordingEffects::new(FailAt::Activate);

        let error =
            execute_one_shot_machine_join(plan(), &mut effects).expect_err("activation must fail");

        assert_eq!(error.step(), OneShotMachineJoinStep::Activate);
        assert_eq!(effects.calls, ["redeem", "stage", "activate"]);
    }

    fn plan() -> OneShotMachineJoinPlan {
        OneShotMachineJoinPlan::new(JoinToken::try_new("pzjoin_secret").expect("join token"))
    }

    fn failure(message: &str) -> FailureMessage {
        FailureMessage::try_new(message).expect("failure message")
    }
}
