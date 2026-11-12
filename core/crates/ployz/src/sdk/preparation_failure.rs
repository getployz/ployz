//! The `details` of a Deploy that was not prepared: what failed, and whether deploying
//! the same thing again fails the same way.
//!
//! The keys are frozen wire JSON that Cloud and older or newer CLIs read, so this one type
//! owns them and `sdk_tests.rs` pins the bytes of every arm.

use ployz_core::{Evidence, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::build::RemoteBuildFailure;

/// `{"preparation": {...}, "same_again": true}`; either key is left out when it does not
/// apply, and a refusal that is only `same_again` carries no `preparation`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PreparationFailure {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preparation: Option<Preparation>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    same_again: bool,
}

impl Evidence for PreparationFailure {
    const KEY: Option<&'static str> = None;
}

impl PreparationFailure {
    pub(crate) fn of(preparation: Preparation) -> Self {
        Self {
            preparation: Some(preparation),
            same_again: false,
        }
    }

    /// A plan refusal that deploying the same thing again meets again.
    pub(crate) fn same_again() -> Self {
        Self {
            preparation: None,
            same_again: true,
        }
    }

    /// `self`, and whether deploying the same thing again fails the same way.
    #[must_use]
    pub(crate) fn again(mut self, same_again: bool) -> Self {
        self.same_again = same_again;
        self
    }

    /// The uploaded Services a new upload would build, if that is what this says.
    pub(crate) fn needs_upload(self) -> Option<Vec<ServiceName>> {
        match self.preparation? {
            Preparation::UploadNeeded { services } => Some(services),
            Preparation::Failed { .. }
            | Preparation::Unknown { .. }
            | Preparation::Cancelled { .. } => None,
        }
    }
}

/// How preparation ended, by `kind`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Preparation {
    /// The attempt ended without a Service prepared.
    Failed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage: Option<Stage>,
        message: String,
        /// Why each Build Machine was passed over; `null` when the entry said nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rejections: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        work: Option<ployz_build::WorkEvidence>,
    },
    /// The Build's termination was not confirmed.
    Unknown {
        stage: ployz_build::Stage,
        message: String,
        work: ployz_build::WorkEvidence,
    },
    /// The caller asked to stop. A Build Machine's evidence rides along when it confirmed
    /// the stop.
    Cancelled {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage: Option<ployz_build::Stage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        work: Option<ployz_build::WorkEvidence>,
    },
    /// No source and no usable image for these uploaded Services: only a new upload builds them.
    UploadNeeded { services: Vec<ServiceName> },
}

/// Where a failed preparation stopped: before a Build began, or at a phase of one.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Stage {
    Selection,
    Observation,
    #[serde(untagged)]
    Build(ployz_build::Stage),
}

impl Preparation {
    /// A Build Machine's account of its Build. A cancellation request turns only a confirmed
    /// failure into `cancelled`: it cannot erase an outcome that is unknown.
    pub(crate) fn remote_build(outcome: RemoteBuildFailure, cancellation_requested: bool) -> Self {
        match outcome {
            RemoteBuildFailure::Failed {
                stage,
                message,
                work,
            } if cancellation_requested => Self::Cancelled {
                stage: Some(stage),
                message: Some(message),
                work: Some(work),
            },
            RemoteBuildFailure::Failed {
                stage,
                message,
                work,
            } => Self::Failed {
                stage: Some(Stage::Build(stage)),
                message,
                rejections: None,
                work: Some(work),
            },
            RemoteBuildFailure::Unknown {
                stage,
                message,
                work,
            } => Self::Unknown {
                stage,
                message,
                work,
            },
        }
    }

    /// A failure before any Build Machine answered, or after none was needed.
    pub(crate) fn failed(message: String) -> Self {
        Self::Failed {
            stage: None,
            message,
            rejections: None,
            work: None,
        }
    }
}
