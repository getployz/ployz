use std::time::Instant;

use tokio::time::sleep as async_sleep;

use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_support::{
    PloyzctlExecutionError, PloyzctlExecutionOutput, api_error, current_unix_seconds,
    operation_api_client, render_api_call,
};
use crate::logs::command::{LogsTailCommand, LogsTailOutput};

pub(crate) async fn tail(
    command: LogsTailCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    if !command.follow {
        return render_api_call(
            config,
            async |api| api.logs_tail(&command.into_request()).await,
            |result| LogsTailOutput::new(result).render(),
        )
        .await;
    }
    let api = operation_api_client(config).await?;
    let started_at = Instant::now();
    let mut output = String::new();
    let mut request = command.clone().into_request();
    loop {
        let next_since = current_unix_seconds();
        let result = api.logs_tail(&request).await.map_err(api_error)?;
        output.push_str(&LogsTailOutput::new(result).render());
        if started_at.elapsed() >= config.ops_watch_timeout() {
            return Ok(PloyzctlExecutionOutput::stdout(output));
        }
        request = command.request_after(next_since);
        async_sleep(config.ops_watch_poll_interval()).await;
    }
}
