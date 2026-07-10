//! Intent-driven gather of each DNS role's actual local resolver answer.

use std::time::Duration;

use futures_util::{StreamExt, stream};
use ployz_core::dataplane::INTERNAL_DNS_SUFFIX;
use ployz_core::ids::MachineId;
use ployz_core::internal_dns::InternalServiceName;
use ployz_core::state::ActiveMachineState;
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::service_runtime::request_json;
use ployz_sdk_types::{
    NetworkDataplaneTestimony, NetworkInternalDnsTestimony, NetworkResolveError,
    NetworkResolveMachineTestimony, NetworkResolveRequest, NetworkResolveResult,
    NetworkStatusError, NetworkStatusMachine, NetworkStatusRequest, NetworkStatusResult,
};

use crate::intent::service::NatsIntentReader;
use crate::roles::dns::service::{
    DnsResolveRpcOk, DnsResolveRpcRequest, DnsStatusRpcOk, DnsStatusRpcRequest,
};
use crate::roles::machine::client::DEFAULT_MACHINE_RPC_TIMEOUT;
use crate::roles::machine::protocol::{
    MachineDataplaneStatusRpcOk, MachineDataplaneStatusRpcRequest,
    MachineDataplaneStatusRpcResponse, MachineRpcResponse,
};

const MAX_CONCURRENT_DNS_READS: usize = 16;

#[derive(Clone)]
pub struct NetworkQueryService {
    intent_reader: NatsIntentReader,
    client: async_nats::Client,
    request_timeout: Duration,
}

impl NetworkQueryService {
    #[must_use]
    pub(crate) const fn new(intent_reader: NatsIntentReader, client: async_nats::Client) -> Self {
        Self {
            intent_reader,
            client,
            request_timeout: DEFAULT_MACHINE_RPC_TIMEOUT,
        }
    }

    pub(crate) async fn resolve(
        &self,
        request: NetworkResolveRequest,
    ) -> Result<NetworkResolveResult, NetworkResolveError> {
        let Some(name) = normalize_internal_name(&request.name) else {
            return Err(NetworkResolveError::InvalidName { name: request.name });
        };
        let intent = self.intent_reader.intent().await.map_err(|error| {
            NetworkResolveError::Unavailable {
                message: error.to_string(),
            }
        })?;
        let machine_ids = intent
            .active_machines
            .into_iter()
            .map(|machine| machine.machine_id)
            .collect::<Vec<_>>();
        let machines =
            gather_dns_answers(&self.client, self.request_timeout, &name, machine_ids).await;
        Ok(NetworkResolveResult { name, machines })
    }

    pub(crate) async fn status(
        &self,
        request: NetworkStatusRequest,
    ) -> Result<NetworkStatusResult, NetworkStatusError> {
        let intent =
            self.intent_reader
                .intent()
                .await
                .map_err(|error| NetworkStatusError::Unavailable {
                    message: error.to_string(),
                })?;
        let machines = gather_network_status(
            &self.client,
            self.request_timeout,
            request.probe,
            intent.active_machines,
        )
        .await;
        Ok(NetworkStatusResult { machines })
    }
}

async fn gather_network_status(
    client: &async_nats::Client,
    request_timeout: Duration,
    probe: bool,
    active_machines: Vec<ActiveMachineState>,
) -> Vec<NetworkStatusMachine> {
    let mut reads = stream::iter(active_machines)
        .map(|active| async move {
            let machine_id = &active.machine_id;
            let dataplane_request = MachineDataplaneStatusRpcRequest { probe };
            let dns_request = DnsStatusRpcRequest {};
            let dataplane = request_json::<_, MachineDataplaneStatusRpcResponse>(
                client,
                machine_service(machine_id, MachineServiceEndpoint::DataplaneStatus),
                &dataplane_request,
                request_timeout,
            );
            let dns = request_json::<_, DnsStatusRpcOk>(
                client,
                machine_service(machine_id, MachineServiceEndpoint::DnsStatus),
                &dns_request,
                request_timeout,
            );
            let (dataplane, dns) = tokio::join!(dataplane, dns);
            let dataplane = match dataplane {
                Ok(MachineRpcResponse::Ok(MachineDataplaneStatusRpcOk {
                    machine_id: responder,
                    value,
                })) if responder == *machine_id => NetworkDataplaneTestimony::Answered { value },
                Ok(MachineRpcResponse::Ok(_))
                | Ok(MachineRpcResponse::DomainError { .. })
                | Err(_) => NetworkDataplaneTestimony::NoAnswer,
            };
            let internal_dns = match dns {
                Ok(DnsStatusRpcOk {
                    machine_id: responder,
                    value,
                }) if responder == *machine_id => NetworkInternalDnsTestimony::Answered { value },
                Ok(_) | Err(_) => NetworkInternalDnsTestimony::NoAnswer,
            };
            NetworkStatusMachine {
                active,
                dataplane,
                internal_dns,
            }
        })
        .buffer_unordered(MAX_CONCURRENT_DNS_READS);
    let mut gathered = Vec::new();
    while let Some(machine) = reads.next().await {
        gathered.push(machine);
    }
    gathered.sort_by(|left, right| left.active.machine_id.cmp(&right.active.machine_id));
    gathered
}

