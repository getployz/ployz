use std::{collections::BTreeMap, future::Future, num::NonZeroU64};

use ployz_core::{
    DockerVolume, DockerVolumeId, DockerVolumeName, MachineFailure, MachineId, MachineName,
    MachineObservation, MachineSuccess, PartialResult, RpcError, RpcErrorCode,
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

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.option
    }

    #[must_use]
    pub(crate) fn matches(&self, volume: &DockerVolume) -> bool {
        volume.driver == "ployz"
            && volume.options.len() == 1
            && volume
                .options
                .get("size")
                .and_then(|size| Self::parse(size).ok())
                .is_some_and(|size| size.bytes == self.bytes)
    }
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
    result: &PartialResult<Vec<DockerVolume>, RpcError>,
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

pub async fn remove_volumes_with<F, Fut>(
    volumes: &[MachineVolume],
    force: bool,
    remove: F,
) -> PartialResult<DockerVolumeName, RpcError>
where
    F: Fn(DockerVolumeId, bool) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Result<(), RpcError>> + Send + 'static,
{
    let mut removals = tokio::task::JoinSet::new();
    for (index, volume) in volumes.iter().enumerate() {
        let id = volume.volume.id.clone();
        let remove = remove.clone();
        removals.spawn(async move {
            let outcome = remove(id.clone(), force).await;
            (index, id, outcome)
        });
    }
    let mut outcomes = Vec::with_capacity(volumes.len());
    while let Some(outcome) = removals.join_next().await {
        outcomes.push(outcome.expect("Volume removal task does not panic"));
    }
    outcomes.sort_by_key(|(index, _, _)| *index);
    let mut result = PartialResult {
        successes: Vec::new(),
        failures: Vec::new(),
        omissions: Vec::new(),
    };
    for (_, id, outcome) in outcomes {
        match outcome {
            Ok(())
            | Err(RpcError {
                code: RpcErrorCode::NotFound,
                ..
            }) => result.successes.push(MachineSuccess {
                machine_id: id.machine_id,
                value: id.name,
            }),
            Err(error) => result.failures.push(MachineFailure {
                machine_id: id.machine_id,
                error,
            }),
        }
    }
    result
}
