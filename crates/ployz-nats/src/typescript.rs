//! TypeScript generation with NATS-owned concrete endpoint metadata.

use crate::subjects::{
    OperationApiEndpoint, OperationApiEndpointExecution, RUNTIME_SNAPSHOT_SEED,
    RUNTIME_SNAPSHOT_STREAM,
};
use ployz_sdk_types::typescript::{NatsOperationApiEndpointMetadata, NatsTypescriptMetadata};

#[must_use]
pub fn generated_typescript() -> String {
    ployz_sdk_types::typescript::generated_typescript(NatsTypescriptMetadata {
        runtime_snapshot_seed: RUNTIME_SNAPSHOT_SEED,
        runtime_snapshot_stream: RUNTIME_SNAPSHOT_STREAM,
        operation_api_endpoint,
    })
}

fn operation_api_endpoint(
    endpoint: ployz_core::subjects::OperationApiEndpoint,
) -> NatsOperationApiEndpointMetadata {
    let endpoint = OperationApiEndpoint::from(endpoint);
    NatsOperationApiEndpointMetadata {
        name: endpoint.name(),
        subject: endpoint.subject(),
        execution: match endpoint.execution() {
            OperationApiEndpointExecution::AcceptsOperation => "accepts_operation",
            OperationApiEndpointExecution::MutatesOperation => "mutates_operation",
            OperationApiEndpointExecution::Query => "query",
        },
    }
}
