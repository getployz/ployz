//! Host preparation as ployzd records it in `prepare/state.json`, and as the check-in loop reads
//! it back.
//!
//! Each step is one object: a `state` tag beside `started_at` and `finished_at`, as ployzd has
//! always written it, so every reader of an earlier release still parses what this one writes.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned, ser::Error};
use serde_json::{Map, Value};

/// One preparation step, Docker's install or the ZFS build, with the outcome `O` it finishes on.
///
/// A step whose fields don't fit together, such as a failure with no `finished_at` or an
/// `installing` step that names one, reads as [`PrepStep::Unknown`]. So does a tag this release
/// doesn't know. Unknown keeps the object it read and writes it back unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PrepStep<O> {
    #[default]
    NotStarted,
    Running {
        started_at: String,
    },
    Finished {
        /// None for a step finished without running, as discarding a ZFS build records it.
        started_at: Option<String>,
        finished_at: String,
        outcome: O,
    },
    Unknown(Map<String, Value>),
}

impl<O> PrepStep<O> {
    #[must_use]
    pub const fn outcome(&self) -> Option<&O> {
        match self {
            Self::Finished { outcome, .. } => Some(outcome),
            Self::NotStarted | Self::Running { .. } | Self::Unknown(_) => None,
        }
    }

    /// The last time this step records, as RFC 3339 text.
    #[must_use]
    pub fn last_dated(&self) -> Option<&str> {
        match self {
            Self::Running { started_at } => Some(started_at),
            Self::Finished { finished_at, .. } => Some(finished_at),
            Self::NotStarted | Self::Unknown(_) => None,
        }
    }
}

impl<O: Outcome> PrepStep<O> {
    /// When this step failed, for a step that finished failed.
    #[must_use]
    pub fn failed_at(&self) -> Option<&str> {
        match self {
            Self::Finished {
                finished_at,
                outcome,
                ..
            } if outcome.failure().is_some() => Some(finished_at),
            Self::NotStarted | Self::Running { .. } | Self::Finished { .. } | Self::Unknown(_) => {
                None
            }
        }
    }
}

/// How a step ended.
pub trait Outcome {
    /// Why the step failed, when it did.
    fn failure(&self) -> Option<&str>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum DockerOutcome {
    Ready,
    Failed { message: String },
}

impl Outcome for DockerOutcome {
    fn failure(&self) -> Option<&str> {
        match self {
            Self::Ready => None,
            Self::Failed { message } => Some(message),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum ZfsOutcome {
    Ready,
    Failed { message: String },
    Skipped { reason: String },
    Off,
}

impl Outcome for ZfsOutcome {
    fn failure(&self) -> Option<&str> {
        match self {
            Self::Failed { message } => Some(message),
            Self::Ready | Self::Skipped { .. } | Self::Off => None,
        }
    }
}

/// A step ployzd has begun. A convention for ployzd's begin and finish pair: begin returns it and
/// finish consumes it, so that writer finishes only a step it began. The type enforces nothing
/// more: anyone can build one, or a finished step directly.
#[derive(Debug)]
#[must_use]
pub struct Begun {
    started_at: String,
}

impl Begun {
    pub const fn at(started_at: String) -> Self {
        Self { started_at }
    }

    /// The step as recorded while it runs.
    #[must_use]
    pub fn running<O>(&self) -> PrepStep<O> {
        PrepStep::Running {
            started_at: self.started_at.clone(),
        }
    }

    pub fn finish<O>(self, outcome: O, finished_at: String) -> PrepStep<O> {
        PrepStep::Finished {
            started_at: Some(self.started_at),
            finished_at,
            outcome,
        }
    }
}

const STATE: &str = "state";
const INSTALLING: &str = "installing";
const STARTED_AT: &str = "started_at";
const FINISHED_AT: &str = "finished_at";

impl<O: DeserializeOwned> PrepStep<O> {
    fn decode(fields: Map<String, Value>) -> Self {
        let time = |key| match fields.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(at)) => Ok(Some(at.clone())),
            Some(_) => Err(()),
        };
        let (Ok(started_at), Ok(finished_at)) = (time(STARTED_AT), time(FINISHED_AT)) else {
            return Self::Unknown(fields);
        };
        let installing = fields.get(STATE).and_then(Value::as_str) == Some(INSTALLING);
        match (installing, started_at, finished_at) {
            (true, None, None) => Self::NotStarted,
            (true, Some(started_at), None) => Self::Running { started_at },
            (false, started_at, Some(finished_at)) => {
                match O::deserialize(Value::Object(fields.clone())) {
                    Ok(outcome) => Self::Finished {
                        started_at,
                        finished_at,
                        outcome,
                    },
                    Err(_) => Self::Unknown(fields),
                }
            }
            (true, _, Some(_)) | (false, _, None) => Self::Unknown(fields),
        }
    }
}

impl<'de, O: DeserializeOwned> Deserialize<'de> for PrepStep<O> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Map::deserialize(deserializer).map(Self::decode)
    }
}

impl<O: Serialize> Serialize for PrepStep<O> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let installing = || Map::from_iter([(STATE.to_owned(), Value::from(INSTALLING))]);
        let (mut fields, started_at, finished_at) = match self {
            Self::NotStarted => (installing(), None, None),
            Self::Running { started_at } => (installing(), Some(started_at), None),
            Self::Finished {
                started_at,
                finished_at,
                outcome,
            } => {
                let Value::Object(fields) =
                    serde_json::to_value(outcome).map_err(S::Error::custom)?
                else {
                    return Err(S::Error::custom("a step outcome is a tagged object"));
                };
                (fields, started_at.as_ref(), Some(finished_at))
            }
            Self::Unknown(fields) => return fields.serialize(serializer),
        };
        let time = |at: Option<&String>| at.map_or(Value::Null, |at| Value::from(at.as_str()));
        fields.insert(STARTED_AT.to_owned(), time(started_at));
        fields.insert(FINISHED_AT.to_owned(), time(finished_at));
        fields.serialize(serializer)
    }
}

#[cfg(test)]
#[path = "host_preparation_tests.rs"]
mod tests;
