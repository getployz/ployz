use std::{collections::BTreeMap, num::NonZeroU64};

use ployz_core::{
    CreateVolumeRequest, DockerVolume, DockerVolumeName, DockerVolumeStorageObservation, MachineId,
    MachineName, MachineObservation, PartialResult, ProvisionedVolumeMaximumBytes, RpcError,
    RpcErrorCode, ServiceVolume, VolumeInventory, VolumeSource,
};
use serde::Serialize;
use thiserror::Error;

/// A positive Provisioned Volume bound accepted by Docker's Ployz driver.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProvisionedVolumeSize {
    option: String,
    bytes: u64,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum ProvisionedVolumeSizeError {
    #[error("invalid Volume size {0:?}; use a positive integer followed by k, m, g, or t")]
    Invalid(String),
    #[error("Volume size {0:?} overflows bytes")]
    Overflow(String),
}

impl ProvisionedVolumeSize {
    /// Parses a positive integer followed by a binary `k`, `m`, `g`, or `t` suffix.
    ///
    /// # Errors
    ///
    /// Returns an error for any other spelling, zero, or a byte count above `u64`.
    pub(crate) fn parse(value: &str) -> Result<Self, ProvisionedVolumeSizeError> {
        let invalid = || ProvisionedVolumeSizeError::Invalid(value.to_owned());
        let (amount, multiplier) = match value.as_bytes().last() {
            Some(b'k') => (&value[..value.len() - 1], 1024_u64),
            Some(b'm') => (&value[..value.len() - 1], 1024_u64.pow(2)),
            Some(b'g') => (&value[..value.len() - 1], 1024_u64.pow(3)),
            Some(b't') => (&value[..value.len() - 1], 1024_u64.pow(4)),
            _ => return Err(invalid()),
        };
        let amount = amount.parse::<NonZeroU64>().map_err(|_| invalid())?.get();
        let bytes = amount
            .checked_mul(multiplier)
            .ok_or_else(|| ProvisionedVolumeSizeError::Overflow(value.to_owned()))?;
        Ok(Self {
            option: value.to_owned(),
            bytes,
        })
    }

    /// Builds the exact internal Docker-driver form of an SDK byte bound.
    #[must_use]
    pub(crate) fn from_maximum_bytes(maximum_bytes: ProvisionedVolumeMaximumBytes) -> Self {
        Self {
            option: format!("{}b", maximum_bytes.get()),
            bytes: maximum_bytes.get(),
        }
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.option
    }

    #[must_use]
    pub(crate) fn bytes(&self) -> NonZeroU64 {
        NonZeroU64::new(self.bytes).expect("ProvisionedVolumeSize is positive")
    }

    #[must_use]
    pub(crate) fn matches(&self, volume: &DockerVolume) -> bool {
        matches!(
            volume.storage,
            DockerVolumeStorageObservation::Provisioned { bound_bytes, .. }
                if bound_bytes.get() == self.bytes
        )
    }
}

/// Builds Docker's named-Volume creation request from a Service mount.
///
/// # Errors
///
/// Returns `InvalidArgument` when the mount does not use a named Docker Volume.
pub(crate) fn create_volume_request(
    volume: &ServiceVolume,
    provisioned: Option<&ProvisionedVolumeSize>,
) -> Result<CreateVolumeRequest, RpcError> {
    let VolumeSource::Named {
        name,
        driver,
        labels,
        ..
    } = &volume.source
    else {
        return Err(RpcError {
            code: RpcErrorCode::InvalidArgument,
            message: "volume creation requires a named Docker Volume".into(),
            details: serde_json::Value::Null,
        });
    };
    let mut request = CreateVolumeRequest {
        name: name.clone(),
        driver: driver
            .as_ref()
            .map_or_else(|| "local".into(), |driver| driver.name.clone()),
        options: driver
            .as_ref()
            .map_or_else(Default::default, |driver| driver.options.clone()),
        labels: labels.clone(),
    };
    if let Some(size) = provisioned {
        request.driver = "ployz".into();
        request.options = BTreeMap::from([("size".into(), size.as_str().into())]);
    }
    Ok(request)
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AssignmentError {
    #[error("expected KEY=VALUE, got {0:?}")]
    MissingDelimiter(String),
    #[error("assignment key cannot be empty")]
    EmptyKey,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MachineVolume {
    pub machine_name: MachineName,
    pub volume: DockerVolume,
}

#[must_use]
pub fn filter_volumes(volumes: &[MachineVolume], names: &[DockerVolumeName]) -> Vec<MachineVolume> {
    volumes
        .iter()
        .filter(|volume| names.is_empty() || names.contains(&volume.volume.id.name))
        .cloned()
        .collect()
}

#[must_use]
pub fn machine_volumes(
    machines: &[MachineObservation],
    result: &PartialResult<VolumeInventory, RpcError>,
) -> Vec<MachineVolume> {
    let names = machines
        .iter()
        .map(|machine| (machine.machine.id, machine.machine.name.clone()))
        .collect::<BTreeMap<MachineId, MachineName>>();
    result
        .successes
        .iter()
        .flat_map(|success| {
            let machine_name = names
                .get(&success.machine_id)
                .cloned()
                .expect("Volume result target came from the Machine snapshot");
            success
                .value
                .volumes
                .iter()
                .cloned()
                .map(move |volume| MachineVolume {
                    machine_name: machine_name.clone(),
                    volume,
                })
        })
        .collect()
}

pub fn parse_assignments<'a>(
    values: impl IntoIterator<Item = &'a str>,
) -> Result<BTreeMap<String, String>, AssignmentError> {
    values
        .into_iter()
        .map(|value| {
            let (key, value) = value
                .split_once('=')
                .ok_or_else(|| AssignmentError::MissingDelimiter(value.to_owned()))?;
            if key.is_empty() {
                return Err(AssignmentError::EmptyKey);
            }
            Ok((key.into(), value.into()))
        })
        .collect()
}
