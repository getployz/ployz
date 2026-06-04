//! Deploy policy and planning models.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::num::NonZeroU16;

use crate::ids::{NodeId, RevisionId, ServiceId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployRequest {
    pub service_id: ServiceId,
    pub target_revision: RevisionId,
    pub image: ImageReference,
    pub replicas: ReplicaCount,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPlanningInput {
    pub request: DeployRequest,
    pub eligible_nodes: Vec<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPlan {
    pub service_id: ServiceId,
    pub target_revision: RevisionId,
    pub steps: Vec<DeployPlanStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployPlanStep {
    RunContainer { node_id: NodeId, slot: ReplicaSlot },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReplicaSlot(u16);

impl ReplicaSlot {
    pub fn try_new(value: u16) -> Result<Self, ReplicaSlotError> {
        if value == 0 {
            return Err(ReplicaSlotError::Zero);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicaSlotError {
    Zero,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployPlanError {
    NoEligibleNodes,
}

pub fn plan_new_service_deploy(input: DeployPlanningInput) -> Result<DeployPlan, DeployPlanError> {
    if input.eligible_nodes.is_empty() {
        return Err(DeployPlanError::NoEligibleNodes);
    }

    let target_replicas = usize::from(input.request.replicas.get());
    let steps = input
        .eligible_nodes
        .iter()
        .cycle()
        .take(target_replicas)
        .enumerate()
        .map(|(index, node_id)| {
            let slot = ReplicaSlot((index + 1) as u16);
            DeployPlanStep::RunContainer {
                node_id: node_id.clone(),
                slot,
            }
        })
        .collect();

    Ok(DeployPlan {
        service_id: input.request.service_id,
        target_revision: input.request.target_revision,
        steps,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ImageReference(String);

impl ImageReference {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ImageReferenceError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ImageReferenceError::Empty);
        }

        if value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(ImageReferenceError::InvalidCharacter { value });
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ImageReference {
    type Error = ImageReferenceError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ImageReference> for String {
    fn from(value: ImageReference) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageReferenceError {
    Empty,
    InvalidCharacter { value: String },
}

impl fmt::Display for ImageReferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("image reference is empty"),
            Self::InvalidCharacter { value } => {
                write!(
                    formatter,
                    "image reference contains invalid characters: {value}"
                )
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct ReplicaCount(NonZeroU16);

impl ReplicaCount {
    pub fn try_new(value: u16) -> Result<Self, ReplicaCountError> {
        let Some(value) = NonZeroU16::new(value) else {
            return Err(ReplicaCountError::Zero);
        };

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl TryFrom<u16> for ReplicaCount {
    type Error = ReplicaCountError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ReplicaCount> for u16 {
    fn from(value: ReplicaCount) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicaCountError {
    Zero,
}

impl fmt::Display for ReplicaCountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("replica count must be greater than zero"),
        }
    }
}
