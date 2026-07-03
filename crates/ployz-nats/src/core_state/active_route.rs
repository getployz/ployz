use super::AsyncNatsCoreStateStore;
use crate::kv::{KvListError, NatsIoTimeout, list_current, with_io_timeout};
use ployz_core::ops::RouteTarget;
use ployz_core::state::{ActiveRouteState, ActiveRouteStateKey};
use std::fmt;

impl AsyncNatsCoreStateStore {
    pub async fn replace_active_route(
        &self,
        state: &ActiveRouteState,
    ) -> Result<(), ActiveRouteStoreError> {
        let key = ActiveRouteStateKey::from_target(&state.target);
        let payload = serde_json::to_vec(state).map_err(ActiveRouteStoreError::Encode)?;
        with_io_timeout(
            "active route state replace",
            self.bucket.put(key.as_str(), payload.into()),
        )
        .await?
        .map_err(|error| ActiveRouteStoreError::Put {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?;

        Ok(())
    }

    pub async fn active_route(
        &self,
        target: &RouteTarget,
    ) -> Result<Option<ActiveRouteState>, ActiveRouteStoreError> {
        let key = ActiveRouteStateKey::from_target(target);
        let Some(payload) =
            with_io_timeout("active route state get", self.bucket.get(key.as_str()))
                .await?
                .map_err(|error| ActiveRouteStoreError::Get {
                    key: key.as_str().to_owned(),
                    message: error.to_string(),
                })?
        else {
            return Ok(None);
        };

        decode_active_route_state(target, &key, &payload).map(Some)
    }

    pub async fn remove_active_route(
        &self,
        target: &RouteTarget,
    ) -> Result<(), ActiveRouteStoreError> {
        let key = ActiveRouteStateKey::from_target(target);
        with_io_timeout(
            "active route state delete",
            self.bucket.delete(key.as_str()),
        )
        .await?
        .map_err(|error| ActiveRouteStoreError::Delete {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?;

        Ok(())
    }

    pub async fn active_routes(&self) -> Result<Vec<ActiveRouteState>, ActiveRouteStoreError> {
        let route_key_prefix = format!("{}.", ployz_core::state::ACTIVE_ROUTE_STATE_PREFIX);
        list_current(
            &self.bucket,
            &route_key_prefix,
            |state: &ActiveRouteState| {
                ActiveRouteStateKey::from_target(&state.target)
                    .as_str()
                    .to_owned()
            },
            |state| state.target.clone(),
        )
        .await
        .map_err(ActiveRouteStoreError::from)
    }

    pub async fn watch_active_route_changes(
        &self,
    ) -> Result<async_nats::jetstream::kv::Watch, ActiveRouteStoreError> {
        let route_key_filter = format!("{}.>", ployz_core::state::ACTIVE_ROUTE_STATE_PREFIX);
        with_io_timeout(
            "active route state watch",
            self.bucket.watch_with_history(route_key_filter),
        )
        .await?
        .map_err(|error| ActiveRouteStoreError::Watch {
            message: error.to_string(),
        })
    }
}

#[derive(Debug)]
pub enum ActiveRouteStoreError {
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    Put {
        key: String,
        message: String,
    },
    Delete {
        key: String,
        message: String,
    },
    Get {
        key: String,
        message: String,
    },
    ListKeys {
        message: String,
    },
    Watch {
        message: String,
    },
    CorruptActiveRouteState {
        key: String,
        expected_target: RouteTarget,
        actual_target: RouteTarget,
    },
    CorruptKey {
        key: String,
        actual_key: String,
    },
    Timeout {
        operation: &'static str,
    },
}

impl fmt::Display for ActiveRouteStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(error) => write!(formatter, "encode active route state: {error}"),
            Self::Decode(error) => write!(formatter, "decode active route state: {error}"),
            Self::Put { key, message } => write!(formatter, "put {key}: {message}"),
            Self::Delete { key, message } => write!(formatter, "delete {key}: {message}"),
            Self::Get { key, message } => write!(formatter, "get {key}: {message}"),
            Self::ListKeys { message } => write!(formatter, "list active route keys: {message}"),
            Self::Watch { message } => write!(formatter, "watch active route keys: {message}"),
            Self::CorruptActiveRouteState {
                key,
                expected_target,
                actual_target,
            } => write!(
                formatter,
                "active route state at {} belongs to {:?}, not {:?}",
                key, actual_target, expected_target
            ),
            Self::CorruptKey { key, actual_key } => write!(
                formatter,
                "active route state key {} does not match encoded target key {}",
                key, actual_key
            ),
            Self::Timeout { operation } => write!(formatter, "{operation} timed out"),
        }
    }
}

impl From<NatsIoTimeout> for ActiveRouteStoreError {
    fn from(timeout: NatsIoTimeout) -> Self {
        Self::Timeout {
            operation: timeout.operation,
        }
    }
}

impl From<KvListError> for ActiveRouteStoreError {
    fn from(error: KvListError) -> Self {
        match error {
            KvListError::Scan { message } => Self::ListKeys { message },
            KvListError::Decode(error) => Self::Decode(error),
            KvListError::CorruptKey { key, actual_key } => Self::CorruptKey { key, actual_key },
        }
    }
}

fn decode_active_route_state(
    expected_target: &RouteTarget,
    key: &ActiveRouteStateKey,
    payload: &[u8],
) -> Result<ActiveRouteState, ActiveRouteStoreError> {
    let state: ActiveRouteState =
        serde_json::from_slice(payload).map_err(ActiveRouteStoreError::Decode)?;
    if state.target != *expected_target {
        return Err(ActiveRouteStoreError::CorruptActiveRouteState {
            key: key.as_str().to_owned(),
            expected_target: expected_target.clone(),
            actual_target: state.target,
        });
    }

    Ok(state)
}
