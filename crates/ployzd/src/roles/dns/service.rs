//! Machine-scoped query service owned by the DNS role.

use std::sync::{Arc, Mutex};

use crate::role_testimony::RoleTestimonyCache;
use crate::roles::dns::InternalResolverHealth;
use crate::roles::dns::internal::query_bound_resolver;
use crate::roles::dns::protocol::{
    DnsResolveRpcOk, DnsResolveRpcRequest, DnsStatusRpcOk, DnsStatusRpcRequest,
};
use crate::service_catalog::{dns_role_service_base, machine_endpoint_spec};
use ployz_core::ids::MachineId;
use ployz_core::internal_dns::{InternalDnsResolverStatus, InternalDnsStatus};
use ployz_core::subjects::MachineServiceEndpoint;
use ployz_nats::service_runtime::{
    NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError, RunningNatsService,
    decode_json_request, start_nats_service,
};

pub(crate) async fn start_dns_role_service(
    client: async_nats::Client,
    machine_id: MachineId,
    facts: RoleTestimonyCache,
    resolver_health: Option<Arc<Mutex<InternalResolverHealth>>>,
) -> Result<RunningNatsService, NatsServiceRuntimeError> {
    let mut service = start_nats_service(client, &dns_role_service_base(&machine_id)).await?;
    let endpoint = machine_endpoint_spec(&machine_id, MachineServiceEndpoint::DnsResolve);
    let resolve_machine_id = machine_id.clone();
    let resolve_health = resolver_health.clone();
    service
        .bind_endpoint(&endpoint, move |request| {
            let machine_id = resolve_machine_id.clone();
            let resolver_health = resolve_health.clone();
            async move { resolve_from_bound_resolver(machine_id, resolver_health, request).await }
        })
        .await?;
    let endpoint = machine_endpoint_spec(&machine_id, MachineServiceEndpoint::DnsStatus);
    service
        .bind_endpoint(&endpoint, move |request| {
            let machine_id = machine_id.clone();
            let facts = facts.clone();
            let resolver_health = resolver_health.clone();
            async move { status_from_local_cache(machine_id, facts, resolver_health, request) }
        })
        .await?;
    Ok(service)
}

fn status_from_local_cache(
    machine_id: MachineId,
    facts: RoleTestimonyCache,
    resolver_health: Option<Arc<Mutex<InternalResolverHealth>>>,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    if let Err(response) = decode_json_request::<DnsStatusRpcRequest>(&request) {
        return response;
    }
    let resolver = resolver_health
        .map(|health| {
            health
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        })
        .map_or(
            InternalDnsResolverStatus::NotConfigured,
            |health| match health {
                InternalResolverHealth::AwaitingBind { attempts } => {
                    InternalDnsResolverStatus::AwaitingBind { attempts }
                }
                InternalResolverHealth::Serving { bound } => {
                    InternalDnsResolverStatus::Serving { bound }
                }
            },
        );
    NatsServiceResponse::json_ok(&DnsStatusRpcOk {
        machine_id,
        value: InternalDnsStatus {
            resolver,
            fact_watermarks: facts.machine_fact_watermarks(),
        },
    })
}

async fn resolve_from_bound_resolver(
    machine_id: MachineId,
    resolver_health: Option<Arc<Mutex<InternalResolverHealth>>>,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<DnsResolveRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let Some(resolver_health) = resolver_health else {
        return NatsServiceResponse::transport_error(
            ployz_nats::service_protocol::NatsServiceError::unavailable(
                "internal DNS resolver is not configured",
            ),
        );
    };
    let health = resolver_health
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let InternalResolverHealth::Serving { bound } = health else {
        return NatsServiceResponse::transport_error(
            ployz_nats::service_protocol::NatsServiceError::unavailable(
                "internal DNS resolver is not bound",
            ),
        );
    };
    let addresses = match query_bound_resolver(bound, &request.name).await {
        Ok(addresses) => addresses,
        Err(error) => {
            return NatsServiceResponse::transport_error(
                ployz_nats::service_protocol::NatsServiceError::unavailable(error.to_string()),
            );
        }
    };
    NatsServiceResponse::json_ok(&DnsResolveRpcOk {
        machine_id,
        name: request.name,
        addresses,
    })
}

#[cfg(test)]
mod tests {
    use ployz_core::ids::{NamespaceId, ServiceId};
    use ployz_core::internal_dns::{InternalDnsResolverStatus, InternalServiceName};
    use ployz_core::machine_runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
    use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse};
    use ployz_test_support::ids::machine_id;

    use super::{resolve_from_bound_resolver, status_from_local_cache};
    use crate::role_testimony::RoleTestimonyCache;
    use crate::roles::dns::protocol::{DnsResolveRpcRequest, DnsStatusRpcOk, DnsStatusRpcRequest};

    #[tokio::test]
    async fn unconfigured_resolver_is_unavailable() {
        let dns_machine_id = machine_id("dns_a");
        let name = InternalServiceName::try_from_ids(
            &ServiceId::try_new("web").expect("service id"),
            &NamespaceId::try_new("team-a").expect("namespace id"),
        )
        .expect("internal service name");

        let response = resolve_from_bound_resolver(
            dns_machine_id.clone(),
            None,
            NatsServiceRequest {
                headers: None,
                payload: serde_json::to_vec(&DnsResolveRpcRequest { name: name.clone() })
                    .expect("request"),
            },
        )
        .await;
        assert!(matches!(
            response,
            NatsServiceResponse::TransportError { .. }
        ));
    }

    #[test]
    fn status_reports_resolver_health_and_fact_watermarks() {
        let dns_machine_id = machine_id("dns_a");
        let observed_machine_id = machine_id("worker_a");
        let facts = MachineFactsSnapshot::try_new(
            observed_machine_id.clone(),
            MachineContainerObservationSnapshot::try_new(observed_machine_id.clone(), [])
                .expect("container facts"),
            None,
            ployz_test_support::fixtures::test_disk_space(),
            ployz_core::image::OciPlatform::current(),
            42,
        )
        .expect("machine facts");
        let cache = RoleTestimonyCache::default();
        cache.record_machine_facts(facts);

        let response = status_from_local_cache(
            dns_machine_id.clone(),
            cache,
            None,
            NatsServiceRequest {
                headers: None,
                payload: serde_json::to_vec(&DnsStatusRpcRequest {}).expect("request"),
            },
        );
        let NatsServiceResponse::Ok { payload } = response else {
            panic!("expected status");
        };
        let status = serde_json::from_slice::<DnsStatusRpcOk>(&payload).expect("response");
        assert_eq!(status.machine_id, dns_machine_id);
        assert_eq!(
            status.value.resolver,
            InternalDnsResolverStatus::NotConfigured
        );
        let [watermark] = status.value.fact_watermarks.as_slice() else {
            panic!("expected one fact watermark");
        };
        assert_eq!(watermark.machine_id, observed_machine_id);
        assert_eq!(watermark.generation.get(), 1);
    }
}
