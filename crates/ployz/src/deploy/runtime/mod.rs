pub(crate) mod compose;
pub(crate) mod follow;
pub(crate) mod history;
pub(crate) mod rollback;

use crate::execution_error::PloyzctlExecutionError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployExecutionError {
    #[error("image push failed: {source}")]
    ImagePush {
        source: crate::deploy::image_push::ImagePushError,
    },
    #[error("registry credential lookup failed: {source}")]
    RegistryAuth {
        source: crate::deploy::registry_auth::RegistryAuthError,
    },
    #[error("failed to write deploy progress: {message}")]
    WriteProgress { message: String },
    #[error("could not generate client operation ids: {message}")]
    GenerateClientOperationIds { message: String },
    #[error("{message}")]
    History { message: String },
}

impl From<DeployExecutionError> for PloyzctlExecutionError {
    fn from(error: DeployExecutionError) -> Self {
        Self::Deploy(error)
    }
}
