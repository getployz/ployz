use super::AsyncNatsCoreStateStore;
use crate::kv::bounded_bucket_key_scan_entries_with_prefix;
use async_nats::jetstream::kv::Operation;
use ployz_core::ids::NodeId;
use ployz_core::state::{ACTIVE_MACHINE_STATE_PREFIX, ActiveMachineState, ActiveMachineStateKey};
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;

const NATS_MACHINE_STATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

impl AsyncNatsCoreStateStore {
    pub async fn replace_active_machine(
        &self,
        state: &ActiveMachineState,
    ) -> Result<(), ActiveMachineWriteError> {
        let key = ActiveMachineStateKey::from_node_id(&state.node_id);
        let payload = serde_json::to_vec(state).map_err(ActiveMachineWriteError::Encode)?;
        with_active_machine_write_timeout(
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
        node_id: &NodeId,
    ) -> Result<Option<ActiveMachineState>, ActiveMachineReadError> {
        let key = ActiveMachineStateKey::from_node_id(node_id);
        let Some(payload) = with_active_machine_read_timeout(
            "active machine state get",
            self.bucket.get(key.as_str()),
        )
        .await?
        .map_err(|error| ActiveMachineReadError::Get {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?
        else {
            return Ok(None);
        };

        decode_active_machine_state(node_id, &key, &payload).map(Some)
    }

    pub async fn active_machines(&self) -> Result<Vec<ActiveMachineState>, ActiveMachineReadError> {
        let entries = bounded_bucket_key_scan_entries_with_prefix(
            &self.bucket,
            &format!("{ACTIVE_MACHINE_STATE_PREFIX}."),
            NATS_MACHINE_STATE_TIMEOUT,
        )
        .await
        .map_err(|error| ActiveMachineReadError::ListKeys {
            message: error.message,
        })?;
        let current_entries = current_kv_entries(entries);
        let mut machines = Vec::new();

        for entry in current_entries.into_values() {
            machines.push(decode_active_machine_state_for_key(
                &entry.key,
                &entry.value,
            )?);
        }

        machines.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        Ok(machines)
    }
}

fn current_kv_entries(
    entries: impl IntoIterator<Item = async_nats::jetstream::kv::Entry>,
) -> BTreeMap<String, async_nats::jetstream::kv::Entry> {
    let mut current = BTreeMap::new();

    for entry in entries {
        match entry.operation {
            Operation::Put => {
                current.insert(entry.key.clone(), entry);
            }
            Operation::Delete | Operation::Purge => {
                current.remove(&entry.key);
            }
        }
    }

    current
}

fn decode_active_machine_state_for_key(
    key: &str,
    payload: &[u8],
) -> Result<ActiveMachineState, ActiveMachineReadError> {
    let state: ActiveMachineState =
        serde_json::from_slice(payload).map_err(ActiveMachineReadError::Decode)?;
    let actual_key = ActiveMachineStateKey::from_node_id(&state.node_id);
    if key != actual_key.as_str() {
        return Err(ActiveMachineReadError::CorruptActiveMachineState {
            key: key.to_owned(),
            expected_node_id: state.node_id.clone(),
        });
    }

    Ok(state)
}

fn decode_active_machine_state(
    expected_node_id: &NodeId,
    key: &ActiveMachineStateKey,
    payload: &[u8],
) -> Result<ActiveMachineState, ActiveMachineReadError> {
    let state: ActiveMachineState =
        serde_json::from_slice(payload).map_err(ActiveMachineReadError::Decode)?;
    if state.node_id != *expected_node_id {
        return Err(ActiveMachineReadError::CorruptActiveMachineState {
            key: key.as_str().to_owned(),
            expected_node_id: expected_node_id.clone(),
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
        expected_node_id: NodeId,
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
                expected_node_id,
            } => write!(
                formatter,
                "active machine state at {} does not belong to {}",
                key,
                expected_node_id.as_str()
            ),
            Self::Timeout { operation } => write!(formatter, "{operation} timed out"),
        }
    }
}

async fn with_active_machine_write_timeout<T>(
    operation: &'static str,
    future: impl Future<Output = T>,
) -> Result<T, ActiveMachineWriteError> {
    tokio::time::timeout(NATS_MACHINE_STATE_TIMEOUT, future)
        .await
        .map_err(|_| ActiveMachineWriteError::Timeout { operation })
}

async fn with_active_machine_read_timeout<T>(
    operation: &'static str,
    future: impl Future<Output = T>,
) -> Result<T, ActiveMachineReadError> {
    tokio::time::timeout(NATS_MACHINE_STATE_TIMEOUT, future)
        .await
        .map_err(|_| ActiveMachineReadError::Timeout { operation })
}
