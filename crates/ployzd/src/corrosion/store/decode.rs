//! Pure admission of replicated row identities and observation documents.

use super::{Error, ReplicatedObservations};
use ployz_core::{ContainerId, ContainerObservation, DockerVolume, DockerVolumeId, Machine};
use serde::de::DeserializeOwned;
use serde_json::Value;

pub(super) const INCOMPLETE_JSON_DOCUMENT: &str = "{}";

pub(super) fn is_incomplete_document(encoded: &str) -> bool {
    encoded == INCOMPLETE_JSON_DOCUMENT
}

fn decode_json_document<T: DeserializeOwned>(encoded: &str) -> Result<Option<T>, Error> {
    if is_incomplete_document(encoded) {
        Ok(None)
    } else {
        Ok(Some(serde_json::from_str(encoded)?))
    }
}

pub(super) fn decode_machine(id: &str, encoded: &str) -> Result<Option<Machine>, Error> {
    let machine: Option<Machine> = decode_json_document(encoded)?;
    if let Some(machine) = &machine
        && machine.id.as_str() != id
    {
        return Err(Error::Protocol(format!(
            "Machine row {id} contains document for {}",
            machine.id
        )));
    }
    Ok(machine)
}

pub(super) fn decode_container(
    id: &ContainerId,
    machine_id: &str,
    encoded: &str,
) -> Result<Option<ContainerObservation>, Error> {
    let observation: Option<ContainerObservation> = decode_json_document(encoded)?;
    if let Some(observation) = &observation
        && (observation.container_id != *id || observation.machine_id.as_str() != machine_id)
    {
        return Err(Error::Protocol(format!(
            "Container row {id} on Machine {machine_id} contains document for {} on Machine {}",
            observation.container_id, observation.machine_id
        )));
    }
    Ok(observation)
}

pub(super) fn decode_volume(
    id: &DockerVolumeId,
    encoded: &str,
) -> Result<Option<DockerVolume>, Error> {
    let volume: Option<DockerVolume> = decode_json_document(encoded)?;
    if let Some(volume) = &volume
        && volume.id != *id
    {
        return Err(Error::Protocol(format!(
            "Docker Volume row {}/{} contains document for {}/{}",
            id.machine_id, id.name, volume.id.machine_id, volume.id.name
        )));
    }
    Ok(volume)
}

pub(super) fn id_and_json<Id>(
    rows: Vec<[Value; 2]>,
    parse_id: impl Fn(&str) -> Result<Id, Error>,
) -> Result<Vec<(Id, String)>, Error> {
    rows.into_iter()
        .map(|[id, encoded]| {
            Ok((
                parse_id(text(&id, "row ID")?)?,
                text(&encoded, "replicated JSON")?.to_owned(),
            ))
        })
        .collect()
}

pub(super) fn decode_observations<T, Id>(
    rows: Vec<(Id, String)>,
    decode: impl Fn(&Id, &str) -> Result<Option<T>, Error>,
) -> Result<ReplicatedObservations<T, Id>, Error> {
    let mut observations = Vec::new();
    let mut incomplete_ids = Vec::new();
    for (id, encoded) in rows {
        match decode(&id, &encoded)? {
            Some(observation) => observations.push(observation),
            None => incomplete_ids.push(id),
        }
    }
    Ok(ReplicatedObservations {
        observations,
        incomplete_ids,
    })
}

pub(super) fn text<'row>(value: &'row Value, field: &str) -> Result<&'row str, Error> {
    value
        .as_str()
        .ok_or_else(|| Error::Protocol(format!("invalid {field}")))
}

pub(super) fn actor_id(value: &Value) -> Result<String, Error> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Array(bytes) => bytes
            .iter()
            .map(|byte| {
                byte.as_u64()
                    .and_then(|byte| u8::try_from(byte).ok())
                    .map(|byte| format!("{byte:02x}"))
                    .ok_or_else(|| Error::Protocol("invalid actor ID byte".into()))
            })
            .collect(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => {
            Err(Error::Protocol("invalid actor ID".into()))
        }
    }
}
