use super::AsyncNatsCoreStateStore;
use ployz_core::ops::RouteTarget;
use ployz_core::state::{
    ActiveRouteCommit, ActiveRouteCommitRequest, ActiveRouteState, ActiveRouteStateKey,
    CoreStateRevision, ExpectedActiveRoute, ExpectedActiveRouteRevision,
};
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
        .map(|entry| {
            loaded_active_route_state(&request.target, &key, &entry.value, entry.revision)
                .map_err(ActiveRouteWriteError::from_route_read)
        })
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
            ActiveRouteCommitDecision::Complete(outcome) => Ok(outcome),
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

        let current = loaded_active_route_state(target, key, &existing.value, existing.revision)
            .map_err(ActiveRouteWriteError::from_route_read)?;
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
    Complete(ActiveRouteCommit),
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
            }) => ActiveRouteCommitDecision::Complete(ActiveRouteCommit::ActiveRouteChanged {
                expected_current: ExpectedActiveRoute::ServiceRevision(
                    ExpectedActiveRouteRevision {
                        service_id: service_id.clone(),
                        revision_id: revision_id.clone(),
                    },
                ),
                current: None,
                attempted: attempted.clone(),
            }),
        };
    };

    if active_route_state_matches(&existing.state, attempted) {
        return ActiveRouteCommitDecision::Complete(ActiveRouteCommit::AlreadyCommitted {
            service_id: existing.state.service_id.clone(),
            revision_id: existing.state.revision_id.clone(),
        });
    }

    match expected_current {
        ExpectedActiveRoute::Absent => {
            ActiveRouteCommitDecision::Complete(ActiveRouteCommit::ActiveRouteChanged {
                expected_current: ExpectedActiveRoute::Absent,
                current: Some(existing.state.clone()),
                attempted: attempted.clone(),
            })
        }
        ExpectedActiveRoute::ServiceRevision(ExpectedActiveRouteRevision {
            service_id,
            revision_id,
        }) if existing.state.service_id != *service_id
            || existing.state.revision_id != *revision_id =>
        {
            ActiveRouteCommitDecision::Complete(ActiveRouteCommit::ActiveRouteChanged {
                expected_current: ExpectedActiveRoute::ServiceRevision(
                    ExpectedActiveRouteRevision {
                        service_id: service_id.clone(),
                        revision_id: revision_id.clone(),
                    },
                ),
                current: Some(existing.state.clone()),
                attempted: attempted.clone(),
            })
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
        && current.service_id == attempted.service_id
        && current.revision_id == attempted.revision_id
}

#[derive(Debug)]
pub enum ActiveRouteReadError {
    Decode(serde_json::Error),
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

impl fmt::Display for ActiveRouteReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(error) => write!(formatter, "decode active route state: {error}"),
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

impl ActiveRouteWriteError {
    fn from_route_read(error: ActiveRouteReadError) -> Self {
        match error {
            ActiveRouteReadError::Decode(error) => Self::Decode(error),
            ActiveRouteReadError::Get { key, message } => Self::Get { key, message },
            ActiveRouteReadError::CorruptActiveRouteState {
                key,
                expected_target,
                actual_target,
            } => Self::CorruptActiveRouteState {
                key,
                expected_target,
                actual_target,
            },
            ActiveRouteReadError::Timeout { operation } => Self::Timeout { operation },
        }
    }
}

fn loaded_active_route_state(
    expected_target: &RouteTarget,
    key: &ActiveRouteStateKey,
    payload: &[u8],
    revision: u64,
) -> Result<LoadedActiveRouteState, ActiveRouteReadError> {
    Ok(LoadedActiveRouteState {
        state: decode_active_route_state(expected_target, key, payload)?,
        revision: CoreStateRevision::new(revision),
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
