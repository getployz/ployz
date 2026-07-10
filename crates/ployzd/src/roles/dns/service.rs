//! Machine-scoped query service owned by the DNS role.

use std::net::Ipv4Addr;

use ployz_core::ids::MachineId;
use ployz_core::internal_dns::{InternalServiceName, internal_dns_records};
use ployz_core::subjects::MachineServiceEndpoint;
use ployz_nats::service_runtime::{
    NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError, RunningNatsService,
    decode_json_request, start_nats_service,
};
use serde::{Deserialize, Serialize};

use crate::fact_cache::FactCache;
use crate::service_catalog::{dns_role_service_base, machine_endpoint_spec};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DnsResolveRpcRequest {
    pub name: InternalServiceName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DnsResolveRpcOk {
    pub machine_id: MachineId,
    pub name: InternalServiceName,
    pub addresses: Vec<Ipv4Addr>,
}

pub(crate) async fn start_dns_role_service(
    client: async_nats::Client,
    machine_id: MachineId,
    facts: FactCache,
) -> Result<RunningNatsService, NatsServiceRuntimeError> {
    let mut service = start_nats_service(client, &dns_role_service_base(&machine_id)).await?;
    let endpoint = machine_endpoint_spec(&machine_id, MachineServiceEndpoint::DnsResolve);
    service
        .bind_endpoint(&endpoint, move |request| {
            let machine_id = machine_id.clone();
            let facts = facts.clone();
            async move { resolve_from_local_cache(machine_id, facts, request) }
        })
        .await?;
    Ok(service)
}

fn resolve_from_local_cache(
    machine_id: MachineId,
    facts: FactCache,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<DnsResolveRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let addresses = internal_dns_records(&facts.machine_facts_all())
        .remove(&request.name)
        .unwrap_or_default();
    NatsServiceResponse::json_ok(&DnsResolveRpcOk {
        machine_id,
        name: request.name,
        addresses,
    })
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use ployz_core::ids::{NamespaceId, ServiceId};
    use ployz_core::internal_dns::InternalServiceName;
    use ployz_core::machine_runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
    use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse};
    use ployz_test_support::{containers, ids::machine_id};

    use super::{DnsResolveRpcOk, DnsResolveRpcRequest, resolve_from_local_cache};
    use crate::fact_cache::FactCache;

    #[test]
    fn answers_from_the_dns_roles_local_fact_cache() {
        let dns_machine_id = machine_id("dns_a");
        let observed_machine_id = machine_id("worker_a");
        let container = containers::observation("worker_a", "ctr_web")
            .with(containers::identity("web").namespace("team-a"))
            .running_at(IpAddr::V4(Ipv4Addr::new(10, 42, 1, 8)))
            .build();
        let facts = MachineFactsSnapshot::try_new(
            observed_machine_id.clone(),
            MachineContainerObservationSnapshot::try_new(observed_machine_id, [container])
                .expect("container facts"),
            None,
            ployz_test_support::fixtures::test_disk_space(),
            1,
        )
        .expect("machine facts");
        let cache = FactCache::default();
        cache.record_machine_facts(facts);
        let name = InternalServiceName::new(
            &ServiceId::try_new("web").expect("service id"),
            &NamespaceId::try_new("team-a").expect("namespace id"),
        );

        let response = resolve_from_local_cache(
            dns_machine_id.clone(),
            cache,
            NatsServiceRequest {
                payload: serde_json::to_vec(&DnsResolveRpcRequest { name: name.clone() })
                    .expect("request"),
            },
        );
        let NatsServiceResponse::Ok { payload } = response else {
            panic!("expected answer");
        };
        assert_eq!(
            serde_json::from_slice::<DnsResolveRpcOk>(&payload).expect("response"),
            DnsResolveRpcOk {
                machine_id: dns_machine_id,
                name,
                addresses: vec![Ipv4Addr::new(10, 42, 1, 8)],
            }
        );
    }
}
