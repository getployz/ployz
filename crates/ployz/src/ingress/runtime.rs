use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_support::{
    PloyzctlExecutionError, PloyzctlExecutionOutput, api_error, operation_api_client,
};
use crate::ingress::command::IngressConfigureCommand;

pub(crate) async fn configure(
    command: IngressConfigureCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let api = operation_api_client(config).await?;
    let accepted = api
        .ingress_configure(&command.into_request())
        .await
        .map_err(api_error)?;
    crate::operation::runtime::watch_accepted(&api, accepted.operation_id, config).await
}
