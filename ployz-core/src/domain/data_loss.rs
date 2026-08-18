//! Data Loss: one named thing an operation will destroy.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{DockerVolumeId, NameMatches, RpcError, RpcErrorCode};

/// One named thing an operation will destroy.
///
/// Identity is per kind: a Docker Volume carries its Machine together with its
/// name. A kind cannot be paired with an identity that does not belong to it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DataLoss {
    /// A Docker Volume identified by its Machine together with its name.
    DockerVolume(DockerVolumeId),
}

impl DataLoss {
    /// Operator-facing display name. Not the unique identity.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::DockerVolume(id) => id.name.as_str(),
        }
    }
}

impl fmt::Display for DataLoss {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DockerVolume(id) => {
                write!(f, "{} on {}", id.name.as_str(), id.machine_id.as_str())
            }
        }
    }
}

/// A typed display name matched more than one listed Data Loss entry.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("Data Loss name {name:?} matches more than one listed entry")]
pub struct AmbiguousDataLossName {
    pub name: String,
}

/// Live Observation of Data Loss. Not a complete Cluster view.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObservedDataLoss {
    pub data_loss: Vec<DataLoss>,
}

impl ObservedDataLoss {
    /// Data Loss in this observation that `confirmation` does not name.
    ///
    /// Extra names in `confirmation` are ignored: data that already went away
    /// is not a surprise.
    #[must_use]
    pub fn uncovered_by(&self, confirmation: &[DataLoss]) -> Vec<DataLoss> {
        self.data_loss
            .iter()
            .filter(|loss| !confirmation.contains(loss))
            .cloned()
            .collect()
    }

    /// Resolve operator-typed display names against this observation.
    ///
    /// Extra names that match nothing are ignored. A name that matches more
    /// than one listed entry is refused rather than guessed.
    ///
    /// # Errors
    ///
    /// Returns [`AmbiguousDataLossName`] when a typed name matches more than
    /// one listed entry.
    pub fn named<'name>(
        &self,
        names: impl IntoIterator<Item = &'name str>,
    ) -> Result<Vec<DataLoss>, AmbiguousDataLossName> {
        let mut confirmed = Vec::new();
        for name in names {
            match NameMatches::from_matches(
                self.data_loss
                    .iter()
                    .filter(|loss| loss.name() == name)
                    .collect(),
            ) {
                NameMatches::None => {}
                NameMatches::One(loss) => confirmed.push(loss.clone()),
                NameMatches::Ambiguous(_) => {
                    return Err(AmbiguousDataLossName {
                        name: name.to_owned(),
                    });
                }
            }
        }
        Ok(confirmed)
    }
}

/// Data Loss a confirmation did not name. The execute-time refusal payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UnconfirmedDataLoss {
    pub missing: Vec<DataLoss>,
}

impl UnconfirmedDataLoss {
    /// Refusal when execute-time Data Loss is not covered by the confirmation.
    #[must_use]
    pub fn into_rpc_error(self) -> RpcError {
        RpcError {
            code: RpcErrorCode::InvalidArgument,
            message: format!(
                "Data Loss is not covered by the confirmation: {}",
                self.missing
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            details: serde_json::to_value(&self).expect("UnconfirmedDataLoss is JSON"),
        }
    }

    /// Execute-time refusal payload, when `error` carries one.
    #[must_use]
    pub fn from_rpc_error(error: &RpcError) -> Option<Self> {
        if error.code != RpcErrorCode::InvalidArgument {
            return None;
        }
        Self::deserialize(&error.details).ok()
    }
}
