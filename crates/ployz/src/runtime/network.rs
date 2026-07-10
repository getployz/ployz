use crate::commands::machine::AcceptedOperationOutput;
use crate::commands::network::{
    NetworkRepairCommand, NetworkResolveCommand, NetworkResolveOutput, NetworkStatusCommand,
    NetworkStatusOutput,
};

use std::time::Duration;

use super::{
    PloyzctlExecutionError, PloyzctlExecutionOutput, PloyzctlRuntimeConfig, api_error,
    operation_api_client, watch_accepted_operation,
};

/// Client request budget covering the daemon's 30s per-RPC machine gather for a
/// network query. `--probe` adds per-peer path-MTU probing, so it needs more.
const NETWORK_QUERY_TIMEOUT: Duration = Duration::from_secs(45);
const NETWORK_PROBE_TIMEOUT: Duration = Duration::from_secs(75);

pub(super) enum NetworkRuntimeCommand {
    Status(NetworkStatusCommand),
    Resolve(NetworkResolveCommand),
    Repair(NetworkRepairCommand),
}

pub(super) async fn execute(
    command: NetworkRuntimeCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    match command {
        NetworkRuntimeCommand::Status(command) => {
            let timeout = if command.probe {
                NETWORK_PROBE_TIMEOUT
            } else {
                NETWORK_QUERY_TIMEOUT
            };
            let api = operation_api_client(config)
                .await?
                .with_request_timeout(timeout);
            let result = api
                .network_status(&command.into_request())
                .await
                .map_err(api_error)?;
            Ok(PloyzctlExecutionOutput::stdout(
                NetworkStatusOutput::from_result(result).render(),
            ))
        }
        NetworkRuntimeCommand::Resolve(command) => {
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
        NetworkRuntimeCommand::Repair(command) => {
            let detach = command.detach;
            let api = operation_api_client(config).await?;
            let accepted = api
                .network_repair(&command.into_request())
                .await
                .map_err(api_error)?;
            if detach {
                return Ok(PloyzctlExecutionOutput::stdout(
                    AcceptedOperationOutput::from_accepted(accepted).render(),
                ));
            }
            watch_accepted_operation(&api, accepted.operation_id, config).await
        }
    }
}
