use super::AsyncNatsCoreStateStore;
use crate::kv::bounded_bucket_key_scan_entries_with_prefix;
use async_nats::jetstream::kv::Operation;
use ployz_core::ops::RouteTarget;
use ployz_core::state::{
    ActiveRouteCommit, ActiveRouteCommitRequest, ActiveRouteState, ActiveRouteStateKey,
    CoreStateRevision, ExpectedActiveRoute, ExpectedActiveRouteRevision,
};
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;

const NATS_ROUTE_STATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

impl AsyncNatsCoreStateStore {
    pub async fn commit_active_route(
        &self,
        request: &ActiveRouteCommitRequest,
    ) -> Result<ActiveRouteCommit, ActiveRouteWriteError> {
        let key = ActiveRouteStateKey::from_target(&request.target);
        let state = ActiveRouteState {
            target: request.target.clone(),
            endpoint_port: request.endpoint_port,
            service_id: request.service_id.clone(),
            revision_id: request.revision_id.clone(),
        };
        let payload = serde_json::to_vec(&state).map_err(ActiveRouteWriteError::Encode)?;
        let existing = with_active_route_write_timeout(
            "active route state entry read",
            self.bucket.entry(key.as_str()),
        )
        .await?
        .map_err(|error| ActiveRouteWriteError::Get {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?
        .map(|entry| loaded_active_route_state_for_write(&request.target, &key, &entry))
        .transpose()?;

        match classify_active_route_preflight(existing.as_ref(), &request.expected_current, &state)
        {
            ActiveRouteCommitDecision::Create => match with_active_route_write_timeout(
                "active route state create",
                self.bucket.create(key.as_str(), payload.into()),
            )
            .await?
            {
                Ok(revision) => Ok(ActiveRouteCommit::Stored {
                    revision: CoreStateRevision::new(revision),
                }),
                Err(error) => {
                    self.classify_active_route_commit_conflict(
                        &request.target,
                        &key,
                        &request.expected_current,
                        &state,
                        error,
                    )
                    .await
                }
            },
            ActiveRouteCommitDecision::Update { revision } => {
                match with_active_route_write_timeout(
                    "active route state update",
                    self.bucket
                        .update(key.as_str(), payload.into(), revision.get()),
                )
                .await?
                {
                    Ok(revision) => Ok(ActiveRouteCommit::Stored {
                        revision: CoreStateRevision::new(revision),
                    }),
                    Err(error) => {
                        self.classify_active_route_commit_conflict(
                            &request.target,
                            &key,
                            &request.expected_current,
                            &state,
                            error,
                        )
                        .await
                    }
                }
            }
            ActiveRouteCommitDecision::Complete(outcome) => Ok(*outcome),
        }
    }

    pub async fn active_route(
        &self,
        target: &RouteTarget,
    ) -> Result<Option<ActiveRouteState>, ActiveRouteReadError> {
        let key = ActiveRouteStateKey::from_target(target);
        let Some(payload) =
            with_active_route_read_timeout("active route state get", self.bucket.get(key.as_str()))
                .await?
                .map_err(|error| ActiveRouteReadError::Get {
                    key: key.as_str().to_owned(),
                    message: error.to_string(),
                })?
        else {
            return Ok(None);
        };

        decode_active_route_state(target, &key, &payload).map(Some)
    }

    pub async fn active_routes(&self) -> Result<Vec<ActiveRouteState>, ActiveRouteReadError> {
        let route_key_prefix = format!("{}.", ployz_core::state::ACTIVE_ROUTE_STATE_PREFIX);
        let entries = bounded_bucket_key_scan_entries_with_prefix(
            &self.bucket,
            &route_key_prefix,
            NATS_ROUTE_STATE_TIMEOUT,
        )
        .await
        .map_err(|error| ActiveRouteReadError::ListKeys {
            message: error.message,
        })?;
        let current_entries = current_kv_entries(entries);
        let mut routes = Vec::new();

        for entry in current_entries.into_values() {
            routes.push(decode_active_route_state_for_key(&entry.key, &entry.value)?);
        }

        routes.sort_by(|left, right| left.target.cmp(&right.target));
        Ok(routes)
    }

    async fn classify_active_route_commit_conflict(
        &self,
        target: &RouteTarget,
        key: &ActiveRouteStateKey,
        expected_current: &ExpectedActiveRoute,
        attempted: &ActiveRouteState,
        error: impl ToString,
    ) -> Result<ActiveRouteCommit, ActiveRouteWriteError> {
        let Some(existing) = with_active_route_write_timeout(
            "active route state conflict read",
            self.bucket.entry(key.as_str()),
        )
        .await?
        .map_err(|read_error| ActiveRouteWriteError::Get {
            key: key.as_str().to_owned(),
            message: read_error.to_string(),
        })?
        else {
            return Err(ActiveRouteWriteError::CasConflict {
                message: error.to_string(),
            });
        };

        let current = loaded_active_route_state_for_write(target, key, &existing)?;
        Ok(classify_active_route_write_conflict(
            &current,
            expected_current,
            attempted,
        ))
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct LoadedActiveRouteState {
    state: ActiveRouteState,
    revision: CoreStateRevision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActiveRouteCommitDecision {
    Create,
    Update { revision: CoreStateRevision },
    Complete(Box<ActiveRouteCommit>),
}

fn classify_active_route_preflight(
    existing: Option<&LoadedActiveRouteState>,
    expected_current: &ExpectedActiveRoute,
    attempted: &ActiveRouteState,
) -> ActiveRouteCommitDecision {
    let Some(existing) = existing else {
        return match expected_current {
            ExpectedActiveRoute::Absent => ActiveRouteCommitDecision::Create,
            ExpectedActiveRoute::ServiceRevision(ExpectedActiveRouteRevision {
                service_id,
                revision_id,
                endpoint_port,
            }) => ActiveRouteCommitDecision::Complete(Box::new(
                ActiveRouteCommit::ActiveRouteChanged {
                    expected_current: ExpectedActiveRoute::ServiceRevision(
                        ExpectedActiveRouteRevision {
                            service_id: service_id.clone(),
                            revision_id: revision_id.clone(),
                            endpoint_port: *endpoint_port,
                        },
                    ),
                    current: None,
                    attempted: attempted.clone(),
                },
            )),
        };
    };

    if active_route_state_matches(&existing.state, attempted) {
        return ActiveRouteCommitDecision::Complete(Box::new(
            ActiveRouteCommit::AlreadyCommitted {
                service_id: existing.state.service_id.clone(),
                revision_id: existing.state.revision_id.clone(),
            },
        ));
    }

    match expected_current {
        ExpectedActiveRoute::Absent => {
            ActiveRouteCommitDecision::Complete(Box::new(ActiveRouteCommit::ActiveRouteChanged {
                expected_current: ExpectedActiveRoute::Absent,
                current: Some(existing.state.clone()),
                attempted: attempted.clone(),
            }))
        }
        ExpectedActiveRoute::ServiceRevision(ExpectedActiveRouteRevision {
            service_id,
            revision_id,
            endpoint_port,
        }) if existing.state.service_id != *service_id
            || existing.state.revision_id != *revision_id
            || existing.state.endpoint_port != *endpoint_port =>
        {
            ActiveRouteCommitDecision::Complete(Box::new(ActiveRouteCommit::ActiveRouteChanged {
                expected_current: ExpectedActiveRoute::ServiceRevision(
                    ExpectedActiveRouteRevision {
                        service_id: service_id.clone(),
                        revision_id: revision_id.clone(),
                        endpoint_port: *endpoint_port,
                    },
                ),
                current: Some(existing.state.clone()),
                attempted: attempted.clone(),
            }))
        }
        ExpectedActiveRoute::ServiceRevision(_) => ActiveRouteCommitDecision::Update {
            revision: existing.revision,
        },
    }
}

fn classify_active_route_write_conflict(
    current: &LoadedActiveRouteState,
    expected_current: &ExpectedActiveRoute,
    attempted: &ActiveRouteState,
) -> ActiveRouteCommit {
    if active_route_state_matches(&current.state, attempted) {
        return ActiveRouteCommit::AlreadyCommitted {
            service_id: current.state.service_id.clone(),
            revision_id: current.state.revision_id.clone(),
        };
    }

    ActiveRouteCommit::ActiveRouteChanged {
        expected_current: expected_current.clone(),
        current: Some(current.state.clone()),
        attempted: attempted.clone(),
    }
}

fn active_route_state_matches(current: &ActiveRouteState, attempted: &ActiveRouteState) -> bool {
    current.target == attempted.target
        && current.endpoint_port == attempted.endpoint_port
        && current.service_id == attempted.service_id
        && current.revision_id == attempted.revision_id
}

#[derive(Debug)]
pub enum ActiveRouteReadError {
    Decode(serde_json::Error),
    ListKeys {
        message: String,
    },
    Get {
        key: String,
        message: String,
    },
    CorruptActiveRouteState {
        key: String,
        expected_target: RouteTarget,
        actual_target: RouteTarget,
    },
    CorruptActiveRouteKey {
        key: String,
        actual_key: String,
    },
    Timeout {
        operation: &'static str,
    },
}

impl fmt::Display for ActiveRouteReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(error) => write!(formatter, "decode active route state: {error}"),
            Self::ListKeys { message } => write!(formatter, "list active route keys: {message}"),
            Self::Get { key, message } => write!(formatter, "get {key}: {message}"),
            Self::CorruptActiveRouteState {
                key,
                expected_target,
                actual_target,
            } => write!(
                formatter,
                "active route state at {} belongs to {:?}, not {:?}",
                key, actual_target, expected_target
            ),
            Self::CorruptActiveRouteKey { key, actual_key } => write!(
                formatter,
                "active route state key {} does not match encoded target key {}",
                key, actual_key
            ),
            Self::Timeout { operation } => write!(formatter, "{operation} timed out"),
        }
    }
}

#[derive(Debug)]
pub enum ActiveRouteWriteError {
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    CasConflict {
        message: String,
    },
    Get {
        key: String,
        message: String,
    },
    CorruptActiveRouteState {
        key: String,
        expected_target: RouteTarget,
        actual_target: RouteTarget,
    },
    Timeout {
        operation: &'static str,
    },
}

impl fmt::Display for ActiveRouteWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(error) => write!(formatter, "encode active route state: {error}"),
            Self::Decode(error) => write!(formatter, "decode active route state: {error}"),
            Self::CasConflict { message } => write!(formatter, "cas conflict: {message}"),
            Self::Get { key, message } => write!(formatter, "get {key}: {message}"),
            Self::CorruptActiveRouteState {
                key,
                expected_target,
                actual_target,
            } => write!(
                formatter,
                "active route state at {} belongs to {:?}, not {:?}",
                key, actual_target, expected_target
            ),
            Self::Timeout { operation } => write!(formatter, "{operation} timed out"),
        }
    }
}

