use crate::core::command::{CorePromoteCommand, CoreReplaceCommand};
use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_support::{PloyzctlExecutionError, PloyzctlExecutionOutput};

pub(crate) async fn promote(
    command: CorePromoteCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    crate::machine::runtime::remote::execute_core_promote_remote(command, config).await
}

pub(crate) async fn replace(
    command: CoreReplaceCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    crate::machine::runtime::remote::execute_core_replace_remote(command, config).await
}
