//! Operation event replay paging.

use serde::{Deserialize, Serialize};
use std::num::NonZeroU16;

use crate::ids::OperationId;

use super::events::OperationEvent;
use super::{EventSequence, MAX_OPERATION_EVENT_REPLAY_LIMIT};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "SafeInteger<\"OperationEventReplayLimit\">")
)]
#[serde(try_from = "u16", into = "u16")]
pub struct OperationEventReplayLimit(NonZeroU16);

impl OperationEventReplayLimit {
    pub fn try_new(value: u16) -> Result<Self, OperationEventReplayLimitError> {
        let Some(value) = NonZeroU16::new(value) else {
            return Err(OperationEventReplayLimitError::Zero);
        };
        if value.get() > MAX_OPERATION_EVENT_REPLAY_LIMIT {
            return Err(OperationEventReplayLimitError::TooLarge {
                value: value.get(),
                max: MAX_OPERATION_EVENT_REPLAY_LIMIT,
            });
        }

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }

    #[must_use]
    pub fn as_usize(self) -> usize {
        usize::from(self.get())
    }
}

impl TryFrom<u16> for OperationEventReplayLimit {
    type Error = OperationEventReplayLimitError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<OperationEventReplayLimit> for u16 {
    fn from(value: OperationEventReplayLimit) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OperationEventReplayLimitError {
    #[error("operation event replay limit must be greater than zero")]
    Zero,
    #[error("operation event replay limit {value} exceeds maximum {max}")]
    TooLarge { value: u16, max: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct OperationEventReplayRequest {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub limit: OperationEventReplayLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct OperationEventReplayPage {
    pub events: Vec<ReplayedOperationEvent>,
    pub cursor: OperationEventReplayCursor,
}

impl OperationEventReplayPage {
    #[must_use]
    pub fn caught_up(events: Vec<ReplayedOperationEvent>) -> Self {
        Self {
            events,
            cursor: OperationEventReplayCursor::CaughtUp,
        }
    }

    #[must_use]
    pub fn more(events: Vec<ReplayedOperationEvent>, next_start_sequence: EventSequence) -> Self {
        Self {
            events,
            cursor: OperationEventReplayCursor::More {
                next_start_sequence,
            },
        }
    }

    #[must_use]
    pub fn terminal(events: Vec<ReplayedOperationEvent>) -> Self {
        Self {
            events,
            cursor: OperationEventReplayCursor::Terminal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationEventReplayCursor {
    CaughtUp,
    Terminal,
    More { next_start_sequence: EventSequence },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ReplayedOperationEvent {
    pub sequence: EventSequence,
    pub event: OperationEvent,
}