fn loaded_active_route_state_for_write(
    expected_target: &RouteTarget,
    key: &ActiveRouteStateKey,
    entry: &async_nats::jetstream::kv::Entry,
) -> Result<LoadedActiveRouteState, ActiveRouteWriteError> {
    Ok(LoadedActiveRouteState {
        state: decode_active_route_state_for_write(expected_target, key, &entry.value)?,
        revision: CoreStateRevision::new(entry.revision),
    })
}

fn decode_active_route_state(
    expected_target: &RouteTarget,
    key: &ActiveRouteStateKey,
    payload: &[u8],
) -> Result<ActiveRouteState, ActiveRouteReadError> {
    let state: ActiveRouteState =
        serde_json::from_slice(payload).map_err(ActiveRouteReadError::Decode)?;
    if state.target != *expected_target {
        return Err(ActiveRouteReadError::CorruptActiveRouteState {
            key: key.as_str().to_owned(),
            expected_target: expected_target.clone(),
            actual_target: state.target,
        });
    }

    Ok(state)
}

fn decode_active_route_state_for_write(
    expected_target: &RouteTarget,
    key: &ActiveRouteStateKey,
    payload: &[u8],
) -> Result<ActiveRouteState, ActiveRouteWriteError> {
    let state: ActiveRouteState =
        serde_json::from_slice(payload).map_err(ActiveRouteWriteError::Decode)?;
    if state.target != *expected_target {
        return Err(ActiveRouteWriteError::CorruptActiveRouteState {
            key: key.as_str().to_owned(),
            expected_target: expected_target.clone(),
            actual_target: state.target,
        });
    }

    Ok(state)
}

fn decode_active_route_state_for_key(
    key: &str,
    payload: &[u8],
) -> Result<ActiveRouteState, ActiveRouteReadError> {
    let state: ActiveRouteState =
        serde_json::from_slice(payload).map_err(ActiveRouteReadError::Decode)?;
    let actual_key = ActiveRouteStateKey::from_target(&state.target);
    if key != actual_key.as_str() {
        return Err(ActiveRouteReadError::CorruptActiveRouteKey {
            key: key.to_owned(),
            actual_key: actual_key.as_str().to_owned(),
        });
    }

    Ok(state)
}

async fn with_active_route_read_timeout<T>(
    operation: &'static str,
    future: impl Future<Output = T>,
) -> Result<T, ActiveRouteReadError> {
    tokio::time::timeout(NATS_ROUTE_STATE_TIMEOUT, future)
        .await
        .map_err(|_| ActiveRouteReadError::Timeout { operation })
}

async fn with_active_route_write_timeout<T>(
    operation: &'static str,
    future: impl Future<Output = T>,
) -> Result<T, ActiveRouteWriteError> {
    tokio::time::timeout(NATS_ROUTE_STATE_TIMEOUT, future)
        .await
        .map_err(|_| ActiveRouteWriteError::Timeout { operation })
}
