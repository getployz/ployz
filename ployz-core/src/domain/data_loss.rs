//! Data Loss: one named thing an operation will destroy.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::PartialResult;
use crate::{DockerVolumeId, LocalMachineRemoved, ProjectName, RpcError, RpcErrorCode};

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

/// Live Observation of Data Loss. Not a complete Cluster view.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObservedDataLoss {
    pub data_loss: Vec<DataLoss>,
}

/// Exact Data Loss identities approved from one Live Observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DataLossConfirmation {
    confirmed: Vec<DataLoss>,
}

impl ObservedDataLoss {
    /// Confirm every observed Data Loss whose display name was supplied.
    ///
    /// Names are intentionally set-valued: one Docker Volume name confirms
    /// every observed Machine-local Volume with that name. Extra names are
    /// ignored so a retry can include Data Loss that has already disappeared.
    ///
    /// # Errors
    ///
    /// Returns [`UnconfirmedDataLoss`] when the names do not cover every
    /// identity in this observation.
    pub fn confirm_names<'name>(
        &self,
        names: impl IntoIterator<Item = &'name str>,
    ) -> Result<DataLossConfirmation, UnconfirmedDataLoss> {
        let names: Vec<_> = names.into_iter().collect();
        let mut confirmed = Vec::new();
        for loss in &self.data_loss {
            if names.contains(&loss.name()) && !confirmed.contains(loss) {
                confirmed.push(loss.clone());
            }
        }
        let confirmation = DataLossConfirmation { confirmed };
        self.require(&confirmation)?;
        Ok(confirmation)
    }

    /// Require this observation to be covered by exact confirmed identities.
    ///
    /// # Errors
    ///
    /// Returns [`UnconfirmedDataLoss`] with identities that were not confirmed.
    pub fn require(&self, confirmation: &DataLossConfirmation) -> Result<(), UnconfirmedDataLoss> {
        let missing: Vec<_> = self
            .data_loss
            .iter()
            .filter(|loss| !confirmation.confirmed.contains(loss))
            .cloned()
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(UnconfirmedDataLoss { missing })
        }
    }
}

/// Data Loss a confirmation did not name. The execute-time refusal payload.
#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
#[error(
    "Data Loss is not covered by the confirmation: {missing}",
    missing = .missing.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
)]
pub struct UnconfirmedDataLoss {
    pub missing: Vec<DataLoss>,
}

impl UnconfirmedDataLoss {
    /// Refusal when execute-time Data Loss is not covered by the confirmation.
    #[must_use]
    pub fn into_rpc_error(self) -> RpcError {
        RpcError {
            code: RpcErrorCode::InvalidArgument,
            message: self.to_string(),
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

/// Partial Result of destroying one Cluster.
///
/// Projects and Machines that completed are named. Unreachable Machines stay
/// in `machines.failures` rather than being omitted. `pairing_revoked` is
/// independent of Machine reset so a repeated attempt can finish leftover work
/// over Dial after Register is already closed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClusterTeardown {
    pub destroyed_projects: Vec<ProjectName>,
    pub machines: PartialResult<LocalMachineRemoved, RpcError>,
    pub pairing_revoked: bool,
}
