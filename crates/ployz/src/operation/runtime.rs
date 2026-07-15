//! Operation query, replay, and progress execution.

use crate::api_client::OperationApiClient;
use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_support::{
    PloyzctlExecutionError, PloyzctlExecutionOutput, api_error, operation_api_client,
    watch_operation_until_terminal,
};
use crate::operation::command::{
    ListOutput, OpsListCommand, OpsStatusCommand, OpsWatchCommand, OpsWatchOutput, StatusOutput,
    WatchOutput,
};
use ployz_core::ids::OperationId;
use ployz_core::operation::{
    EventSequence, MAX_OPERATION_EVENT_REPLAY_LIMIT, OperationEventReplayCursor,
    OperationEventReplayLimit, OperationEventReplayRequest, ReplayedOperationEvent,
};

pub(crate) async fn status(
    command: OpsStatusCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let api = operation_api_client(config).await?;
    let operation_id = command.operation_id.clone();
    let snapshot = api
        .ops_status(&command.into_request())
        .await
        .map_err(api_error)?;
    let events = replay_events(&api, replay_request(operation_id)).await?;
    Ok(PloyzctlExecutionOutput::stdout(
        StatusOutput::new(snapshot, events).render(),
    ))
}

pub(crate) async fn list(
    command: OpsListCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let active_only = command.active_only;
    let api = operation_api_client(config).await?;
    let result = api
        .ops_list(&command.into_request())
        .await
        .map_err(api_error)?;
    let output = ListOutput::from_result(result);
    Ok(PloyzctlExecutionOutput::stdout(output.render())
        .with_stderr(output.render_more_hint(active_only)))
}

pub(crate) async fn watch(
    command: OpsWatchCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let api = operation_api_client(config).await?;
    let output = command.output;
    let request = command.into_request();
    let (events, outcome) = watch_operation_until_terminal(
        &api,
        request,
        config.ops_watch_timeout(),
        config.ops_watch_poll_interval(),
    )
    .await?;

    Ok(
        PloyzctlExecutionOutput::stdout(WatchOutput { events, output }.render())
            .with_operation_outcome(outcome),
    )
}

pub(crate) async fn watch_accepted(
    api: &OperationApiClient,
    operation_id: OperationId,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let (events, outcome) = watch_operation_until_terminal(
        api,
        replay_request(operation_id),
        config.ops_watch_timeout(),
        config.ops_watch_poll_interval(),
    )
    .await?;
    Ok(PloyzctlExecutionOutput::stdout(
        WatchOutput {
            events,
            output: OpsWatchOutput::Text,
        }
        .render(),
    )
    .with_operation_outcome(outcome))
}

pub(crate) async fn replay_events(
    api: &OperationApiClient,
    mut request: OperationEventReplayRequest,
) -> Result<Vec<ReplayedOperationEvent>, PloyzctlExecutionError> {
    let mut events = Vec::new();
    loop {
        let page = api.ops_watch(&request).await.map_err(api_error)?;
        events.extend(page.events);
        match page.cursor {
            OperationEventReplayCursor::More {
                next_start_sequence,
            } => request.start_sequence = next_start_sequence,
            OperationEventReplayCursor::CaughtUp | OperationEventReplayCursor::Terminal => {
                return Ok(events);
            }
        }
    }
}

pub(crate) fn replay_request(operation_id: OperationId) -> OperationEventReplayRequest {
    OperationEventReplayRequest {
        operation_id,
        start_sequence: EventSequence::first(),
        limit: OperationEventReplayLimit::try_new(MAX_OPERATION_EVENT_REPLAY_LIMIT)
            .expect("max replay limit is valid"),
    }
}
