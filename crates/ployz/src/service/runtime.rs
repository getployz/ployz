use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_support::{
    PloyzctlExecutionError, PloyzctlExecutionOutput, api_error, operation_api_client,
    render_api_call,
};
use crate::service::command::{
    ServiceInspectCommand, ServiceInspectOutput, ServiceListCommand, ServiceListOutput,
    ServiceRestartCommand,
};

pub(crate) async fn list(
    command: ServiceListCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    render_api_call(
        config,
        async |api| api.service_list(&command.into_request()).await,
        |result| ServiceListOutput::from_result(result).render(),
    )
    .await
}

pub(crate) async fn inspect(
    command: ServiceInspectCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    render_api_call(
        config,
        async |api| api.service_inspect(&command.into_request()).await,
        |service| ServiceInspectOutput::new(service).render(),
    )
    .await
}

pub(crate) async fn restart(
    command: ServiceRestartCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let detach = command.detach;
    let api = operation_api_client(config).await?;
    let accepted = api
        .service_restart(&command.into_request())
        .await
        .map_err(api_error)?;
    if detach {
        return Ok(PloyzctlExecutionOutput::stdout(
            crate::operation::command::AcceptedOperationOutput::from_accepted(accepted).render(),
        ));
    }
    crate::operation::runtime::watch_accepted(&api, accepted.operation_id, config).await
}
