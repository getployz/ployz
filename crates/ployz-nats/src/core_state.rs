//! NATS-backed canonical current-state adapters.

use crate::kv::KV_CORE_BUCKET;
use async_nats::jetstream;
use ployz_core::ids::ServiceId;
use ployz_core::state::{
    ActiveServiceCommit, ActiveServiceCommitRequest, ActiveServiceStaleReason, ActiveServiceState,
    ActiveServiceStateKey, CoreStateRevision, ExpectedActiveService,
};
use std::future::Future;

const NATS_CORE_STATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct AsyncNatsCoreStateStore {
    bucket: jetstream::kv::Store,
}

impl AsyncNatsCoreStateStore {
    pub async fn from_jetstream(
        jetstream: &jetstream::Context,
    ) -> Result<Self, CoreStateStoreError> {
        let bucket = with_core_state_timeout(
            "core state bucket open",
            jetstream.get_key_value(KV_CORE_BUCKET),
        )
        .await?
        .map_err(|error| CoreStateStoreError::OpenBucket {
            bucket: KV_CORE_BUCKET,
            message: error.to_string(),
        })?;

        Ok(Self { bucket })
    }

    pub async fn commit_active_service(
        &self,
        request: &ActiveServiceCommitRequest,
    ) -> Result<ActiveServiceCommit, CoreStateStoreError> {
        let key = ActiveServiceStateKey::from_service_id(&request.service_id);
        let state = ActiveServiceState {
            service_id: request.service_id.clone(),
            active_revision: request.target_revision.clone(),
        };
        let payload = serde_json::to_vec(&state).map_err(CoreStateStoreError::Encode)?;
        let existing = with_core_state_timeout(
            "active service state entry read",
            self.bucket.entry(key.as_str()),
        )
        .await?
        .map_err(|error| CoreStateStoreError::Get {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?
        .map(|entry| {
            loaded_active_service_state(&request.service_id, &key, &entry.value, entry.revision)
        })
        .transpose()?;

        match classify_active_service_preflight(
            existing.as_ref(),
            &request.expected_current,
            &state,
        ) {
            ActiveServiceCommitDecision::Create => match with_core_state_timeout(
                "active service state create",
                self.bucket.create(key.as_str(), payload.into()),
            )
            .await?
            {
                Ok(revision) => Ok(ActiveServiceCommit::Stored {
                    revision: CoreStateRevision::new(revision),
                }),
                Err(error) => {
                    self.classify_commit_conflict(
                        &request.service_id,
                        &key,
                        &request.expected_current,
                        &state,
                        error,
                    )
                    .await
                }
            },
            ActiveServiceCommitDecision::Update { revision } => {
                match with_core_state_timeout(
                    "active service state update",
                    self.bucket
                        .update(key.as_str(), payload.into(), revision.get()),
                )
                .await?
                {
                    Ok(revision) => Ok(ActiveServiceCommit::Stored {
                        revision: CoreStateRevision::new(revision),
                    }),
                    Err(error) => {
                        self.classify_commit_conflict(
                            &request.service_id,
                            &key,
                            &request.expected_current,
                            &state,
                            error,
                        )
                        .await
                    }
                }
            }
            ActiveServiceCommitDecision::Complete(outcome) => Ok(outcome),
        }
    }

    pub async fn active_service(
        &self,
        service_id: &ServiceId,
    ) -> Result<Option<ActiveServiceState>, CoreStateStoreError> {
        let key = ActiveServiceStateKey::from_service_id(service_id);
        let Some(payload) =
            with_core_state_timeout("active service state get", self.bucket.get(key.as_str()))
                .await?
                .map_err(|error| CoreStateStoreError::Get {
                    key: key.as_str().to_owned(),
                    message: error.to_string(),
                })?
        else {
            return Ok(None);
        };

        decode_active_service_state(service_id, &key, &payload).map(Some)
    }

    async fn classify_commit_conflict(
        &self,
        service_id: &ServiceId,
        key: &ActiveServiceStateKey,
        expected_current: &ExpectedActiveService,
        attempted: &ActiveServiceState,
        error: impl ToString,
    ) -> Result<ActiveServiceCommit, CoreStateStoreError> {
        let Some(existing) = with_core_state_timeout(
            "active service state conflict read",
            self.bucket.entry(key.as_str()),
        )
        .await?
        .map_err(|read_error| CoreStateStoreError::Get {
            key: key.as_str().to_owned(),
            message: read_error.to_string(),
        })?
        else {
            return Err(CoreStateStoreError::CasConflict {
                message: error.to_string(),
            });
        };

        let current =
            loaded_active_service_state(service_id, key, &existing.value, existing.revision)?;
        Ok(classify_active_service_write_conflict(
            &current,
            expected_current,
            attempted,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LoadedActiveServiceState {
    state: ActiveServiceState,
    revision: CoreStateRevision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActiveServiceCommitDecision {
    Create,
    Update { revision: CoreStateRevision },
    Complete(ActiveServiceCommit),
}

fn classify_active_service_preflight(
    existing: Option<&LoadedActiveServiceState>,
    expected_current: &ExpectedActiveService,
    attempted: &ActiveServiceState,
) -> ActiveServiceCommitDecision {
    let Some(existing) = existing else {
        return match expected_current {
            ExpectedActiveService::Absent => ActiveServiceCommitDecision::Create,
            ExpectedActiveService::Revision(expected) => {
                ActiveServiceCommitDecision::Complete(ActiveServiceCommit::Stale {
                    reason: ActiveServiceStaleReason::Missing {
                        expected_revision: expected.clone(),
                    },
                })
            }
        };
    };

    let current_revision = &existing.state.active_revision;
    if current_revision == &attempted.active_revision {
        return ActiveServiceCommitDecision::Complete(ActiveServiceCommit::AlreadyCommitted {
            current_revision: current_revision.clone(),
        });
    }

    match expected_current {
        ExpectedActiveService::Absent => {
            ActiveServiceCommitDecision::Complete(ActiveServiceCommit::Stale {
                reason: ActiveServiceStaleReason::UnexpectedCurrent {
                    current_revision: current_revision.clone(),
                },
            })
        }
        ExpectedActiveService::Revision(expected) if current_revision != expected => {
            ActiveServiceCommitDecision::Complete(ActiveServiceCommit::Stale {
                reason: ActiveServiceStaleReason::Mismatch {
                    expected_revision: expected.clone(),
                    current_revision: current_revision.clone(),
                },
            })
        }
        ExpectedActiveService::Revision(_) => ActiveServiceCommitDecision::Update {
            revision: existing.revision,
        },
    }
}

fn classify_active_service_write_conflict(
    current: &LoadedActiveServiceState,
    expected_current: &ExpectedActiveService,
    attempted: &ActiveServiceState,
) -> ActiveServiceCommit {
    if current.state.active_revision == attempted.active_revision {
        return ActiveServiceCommit::AlreadyCommitted {
            current_revision: current.state.active_revision.clone(),
        };
    }

    ActiveServiceCommit::Contended {
        current_revision: current.state.active_revision.clone(),
        attempted_revision: attempted.active_revision.clone(),
        expected_current: expected_current.clone(),
    }
}

#[derive(Debug)]
pub enum CoreStateStoreError {
    OpenBucket {
        bucket: &'static str,
        message: String,
    },
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    CasConflict {
        message: String,
    },
    Get {
        key: String,
        message: String,
    },
    CorruptActiveServiceState {
        key: String,
        expected_service_id: ServiceId,
        actual_service_id: ServiceId,
    },
    Timeout {
        operation: &'static str,
    },
}

fn loaded_active_service_state(
    expected_service_id: &ServiceId,
    key: &ActiveServiceStateKey,
    payload: &[u8],
    revision: u64,
) -> Result<LoadedActiveServiceState, CoreStateStoreError> {
    Ok(LoadedActiveServiceState {
        state: decode_active_service_state(expected_service_id, key, payload)?,
        revision: CoreStateRevision::new(revision),
    })
}

fn decode_active_service_state(
    expected_service_id: &ServiceId,
    key: &ActiveServiceStateKey,
    payload: &[u8],
) -> Result<ActiveServiceState, CoreStateStoreError> {
    let state: ActiveServiceState =
        serde_json::from_slice(payload).map_err(CoreStateStoreError::Decode)?;
    if state.service_id != *expected_service_id {
        return Err(CoreStateStoreError::CorruptActiveServiceState {
            key: key.as_str().to_owned(),
            expected_service_id: expected_service_id.clone(),
            actual_service_id: state.service_id,
        });
    }

    Ok(state)
}

async fn with_core_state_timeout<T>(
    operation: &'static str,
    future: impl Future<Output = T>,
) -> Result<T, CoreStateStoreError> {
    tokio::time::timeout(NATS_CORE_STATE_TIMEOUT, future)
        .await
        .map_err(|_| CoreStateStoreError::Timeout { operation })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::RevisionId;

    #[test]
    fn active_service_classifier_requests_update_when_precondition_still_holds() {
        let service_id = service_id("svc_api");
        let rev_1 = revision_id("rev_1");
        let rev_2 = revision_id("rev_2");
        let existing = LoadedActiveServiceState {
            state: ActiveServiceState {
                service_id: service_id.clone(),
                active_revision: rev_1.clone(),
            },
            revision: CoreStateRevision::new(7),
        };
        let attempted = ActiveServiceState {
            service_id,
            active_revision: rev_2,
        };

        assert_eq!(
            classify_active_service_preflight(
                Some(&existing),
                &ExpectedActiveService::Revision(rev_1),
                &attempted,
            ),
            ActiveServiceCommitDecision::Update {
                revision: CoreStateRevision::new(7)
            }
        );
    }

    #[test]
    fn active_service_conflict_reports_concurrent_revision() {
        let service_id = service_id("svc_api");
        let rev_1 = revision_id("rev_1");
        let rev_2 = revision_id("rev_2");
        let rev_3 = revision_id("rev_3");
        let current = LoadedActiveServiceState {
            state: ActiveServiceState {
                service_id: service_id.clone(),
                active_revision: rev_2.clone(),
            },
            revision: CoreStateRevision::new(8),
        };
        let attempted = ActiveServiceState {
            service_id,
            active_revision: rev_3.clone(),
        };

        assert_eq!(
            classify_active_service_write_conflict(
                &current,
                &ExpectedActiveService::Revision(rev_1.clone()),
                &attempted,
            ),
            ActiveServiceCommit::Contended {
                current_revision: rev_2,
                attempted_revision: rev_3,
                expected_current: ExpectedActiveService::Revision(rev_1)
            }
        );
    }

    fn service_id(value: &str) -> ServiceId {
        ServiceId::try_new(value).expect("valid service id")
    }

    fn revision_id(value: &str) -> RevisionId {
        RevisionId::try_new(value).expect("valid revision id")
    }
}
