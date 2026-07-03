use super::AsyncNatsCoreStateStore;
use crate::kv::{KvListError, NatsIoTimeout, list_current, with_io_timeout};
use ployz_core::ops::RouteTarget;
use ployz_core::state::{RouteBindingState, RouteBindingStateKey};
use std::fmt;

impl AsyncNatsCoreStateStore {
    pub async fn replace_route_binding(
        &self,
        state: &RouteBindingState,
    ) -> Result<(), RouteBindingStoreError> {
        let key = RouteBindingStateKey::from_target(&state.target);
        let payload = serde_json::to_vec(state).map_err(RouteBindingStoreError::Encode)?;
        with_io_timeout(
            "route binding state replace",
            self.bucket.put(key.as_str(), payload.into()),
        )
        .await?
        .map_err(|error| RouteBindingStoreError::Put {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?;

        Ok(())
    }

    pub async fn route_binding(
        &self,
        target: &RouteTarget,
    ) -> Result<Option<RouteBindingState>, RouteBindingStoreError> {
        let key = RouteBindingStateKey::from_target(target);
        let Some(payload) =
            with_io_timeout("route binding state get", self.bucket.get(key.as_str()))
                .await?
                .map_err(|error| RouteBindingStoreError::Get {
                    key: key.as_str().to_owned(),
                    message: error.to_string(),
                })?
        else {
            return Ok(None);
        };

        decode_active_route_state(target, &key, &payload).map(Some)
    }

    pub async fn remove_route_binding(
        &self,
        target: &RouteTarget,
    ) -> Result<(), RouteBindingStoreError> {
        let key = RouteBindingStateKey::from_target(target);
        with_io_timeout(
            "route binding state delete",
            self.bucket.delete(key.as_str()),
        )
        .await?
        .map_err(|error| RouteBindingStoreError::Delete {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?;

        Ok(())
    }

    pub async fn route_bindings(&self) -> Result<Vec<RouteBindingState>, RouteBindingStoreError> {
        let route_key_prefix = format!("{}.", ployz_core::state::ROUTE_BINDING_STATE_PREFIX);
        list_current(
            &self.bucket,
            &route_key_prefix,
            |state: &RouteBindingState| {
                RouteBindingStateKey::from_target(&state.target)
                    .as_str()
                    .to_owned()
            },
            |state| state.target.clone(),
        )
        .await
        .map_err(RouteBindingStoreError::from)
    }

    pub async fn watch_active_route_changes(
        &self,
    ) -> Result<async_nats::jetstream::kv::Watch, RouteBindingStoreError> {
        let route_key_filter = format!("{}.>", ployz_core::state::ROUTE_BINDING_STATE_PREFIX);
        with_io_timeout(
            "route binding state watch",
            self.bucket.watch_with_history(route_key_filter),
        )
        .await?
        .map_err(|error| RouteBindingStoreError::Watch {
            message: error.to_string(),
        })
    }
}

#[derive(Debug)]
pub enum RouteBindingStoreError {
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

impl fmt::Display for RouteBindingStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(error) => write!(formatter, "encode route binding state: {error}"),
            Self::Decode(error) => write!(formatter, "decode route binding state: {error}"),
            Self::Put { key, message } => write!(formatter, "put {key}: {message}"),
            Self::Delete { key, message } => write!(formatter, "delete {key}: {message}"),
            Self::Get { key, message } => write!(formatter, "get {key}: {message}"),
            Self::ListKeys { message } => write!(formatter, "list route binding keys: {message}"),
            Self::Watch { message } => write!(formatter, "watch route binding keys: {message}"),
            Self::CorruptActiveRouteState {
                key,
                expected_target,
                actual_target,
            } => write!(
                formatter,
                "route binding state at {} belongs to {:?}, not {:?}",
                key, actual_target, expected_target
            ),
            Self::CorruptKey { key, actual_key } => write!(
                formatter,
                "route binding state key {} does not match encoded target key {}",
                key, actual_key
            ),
            Self::Timeout { operation } => write!(formatter, "{operation} timed out"),
        }
    }
}

impl From<NatsIoTimeout> for RouteBindingStoreError {
    fn from(timeout: NatsIoTimeout) -> Self {
        Self::Timeout {
            operation: timeout.operation,
        }
    }
}

impl From<KvListError> for RouteBindingStoreError {
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
    key: &RouteBindingStateKey,
    payload: &[u8],
) -> Result<RouteBindingState, RouteBindingStoreError> {
    let state: RouteBindingState =
        serde_json::from_slice(payload).map_err(RouteBindingStoreError::Decode)?;
    if state.target != *expected_target {
        return Err(RouteBindingStoreError::CorruptActiveRouteState {
            key: key.as_str().to_owned(),
            expected_target: expected_target.clone(),
            actual_target: state.target,
        });
    }

    Ok(state)
}
