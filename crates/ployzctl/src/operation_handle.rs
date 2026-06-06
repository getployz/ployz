//! Ergonomic handle for accepted operations.

use crate::api_client::{OperationApiClient, OperationApiClientError};
use ployz_core::ops::{
    EventSequence, OperationEventReplayCursor, OperationEventReplayLimit, OperationEventReplayPage,
    OperationStatusSnapshot,
};
use ployz_sdk_types::{
    AcceptedOperation, OpsStatusError, OpsStatusRequest, OpsWatchError, OpsWatchRequest,
};

#[derive(Debug, Clone)]
pub struct OperationHandle {
    accepted: AcceptedOperation,
    client: OperationApiClient,
}

impl OperationHandle {
    #[must_use]
    pub fn new(accepted: AcceptedOperation, client: OperationApiClient) -> Self {
        Self { accepted, client }
    }

    #[must_use]
    pub const fn accepted(&self) -> &AcceptedOperation {
        &self.accepted
    }

    pub async fn status(
        &self,
    ) -> Result<OperationStatusSnapshot, OperationApiClientError<OpsStatusError>> {
        self.client
            .ops_status(&OpsStatusRequest {
                operation_id: self.accepted.operation_id.clone(),
            })
            .await
    }

    pub async fn replay_from_start(
        &self,
        limit: OperationEventReplayLimit,
    ) -> Result<OperationEventReplayPage, OperationApiClientError<OpsWatchError>> {
        self.replay_from(self.accepted.start_sequence, limit).await
    }

    pub async fn replay_from(
        &self,
        start_sequence: EventSequence,
        limit: OperationEventReplayLimit,
    ) -> Result<OperationEventReplayPage, OperationApiClientError<OpsWatchError>> {
        self.client
            .ops_watch(&OpsWatchRequest {
                operation_id: self.accepted.operation_id.clone(),
                start_sequence,
                limit,
            })
            .await
    }

    #[must_use]
    pub fn replay_pages(&self, limit: OperationEventReplayLimit) -> OperationReplayPages<'_> {
        OperationReplayPages {
            handle: self,
            next_sequence: Some(self.accepted.start_sequence),
            limit,
        }
    }
}

pub struct OperationReplayPages<'a> {
    handle: &'a OperationHandle,
    next_sequence: Option<EventSequence>,
    limit: OperationEventReplayLimit,
}

impl OperationReplayPages<'_> {
    pub async fn next_page(
        &mut self,
    ) -> Result<Option<OperationEventReplayPage>, OperationApiClientError<OpsWatchError>> {
        let Some(start_sequence) = self.next_sequence else {
            return Ok(None);
        };

        let page = self.handle.replay_from(start_sequence, self.limit).await?;
        self.next_sequence = match &page.cursor {
            OperationEventReplayCursor::More {
                next_start_sequence,
            } => Some(*next_start_sequence),
            OperationEventReplayCursor::CaughtUp | OperationEventReplayCursor::Terminal => None,
        };

        Ok(Some(page))
    }
}
