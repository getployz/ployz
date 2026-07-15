use crate::network::command::{
    NetworkRepairCommand, NetworkRepairOutput, NetworkResolveCommand, NetworkResolveOutput,
    NetworkStatusCommand, NetworkStatusOutput,
};

use std::time::Duration;

use crate::execution_support::{
    PloyzctlExecutionError, PloyzctlExecutionOutput, api_error, operation_api_client,
};
use crate::operation::runtime::watch_accepted;
use crate::runtime::PloyzctlRuntimeConfig;

/// Client request budget covering the daemon's 30s per-RPC machine gather for a
/// network query. `--probe` adds per-peer path-MTU probing, so it needs more.
const NETWORK_QUERY_TIMEOUT: Duration = Duration::from_secs(45);
const NETWORK_PROBE_TIMEOUT: Duration = Duration::from_secs(75);

pub(crate) async fn status(
    command: NetworkStatusCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let mode = command.mode;
    let timeout = if command.mode.probes_path_mtu() {
        NETWORK_PROBE_TIMEOUT
    } else {
        NETWORK_QUERY_TIMEOUT
    };
    let api = operation_api_client(config)
        .await?
        .with_request_timeout(timeout);
    let mut request = command.into_request();
    let mut machines = Vec::new();
    loop {
        let page = api.network_status(&request).await.map_err(api_error)?;
        let ployz_sdk_types::NetworkStatusResult {
            snapshot,
            machines: page_machines,
            next_cursor,
        } = page;
        machines.extend(page_machines);
        let Some(next_cursor) = next_cursor else {
            break;
        };
        request = ployz_sdk_types::NetworkStatusRequest::Continuation {
            mode,
            snapshot,
            after: next_cursor,
        };
    }
    Ok(PloyzctlExecutionOutput::stdout(
        NetworkStatusOutput { machines }.render(),
    ))
}

pub(crate) async fn resolve(
    command: NetworkResolveCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let api = operation_api_client(config)
        .await?
        .with_request_timeout(NETWORK_QUERY_TIMEOUT);
    let result = api
        .network_resolve(&command.into_request())
        .await
        .map_err(api_error)?;
    Ok(PloyzctlExecutionOutput::stdout(
        NetworkResolveOutput::new(result).render(),
    ))
}

pub(crate) async fn repair(
    command: NetworkRepairCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let detach = command.detach;
    let api = operation_api_client(config).await?;
    let accepted = api
        .network_repair(&command.into_request())
        .await
        .map_err(api_error)?;
    if detach {
        return Ok(PloyzctlExecutionOutput::stdout(
            NetworkRepairOutput::from_accepted(accepted).render(),
        ));
    }
    watch_accepted(&api, accepted.operation_id, config).await
}
