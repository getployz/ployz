//! Data Loss: one named thing an operation will destroy.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{DockerVolumeId, RpcError, RpcErrorCode};

/// One named thing an operation will destroy.
///
/// Identity is per kind: a Docker Volume carries its Machine together with its
/// name. A kind cannot be paired with an identity that does not belong to it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DataLoss {
    /// A Docker Volume identified by its Machine together with its name.
    DockerVolume(DockerVolumeId),
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
}