fn normalize_internal_name(name: &str) -> Option<InternalServiceName> {
    let labels = name.split('.').collect::<Vec<_>>();
    let normalized = match labels.as_slice() {
        [service] => format!("{service}.default.{INTERNAL_DNS_SUFFIX}"),
        // A bare `<service>.internal` is ambiguous: it could be a namespace
        // named `internal` or a fully qualified name missing its namespace.
        [_, namespace] if namespace.eq_ignore_ascii_case(INTERNAL_DNS_SUFFIX) => return None,
        [service, namespace] => format!("{service}.{namespace}.{INTERNAL_DNS_SUFFIX}"),
        [service, namespace, suffix] if suffix.eq_ignore_ascii_case(INTERNAL_DNS_SUFFIX) => {
            format!("{service}.{namespace}.{INTERNAL_DNS_SUFFIX}")
        }
        [] | [_, _, _] | [_, _, _, ..] => return None,
    };
    InternalServiceName::try_new(normalized).ok()
}

async fn gather_dns_answers(
    client: &async_nats::Client,
    request_timeout: Duration,
    name: &InternalServiceName,
    machine_ids: Vec<MachineId>,
) -> Vec<NetworkResolveMachineTestimony> {
    let mut answers = stream::iter(machine_ids)
        .map(|machine_id| async move {
            let response = request_json::<_, DnsResolveRpcOk>(
                client,
                machine_service(&machine_id, MachineServiceEndpoint::DnsResolve),
                &DnsResolveRpcRequest { name: name.clone() },
                request_timeout,
            )
            .await;
            match response {
                Ok(answer) if answer.machine_id == machine_id && answer.name == *name => {
                    NetworkResolveMachineTestimony::Answered {
                        machine_id,
                        name: answer.name,
                        addresses: answer.addresses,
                    }
                }
                Ok(_) | Err(_) => NetworkResolveMachineTestimony::NoAnswer { machine_id },
            }
        })
        .buffer_unordered(MAX_CONCURRENT_DNS_READS);

    let mut gathered = Vec::new();
    while let Some(answer) = answers.next().await {
        gathered.push(answer);
    }
    gathered.sort_by(|left, right| testimony_machine_id(left).cmp(testimony_machine_id(right)));
    gathered
}

