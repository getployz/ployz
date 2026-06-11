use super::AsyncNatsCoreStateStore;
use crate::kv::{KvListError, NatsIoTimeout, list_current, with_io_timeout};
use ployz_core::ops::RouteTarget;
use ployz_core::state::{
    ActiveRouteCommit, ActiveRouteCommitRequest, ActiveRouteState, ActiveRouteStateKey,
    CoreStateRevision, ExpectedActiveRoute, ExpectedActiveRouteRevision,
};
use std::fmt;

impl AsyncNatsCoreStateStore {
    pub async fn commit_active_route(
        &self,
        request: &ActiveRouteCommitRequest,
    ) -> Result<ActiveRouteCommit, ActiveRouteStoreError> {
        let key = ActiveRouteStateKey::from_target(&request.target);
        let state = ActiveRouteState {
            target: request.target.clone(),
            endpoint_port: request.endpoint_port,
            service_id: request.service_id.clone(),
            revision_id: request.revision_id.clone(),
        };
        let payload = serde_json::to_vec(&state).map_err(ActiveRouteStoreError::Encode)?;
        let existing = with_io_timeout(
            "active route state entry read",
            self.bucket.entry(key.as_str()),
        )
        .await?
        .map_err(|error| ActiveRouteStoreError::Get {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?
        .map(|entry| loaded_active_route_state(&request.target, &key, &entry))
        .transpose()?;

        match classify_active_route_preflight(existing.as_ref(), &request.expected_current, &state)
        {
            ActiveRouteCommitDecision::Create => match with_io_timeout(
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
                match with_io_timeout(
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

    async fn classify_active_route_commit_conflict(
        &self,
        target: &RouteTarget,
        key: &ActiveRouteStateKey,
        expected_current: &ExpectedActiveRoute,
        attempted: &ActiveRouteState,
        error: impl ToString,
    ) -> Result<ActiveRouteCommit, ActiveRouteStoreError> {
        let Some(existing) = with_io_timeout(
            "active route state conflict read",
            self.bucket.entry(key.as_str()),
        )
        .await?
        .map_err(|read_error| ActiveRouteStoreError::Get {
            key: key.as_str().to_owned(),
            message: read_error.to_string(),
        })?
        else {
            return Err(ActiveRouteStoreError::CasConflict {
                message: error.to_string(),
            });
        };

        let current = loaded_active_route_state(target, key, &existing)?;
        Ok(classify_active_route_write_conflict(
            &current,
            expected_current,
            attempted,
        ))
    }
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
pub enum ActiveRouteStoreError {
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    CasConflict {
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
            Self::CasConflict { message } => write!(formatter, "cas conflict: {message}"),
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

fn loaded_active_route_state(
    expected_target: &RouteTarget,
    key: &ActiveRouteStateKey,
    entry: &async_nats::jetstream::kv::Entry,
) -> Result<LoadedActiveRouteState, ActiveRouteStoreError> {
    Ok(LoadedActiveRouteState {
        state: decode_active_route_state(expected_target, key, &entry.value)?,
        revision: CoreStateRevision::new(entry.revision),
    })
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
