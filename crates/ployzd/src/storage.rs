//! Access to the local volume plugin's storage admission boundary.

use ployz_core::{RpcError, StorageCapacityError};
use serde::{Serialize, de::DeserializeOwned};

/// Ask the socket-activated plugin for a bounded storage operation.
pub(crate) async fn plugin<T: DeserializeOwned>(
    route: &str,
    request: &impl Serialize,
) -> Result<T, RpcError> {
    async {
        reqwest::Client::builder()
            .unix_socket("/run/docker/plugins/ployz.sock")
            .timeout(std::time::Duration::from_secs(120))
            .build()?
            .post(format!("http://localhost/{route}"))
            .json(request)
            .send()
            .await?
            .error_for_status()?
            .json::<Result<T, RpcError>>()
            .await
    }
    .await
    .map_err(|error: reqwest::Error| {
        StorageCapacityError::StorageCapacityUnknown {
            message: error.to_string(),
        }
        .into_rpc_error()
    })?
}
