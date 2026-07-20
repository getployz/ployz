use super::super::*;

/// Durable evidence for an applied volume handoff. The executable plan keeps
/// the same participants inside [`DeployServiceWork::VolumeHandoff`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployVolumeHandoffPlan {
    pub machine_id: MachineId,
    pub volume_names: NonEmptyVolumeNames,
    pub superseded: NonEmptyVolumeHandoffParticipants,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployVolumeHandoffParticipant {
    pub target: DeployCleanupContainer,
    pub prior_state: DeployVolumeHandoffPriorState,
    pub shared_volume_names: NonEmptyVolumeNames,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(
    try_from = "Vec<DeployVolumeHandoffParticipant>",
    into = "Vec<DeployVolumeHandoffParticipant>"
)]
pub struct NonEmptyVolumeHandoffParticipants(Vec<DeployVolumeHandoffParticipant>);

impl NonEmptyVolumeHandoffParticipants {
    pub fn try_new(
        participants: impl IntoIterator<Item = DeployVolumeHandoffParticipant>,
    ) -> Result<Self, EmptyVolumeHandoffParticipantsError> {
        let participants = participants.into_iter().collect::<Vec<_>>();
        if participants.is_empty() {
            return Err(EmptyVolumeHandoffParticipantsError);
        }
        Ok(Self(participants))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[DeployVolumeHandoffParticipant] {
        &self.0
    }
}

impl TryFrom<Vec<DeployVolumeHandoffParticipant>> for NonEmptyVolumeHandoffParticipants {
    type Error = EmptyVolumeHandoffParticipantsError;

    fn try_from(value: Vec<DeployVolumeHandoffParticipant>) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<NonEmptyVolumeHandoffParticipants> for Vec<DeployVolumeHandoffParticipant> {
    fn from(value: NonEmptyVolumeHandoffParticipants) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("volume handoff work must contain at least one participant")]
pub struct EmptyVolumeHandoffParticipantsError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(try_from = "Vec<VolumeName>", into = "Vec<VolumeName>")]
pub struct NonEmptyVolumeNames(Vec<VolumeName>);

impl NonEmptyVolumeNames {
    pub fn try_new(
        names: impl IntoIterator<Item = VolumeName>,
    ) -> Result<Self, EmptyVolumeNamesError> {
        let mut names = names.into_iter().collect::<Vec<_>>();
        names.sort();
        names.dedup();
        if names.is_empty() {
            return Err(EmptyVolumeNamesError);
        }
        Ok(Self(names))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[VolumeName] {
        &self.0
    }
}

impl TryFrom<Vec<VolumeName>> for NonEmptyVolumeNames {
    type Error = EmptyVolumeNamesError;

    fn try_from(value: Vec<VolumeName>) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<NonEmptyVolumeNames> for Vec<VolumeName> {
    fn from(value: NonEmptyVolumeNames) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("volume handoff participant must share at least one named volume")]
pub struct EmptyVolumeNamesError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DeployVolumeHandoffPriorState {
    Running,
    Stopped,
}
