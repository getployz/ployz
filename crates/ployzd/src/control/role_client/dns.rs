//! Request-side adapter for DNS-role testimony.

use std::time::Duration;

use ployz_core::ids::MachineId;
use ployz_core::internal_dns::InternalServiceName;
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::service_runtime::{NatsJsonServiceRequestError, request_json};

use crate::roles::dns::protocol::{
    DnsResolveRpcOk, DnsResolveRpcRequest, DnsStatusRpcOk, DnsStatusRpcRequest,
};

#[derive(Debug, Clone)]
pub(crate) struct NatsDnsClient {
    client: async_nats::Client,
}

impl NatsDnsClient {
    #[must_use]
    pub(crate) fn new(client: async_nats::Client) -> Self {
        Self { client }
    }

    pub(crate) async fn resolve(
        &self,
        machine_id: &MachineId,
        name: &InternalServiceName,
        timeout: Duration,
    ) -> Result<DnsResolveRpcOk, NatsJsonServiceRequestError> {
        request_json(
            &self.client,
            machine_service(machine_id, MachineServiceEndpoint::DnsResolve),
            &DnsResolveRpcRequest { name: name.clone() },
            timeout,
        )
        .await
    }

    pub(crate) async fn status(
        &self,
        machine_id: &MachineId,
        timeout: Duration,
    ) -> Result<DnsStatusRpcOk, NatsJsonServiceRequestError> {
        request_json(
            &self.client,
            machine_service(machine_id, MachineServiceEndpoint::DnsStatus),
            &DnsStatusRpcRequest {},
            timeout,
        )
        .await
    }
}
