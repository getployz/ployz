//! Pure admission of replicated row identities and observation documents.

use std::fmt::Display;

use super::{Error, IngressRouteAck, IngressRouteRow, ReplicatedObservations};
use ployz_core::{
    ContainerId, ContainerObservation, DockerVolume, DockerVolumeId, Machine, MachineId,
    QualifiedService,
};
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

/// What a reader does with a row whose document will not decode, chosen once for each
/// kind of table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RowPolicy {
    /// The read fails, so a row that cannot be understood is never read as absent.
    FailRead,
    /// The row is logged by its key and left out, so one bad row hides no other.
    Skip { table: &'static str },
}

pub(super) fn decode_observations<T, Id: Display, Doc>(
    rows: Vec<(Id, Doc)>,
    policy: RowPolicy,
    decode: impl Fn(&Id, &Doc) -> Result<Option<T>, Error>,
) -> Result<ReplicatedObservations<T, Id>, Error> {
    let mut observations = Vec::new();
    let mut incomplete_ids = Vec::new();
    for (id, encoded) in rows {
        match decode(&id, &encoded) {
            Ok(Some(observation)) => observations.push(observation),
            Ok(None) => incomplete_ids.push(id),
            Err(error) => match policy {
                RowPolicy::FailRead => return Err(error),
                RowPolicy::Skip { table } => tracing::warn!(
                    %id,
                    error = %ployz_core::error_chain::inline(&error),
                    "skipping {table} row"
                ),
            },
        }
    }
    Ok(ReplicatedObservations {
        observations,
        incomplete_ids,
    })
}

/// An ingress route row a reader left out, named by what its key columns held.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct Skipped {
    table: &'static str,
    key: String,
}

impl Skipped {
    pub(super) fn log(&self) {
        tracing::warn!(key = %self.key, "skipping {} row", self.table);
    }
}

/// One `ingress_routes` row. A key that is not a Machine and a Service leaves the row
/// out; a `containers` column that is not a set of containers is a partial row.
pub(super) fn decode_ingress_route(
    machine_id: &Value,
    service_id: &Value,
    containers: &Value,
) -> Result<((MachineId, QualifiedService), IngressRouteRow), Skipped> {
    let key = || {
        let part = |value: &Value| {
            value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned)
        };
        format!("{}/{}", part(machine_id), part(service_id))
    };
    let skipped = || Skipped {
        table: "ingress route",
        key: key(),
    };
    let machine_id = machine_id
        .as_str()
        .and_then(|machine_id| MachineId::parse(machine_id).ok())
        .ok_or_else(skipped)?;
    let service = service_id
        .as_str()
        .and_then(|service| QualifiedService::parse(service).ok())
        .ok_or_else(skipped)?;
    let row = match containers
        .as_str()
        .and_then(|containers| serde_json::from_str(containers).ok())
    {
        Some(containers) => IngressRouteRow::Acked(IngressRouteAck { containers }),
        None => IngressRouteRow::UnknownContainers,
    };
    Ok(((machine_id, service), row))
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn decode(id: &u8, encoded: &&str) -> Result<Option<String>, Error> {
        match *encoded {
            "{}" => Ok(None),
            "bad" => Err(Error::Protocol(format!("row {id} is bad"))),
            encoded => Ok(Some(format!("{id}:{encoded}"))),
        }
    }

    #[test]
    fn a_fail_read_table_fails_on_the_first_row_it_cannot_decode() {
        let rows = vec![(1, "a"), (2, "bad"), (3, "{}")];
        let error = decode_observations(rows, RowPolicy::FailRead, decode).unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid Corrosion protocol response: row 2 is bad"
        );
    }

    #[test]
    fn a_skip_table_leaves_out_a_row_it_cannot_decode_and_names_an_incomplete_one() {
        let rows = vec![(1, "a"), (2, "bad"), (3, "{}"), (4, "d")];
        let read = decode_observations(rows, RowPolicy::Skip { table: "test" }, decode).unwrap();
        assert_eq!(read.observations, ["1:a", "4:d"]);
        assert_eq!(read.incomplete_ids, [3]);
    }

    #[test]
    fn an_ingress_route_is_acked_partial_or_skipped_by_name() {
        let machine = "a".repeat(32);
        let container = "c".repeat(64);
        let acked = decode_ingress_route(
            &json!(machine),
            &json!("app/api"),
            &json!(format!("[\"{container}\"]")),
        )
        .unwrap();
        assert_eq!(
            acked.1,
            IngressRouteRow::Acked(IngressRouteAck {
                containers: [ContainerId::parse(container).unwrap()].into()
            })
        );
        let partial =
            decode_ingress_route(&json!(machine), &json!("app/api"), &json!("\"partial\""))
                .unwrap();
        assert_eq!(partial.1, IngressRouteRow::UnknownContainers);
        let null = decode_ingress_route(&json!(machine), &json!("app/api"), &Value::Null).unwrap();
        assert_eq!(null.1, IngressRouteRow::UnknownContainers);
        assert_eq!(
            decode_ingress_route(&json!("not-an-id"), &json!("app/api"), &json!("[]")),
            Err(Skipped {
                table: "ingress route",
                key: "not-an-id/app/api".to_owned()
            })
        );
        assert_eq!(
            decode_ingress_route(&json!(machine), &json!(7), &json!("[]")),
            Err(Skipped {
                table: "ingress route",
                key: format!("{machine}/7")
            })
        );
    }
}
