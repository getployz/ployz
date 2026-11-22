//! The one Docker inspect of a Container. Bollard's typed decode refuses a response
//! with a value its enums don't know, such as a newer Docker's `HostConfig.Isolation`,
//! so every inspect falls back to decoding the raw JSON into only the fields Ployz
//! reads. A Container the inventory lists can then also be started, stopped and
//! removed.

use std::collections::HashMap;

use bollard::{
    Docker,
    errors::Error as DockerError,
    models::{ContainerInspectResponse, HealthConfig},
};
use ployz_core::{ContainerId, labels};
use serde::Deserialize;

use super::{Error, Status, classify};

/// Docker's inspect of `name`, a Container ID or name, or `None` once Docker has no
/// such Container. A JSON decode failure is a [`DockerError::JsonSerdeError`].
///
/// # Errors
///
/// Returns when Docker cannot answer, or answers JSON this cannot decode either.
pub(crate) async fn inspect(client: &Docker, name: &str) -> Result<Option<Inspected>, DockerError> {
    decode(client.inspect_container(name, None).await)
}

pub(super) fn decode(
    answer: Result<ContainerInspectResponse, DockerError>,
) -> Result<Option<Inspected>, DockerError> {
    let decoded = match answer {
        Ok(typed) => Inspected::from_typed(&typed),
        Err(DockerError::JsonDataError { contents, .. }) => serde_json::from_str(&contents),
        Err(error) if classify(&error) == Status::Absent => return Ok(None),
        Err(error) => return Err(error),
    };
    decoded
        .map(Some)
        .map_err(|err| DockerError::JsonSerdeError { err })
}

/// Docker's inspect of `container_id`, an absent one as `ContainerNotFound`.
///
/// # Errors
///
/// Returns `ContainerNotFound` when Docker has no such Container, [`Error::Json`]
/// when its answer doesn't decode, and [`Error::Docker`] when Docker cannot answer.
pub(crate) async fn inspect_id(
    client: &Docker,
    container_id: &ContainerId,
) -> Result<Inspected, Error> {
    inspect(client, container_id.as_str())
        .await
        .map_err(failed)?
        .ok_or(Error::ContainerNotFound(*container_id))
}

/// A failed [`inspect`] as the module's [`Error`].
pub(crate) fn failed(error: DockerError) -> Error {
    match error {
        DockerError::JsonSerdeError { err } => Error::Json(err),
        error => Error::Docker(error),
    }
}

/// A time Docker stamped in `State`, such as `StartedAt`.
pub(super) fn state_time(
    state: &serde_json::Value,
    field: &str,
) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(state.get(field)?.as_str()?)
        .ok()
        .map(Into::into)
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Inspected {
    pub(super) id: Option<String>,
    pub(super) name: Option<String>,
    pub(super) restart_count: Option<i64>,
    pub(super) created: Option<String>,
    pub(super) image: Option<String>,
    pub(super) config: Option<RawContainerConfig>,
    pub(super) state: Option<serde_json::Value>,
    pub(super) network_settings: Option<RawNetworkSettings>,
    pub(super) host_config: Option<RawHostConfig>,
    pub(super) mounts: Option<Vec<RawMount>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawContainerConfig {
    pub(super) env: Option<Vec<String>>,
    pub(super) labels: Option<HashMap<String, String>>,
    pub(super) healthcheck: Option<HealthConfig>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawHostConfig {
    pub(super) log_config: Option<RawLogConfig>,
    pub(super) network_mode: Option<String>,
    pub(super) nano_cpus: Option<i64>,
    pub(super) memory: Option<i64>,
    pub(super) memory_reservation: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct RawLogConfig {
    #[serde(rename = "Type")]
    pub(super) typ: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawNetworkSettings {
    pub(super) networks: Option<HashMap<String, RawEndpointSettings>>,
}

#[derive(Deserialize)]
pub(crate) struct RawEndpointSettings {
    #[serde(rename = "IPAddress")]
    pub(super) ip_address: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawMount {
    #[serde(rename = "Type")]
    pub(super) typ: Option<String>,
    pub(super) name: Option<String>,
    pub(super) driver: Option<String>,
}

impl Inspected {
    pub(crate) fn from_typed(
        inspected: &ContainerInspectResponse,
    ) -> Result<Self, serde_json::Error> {
        serde_json::from_value(serde_json::to_value(inspected)?)
    }

    pub(crate) fn labels(&self) -> Option<&HashMap<String, String>> {
        self.config.as_ref()?.labels.as_ref()
    }

    pub(crate) fn label(&self, key: &str) -> Option<&str> {
        self.labels()?.get(key).map(String::as_str)
    }

    /// The creation key a keyed create stamped, `None` on a Container made without one.
    pub(crate) fn creation_key(&self) -> Option<&str> {
        self.label(labels::CREATION_KEY)
    }

    fn state_field(&self, field: &str) -> Option<&serde_json::Value> {
        self.state.as_ref()?.get(field)
    }

    /// Docker's `State.Status`: `created`, `running`, `paused`, `restarting`,
    /// `removing`, `exited` or `dead`.
    pub(crate) fn status(&self) -> Option<&str> {
        self.state_field("Status")?.as_str()
    }

    /// Whether Docker's status is `running`. A paused or restarting Container is not.
    pub(crate) fn is_running(&self) -> bool {
        self.status() == Some("running")
    }

    /// Docker's `State.Running`: the Container has a process, which a paused or
    /// restarting Container also has.
    pub(crate) fn process_alive(&self) -> bool {
        self.state_field("Running")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    /// Docker left this container created and recorded why its start failed.
    /// Docker clears `State.Error` when a start succeeds, so a container at rest with one
    /// recorded is one whose last start failed: its first, or the one at boot that
    /// leaves it exited with no retry.
    pub(crate) fn failed_start(&self) -> bool {
        matches!(self.status(), Some("created" | "exited"))
            && self
                .state_field("Error")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|error| !error.is_empty())
    }

    /// The Container's process ID, `None` when Docker reports none or zero.
    pub(crate) fn pid(&self) -> Option<i64> {
        self.state_field("Pid")
            .and_then(serde_json::Value::as_i64)
            .filter(|pid| *pid > 0)
    }

    pub(crate) fn exit_code(&self) -> Option<i64> {
        self.state_field("ExitCode")?.as_i64()
    }

    /// Docker OOM-killed a process of this run.
    pub(crate) fn oom_killed(&self) -> bool {
        self.state_field("OOMKilled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    pub(crate) fn started_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        state_time(self.state.as_ref()?, "StartedAt")
    }

    pub(crate) fn finished_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        state_time(self.state.as_ref()?, "FinishedAt")
    }

    pub(crate) fn network_mode(&self) -> Option<&str> {
        self.host_config.as_ref()?.network_mode.as_deref()
    }

    /// Whether Docker lists the Container on the network `name`.
    pub(crate) fn attached_to(&self, name: &str) -> bool {
        self.network_settings
            .as_ref()
            .and_then(|settings| settings.networks.as_ref())
            .is_some_and(|networks| networks.contains_key(name))
    }
}