const fn testimony_machine_id(testimony: &NetworkResolveMachineTestimony) -> &MachineId {
    match testimony {
        NetworkResolveMachineTestimony::Answered { machine_id, .. }
        | NetworkResolveMachineTestimony::NoAnswer { machine_id } => machine_id,
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;

    use futures_util::StreamExt;
    use ployz_core::dataplane::{
        EbpfAttachmentStatus, MachineDataplaneStatus, MachineEndpointSubnet,
        WireGuardConfiguredMtu, WireGuardDetectedMtu, WireGuardStatus,
    };
    use ployz_core::machine::MachineName;
    use ployz_core::machine_runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
    use ployz_core::state::{ActiveMachineState, MachineLifecycle};
    use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
    use ployz_sdk_types::{
        NetworkDataplaneTestimony, NetworkInternalDnsTestimony, NetworkResolveMachineTestimony,
    };
    use ployz_test_support::{
        containers,
        ids::{machine_id, operation_id},
    };

    use super::{gather_dns_answers, gather_network_status, normalize_internal_name};
    use crate::fact_cache::FactCache;
    use crate::roles::dns::service::start_dns_role_service;
    use crate::roles::machine::protocol::{
        MachineDataplaneStatusRpcOk, MachineDataplaneStatusRpcResponse,
    };

    #[test]
    fn normalizes_supported_name_forms() {
        let cases = [
            ("db", "db.default.internal"),
            ("db.team-a", "db.team-a.internal"),
            ("DB.Team-A.Internal", "db.team-a.internal"),
        ];

        for (query, expected) in cases {
            assert_eq!(
                normalize_internal_name(query)
                    .expect("supported internal name")
                    .as_str(),
                expected
            );
        }
    }

    #[test]
    fn rejects_invalid_name() {
        assert!(normalize_internal_name("db.team-a.internal.extra").is_none());
        assert!(normalize_internal_name("db.internal").is_none());
        assert!(normalize_internal_name("db.Internal").is_none());
    }

    #[tokio::test]
    async fn gathers_each_intended_dns_role_and_keeps_silence() {
        let nats = ployz_test_support::nats::TestNats::start_with_machines(&[
            machine_id("dns_a"),
            machine_id("dns_b"),
        ])
        .await;
        let cache = FactCache::default();
        let observed_machine_id = machine_id("worker_a");
        let container = containers::observation("worker_a", "ctr_web")
            .with(containers::identity("web").namespace("team-a"))
            .running_at(IpAddr::V4(Ipv4Addr::new(10, 42, 1, 8)))
            .build();
        cache.record_machine_facts(
            MachineFactsSnapshot::try_new(
                observed_machine_id.clone(),
                MachineContainerObservationSnapshot::try_new(observed_machine_id, [container])
                    .expect("container facts"),
                None,
                ployz_test_support::fixtures::test_disk_space(),
                1,
            )
            .expect("machine facts"),
        );
        let service = start_dns_role_service(
            nats.machine_client(&machine_id("dns_a")).await,
            machine_id("dns_a"),
            cache,
            None,
        )
        .await
        .expect("DNS role service");
        let name = normalize_internal_name("web.team-a").expect("internal name");

        let answers = gather_dns_answers(
            &nats.controller,
            Duration::from_millis(100),
            &name,
            vec![machine_id("dns_a"), machine_id("dns_b")],
        )
        .await;

        assert_eq!(
            answers,
            vec![
                NetworkResolveMachineTestimony::Answered {
                    machine_id: machine_id("dns_a"),
                    name,
                    addresses: vec![Ipv4Addr::new(10, 42, 1, 8)],
                },
                NetworkResolveMachineTestimony::NoAnswer {
                    machine_id: machine_id("dns_b"),
                },
            ]
        );
        service.shutdown().await.expect("shutdown DNS role service");
    }

    #[tokio::test]
    async fn status_keeps_independent_dataplane_and_dns_silence() {
        let machine_a = machine_id("machine_a");
        let machine_b = machine_id("machine_b");
        let nats = ployz_test_support::nats::TestNats::start_with_machines(&[
            machine_a.clone(),
            machine_b.clone(),
        ])
        .await;
        let dns_service = start_dns_role_service(
            nats.machine_client(&machine_a).await,
            machine_a.clone(),
            FactCache::default(),
            None,
        )
        .await
        .expect("DNS role service");

        let machine_client = nats.machine_client(&machine_b).await;
        let mut requests = machine_client
            .subscribe(machine_service(
                &machine_b,
                MachineServiceEndpoint::DataplaneStatus,
            ))
            .await
            .expect("subscribe dataplane status");
        let responder_machine = machine_b.clone();
        let responder = tokio::spawn(async move {
            let request = requests.next().await.expect("status request");
            let reply = request.reply.expect("reply subject");
            let response = MachineDataplaneStatusRpcResponse::Ok(MachineDataplaneStatusRpcOk {
                machine_id: responder_machine,
                value: MachineDataplaneStatus {
                    wireguard: WireGuardStatus {
                        interface: "ployz-wg0".to_owned(),
                        configured_mtu: WireGuardConfiguredMtu::Auto,
                        detected_mtu: WireGuardDetectedMtu::Detected { mtu: 1420 },
                        interface_mtu: Some(1420),
                        peers: Vec::new(),
                    },
                    ebpf_attachment: EbpfAttachmentStatus::Attached,
                },
            });
            machine_client
                .publish(
                    reply,
                    serde_json::to_vec(&response).expect("response").into(),
                )
                .await
                .expect("publish response");
        });

        let result = gather_network_status(
            &nats.controller,
            Duration::from_millis(200),
            true,
            vec![
                active_machine("machine_a", "10.198.1.0/24"),
                active_machine("machine_b", "10.198.2.0/24"),
            ],
        )
        .await;

        let [machine_a, machine_b] = result.as_slice() else {
            panic!("expected two intended machines");
        };
        assert!(matches!(
            machine_a.dataplane,
            NetworkDataplaneTestimony::NoAnswer
        ));
        assert!(matches!(
            machine_a.internal_dns,
            NetworkInternalDnsTestimony::Answered { .. }
        ));
        assert!(matches!(
            machine_b.dataplane,
            NetworkDataplaneTestimony::Answered { .. }
        ));
        assert!(matches!(
            machine_b.internal_dns,
            NetworkInternalDnsTestimony::NoAnswer
        ));
        responder.await.expect("dataplane responder");
        dns_service.shutdown().await.expect("shutdown DNS service");
    }

    fn active_machine(id: &str, subnet: &str) -> ActiveMachineState {
        ActiveMachineState {
            machine_id: machine_id(id),
            name: MachineName::try_new(id).expect("machine name"),
            activated_by: operation_id(&format!("op_{id}")),
            lifecycle: MachineLifecycle::Active,
            control_endpoints: Vec::new(),
            mesh_endpoints: Vec::new(),
            endpoint_subnet: MachineEndpointSubnet::try_new(subnet).expect("endpoint subnet"),
        }
    }
}
