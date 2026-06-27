use super::AsyncNatsCoreStateStore;
use crate::kv::{KvListError, NatsIoTimeout, list_current, with_io_timeout};
use ployz_core::ids::MachineId;
use ployz_core::state::{ACTIVE_MACHINE_STATE_PREFIX, ActiveMachineState, ActiveMachineStateKey};
use std::fmt;

impl AsyncNatsCoreStateStore {
    pub async fn replace_active_machine(
        &self,
        state: &ActiveMachineState,
    ) -> Result<(), ActiveMachineWriteError> {
        let key = ActiveMachineStateKey::from_machine_id(&state.machine_id);
        let payload = serde_json::to_vec(state).map_err(ActiveMachineWriteError::Encode)?;
        with_io_timeout(
            "active machine state put",
            self.bucket.put(key.as_str(), payload.into()),
        )
        .await?
        .map_err(|error| ActiveMachineWriteError::Put {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?;

        Ok(())
    }

    pub async fn active_machine(
        &self,
        machine_id: &MachineId,
    ) -> Result<Option<ActiveMachineState>, ActiveMachineReadError> {
        let key = ActiveMachineStateKey::from_machine_id(machine_id);
        let Some(payload) =
            with_io_timeout("active machine state get", self.bucket.get(key.as_str()))
                .await?
                .map_err(|error| ActiveMachineReadError::Get {
                    key: key.as_str().to_owned(),
                    message: error.to_string(),
                })?
        else {
            return Ok(None);
        };

        decode_active_machine_state(machine_id, &key, &payload).map(Some)
    }

    pub async fn active_machines(&self) -> Result<Vec<ActiveMachineState>, ActiveMachineReadError> {
        list_current(
            &self.bucket,
            &format!("{ACTIVE_MACHINE_STATE_PREFIX}."),
            |state: &ActiveMachineState| {
                ActiveMachineStateKey::from_machine_id(&state.machine_id)
                    .as_str()
                    .to_owned()
            },
            |state| state.machine_id.clone(),
        )
        .await
        .map_err(ActiveMachineReadError::from)
    }
}

fn decode_active_machine_state(
    expected_machine_id: &MachineId,
    key: &ActiveMachineStateKey,
    payload: &[u8],
) -> Result<ActiveMachineState, ActiveMachineReadError> {
    let state: ActiveMachineState =
        serde_json::from_slice(payload).map_err(ActiveMachineReadError::Decode)?;
    if state.machine_id != *expected_machine_id {
        return Err(ActiveMachineReadError::CorruptActiveMachineState {
            key: key.as_str().to_owned(),
            expected_machine_id: expected_machine_id.clone(),
        });
    }

    Ok(state)
}

#[derive(Debug)]
pub enum ActiveMachineWriteError {
    Encode(serde_json::Error),
    Put { key: String, message: String },
    Timeout { operation: &'static str },
}

impl fmt::Display for ActiveMachineWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(error) => write!(formatter, "encode active machine state: {error}"),
            Self::Put { key, message } => write!(formatter, "put {key}: {message}"),
            Self::Timeout { operation } => write!(formatter, "{operation} timed out"),
        }
    }
}

impl From<NatsIoTimeout> for ActiveMachineWriteError {
    fn from(timeout: NatsIoTimeout) -> Self {
        Self::Timeout {
            operation: timeout.operation,
        }
    }
}

#[derive(Debug)]
pub enum ActiveMachineReadError {
    Decode(serde_json::Error),
    Get {
        key: String,
        message: String,
    },
    ListKeys {
        message: String,
    },
    CorruptActiveMachineState {
        key: String,
        expected_machine_id: MachineId,
    },
    CorruptKey {
        key: String,
        actual_key: String,
    },
    Timeout {
        operation: &'static str,
    },
}

impl fmt::Display for ActiveMachineReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(error) => write!(formatter, "decode active machine state: {error}"),
            Self::Get { key, message } => write!(formatter, "get {key}: {message}"),
            Self::ListKeys { message } => write!(formatter, "list active machine keys: {message}"),
            Self::CorruptActiveMachineState {
                key,
                expected_machine_id,
            } => write!(
                formatter,
                "active machine state at {} does not belong to {}",
                key,
                expected_machine_id.as_str()
            ),
            Self::CorruptKey { key, actual_key } => write!(
                formatter,
                "active machine state key {key} does not match canonical key {actual_key}"
            ),
            Self::Timeout { operation } => write!(formatter, "{operation} timed out"),
        }
    }
}

impl From<NatsIoTimeout> for ActiveMachineReadError {
    fn from(timeout: NatsIoTimeout) -> Self {
        Self::Timeout {
            operation: timeout.operation,
        }
    }
}

impl From<KvListError> for ActiveMachineReadError {
    fn from(error: KvListError) -> Self {
        match error {
            KvListError::Scan { message } => Self::ListKeys { message },
            KvListError::Decode(error) => Self::Decode(error),
            KvListError::CorruptKey { key, actual_key } => Self::CorruptKey { key, actual_key },
        }
    }
}
