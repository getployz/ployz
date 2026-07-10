//! Intent-driven gather of each DNS role's actual local resolver answer.

use std::time::Duration;

use futures_util::{StreamExt, stream};
use ployz_core::dataplane::{INTERNAL_DNS_SUFFIX, NetworkStatusMode};
use ployz_core::ids::MachineId;
use ployz_core::internal_dns::InternalServiceName;
use ployz_core::state::ActiveMachineState;
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::service_protocol::NatsServiceErrorCode;
use ployz_nats::service_runtime::{
    NatsJsonServiceRequestError, NatsServiceRequestFailure, request_json,
};
use ployz_sdk_types::{
    NetworkDataplaneTestimony, NetworkInternalDnsTestimony, NetworkResolveError,
    NetworkResolveMachineTestimony, NetworkResolveRequest, NetworkResolveResult,
    NetworkStatusError, NetworkStatusMachine, NetworkStatusRequest, NetworkStatusResult,
};

use crate::intent::service::NatsIntentReader;
use crate::roles::dns::service::{
    DnsResolveRpcOk, DnsResolveRpcRequest, DnsStatusRpcOk, DnsStatusRpcRequest,
};
use crate::roles::machine::client::{DEFAULT_MACHINE_RPC_TIMEOUT, MAX_CONCURRENT_MACHINE_READS};
use crate::roles::machine::protocol::{
    MachineDataplaneStatusDomainError, MachineDataplaneStatusRpcOk,
    MachineDataplaneStatusRpcRequest, MachineDataplaneStatusRpcResponse, MachineRpcResponse,
};

const NETWORK_PROBE_GATHER_TIMEOUT: Duration = Duration::from_secs(60);

enum NetworkRequestFailure {
    NoAnswer,
    TimedOut,
    RequestFailed { message: String },
    ProtocolFailed { message: String },
    DecodeFailed { message: String },
}

fn network_request_failure(error: NatsJsonServiceRequestError) -> NetworkRequestFailure {
    match error {
        NatsJsonServiceRequestError::Request {
            failure: NatsServiceRequestFailure::NoResponders,
        } => NetworkRequestFailure::NoAnswer,
        NatsJsonServiceRequestError::Request {
            failure: NatsServiceRequestFailure::TimedOut,
        }
        | NatsJsonServiceRequestError::Service {
            failure:
                ployz_nats::service_protocol::NatsServiceError {
                    code: NatsServiceErrorCode::Timeout,
                    ..
                },
        } => NetworkRequestFailure::TimedOut,
        NatsJsonServiceRequestError::ServiceProtocol { error } => {
            NetworkRequestFailure::ProtocolFailed {
                message: error.to_string(),
            }
        }
        NatsJsonServiceRequestError::DecodeResponse { message } => {
            NetworkRequestFailure::DecodeFailed { message }
        }
        error @ NatsJsonServiceRequestError::EncodeRequest { .. }
        | error @ NatsJsonServiceRequestError::Request {
            failure:
                NatsServiceRequestFailure::InvalidSubject
                | NatsServiceRequestFailure::MaxPayloadExceeded
                | NatsServiceRequestFailure::Other { .. },
        }
        | error @ NatsJsonServiceRequestError::Service {
            failure:
                ployz_nats::service_protocol::NatsServiceError {
                    code:
                        NatsServiceErrorCode::BadRequest
                        | NatsServiceErrorCode::Conflict
                        | NatsServiceErrorCode::Unavailable
                        | NatsServiceErrorCode::Internal,
                    ..
                },
        } => NetworkRequestFailure::RequestFailed {
            message: error.to_string(),
        },
    }
}

#[derive(Clone)]
pub struct NetworkQueryService {
    intent_reader: NatsIntentReader,
    client: async_nats::Client,
}

impl NetworkQueryService {
    #[must_use]
    pub(crate) const fn new(intent_reader: NatsIntentReader, client: async_nats::Client) -> Self {
        Self {
            intent_reader,
            client,
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
        let machines = gather_dns_answers(
            &self.client,
            DEFAULT_MACHINE_RPC_TIMEOUT,
            &name,
            machine_ids,
        )
        .await;
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
        let gather_timeout = match request.mode {
            NetworkStatusMode::Snapshot => DEFAULT_MACHINE_RPC_TIMEOUT,
            NetworkStatusMode::ProbePathMtu => NETWORK_PROBE_GATHER_TIMEOUT,
        };
        let machines = gather_network_status(
            &self.client,
            gather_timeout,
            request.mode,
            intent.active_machines,
        )
        .await;
        Ok(NetworkStatusResult { machines })
    }
}

async fn gather_network_status(
    client: &async_nats::Client,
    gather_timeout: Duration,
    mode: NetworkStatusMode,
    active_machines: Vec<ActiveMachineState>,
) -> Vec<NetworkStatusMachine> {
    let deadline = tokio::time::Instant::now() + gather_timeout;
    let reads = stream::iter(active_machines)
        .map(|active| async move {
            let machine_id = &active.machine_id;
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let dataplane_request = MachineDataplaneStatusRpcRequest { mode };
            let dns_request = DnsStatusRpcRequest {};
            let dataplane = request_json::<_, MachineDataplaneStatusRpcResponse>(
                client,
                machine_service(machine_id, MachineServiceEndpoint::DataplaneStatus),
                &dataplane_request,
                remaining,
            );
            let dns = request_json::<_, DnsStatusRpcOk>(
                client,
                machine_service(machine_id, MachineServiceEndpoint::DnsStatus),
                &dns_request,
                remaining,
            );
            let (dataplane, dns) = tokio::join!(dataplane, dns);
            let dataplane = dataplane_testimony(machine_id, dataplane);
            let internal_dns = dns_status_testimony(machine_id, dns);
            NetworkStatusMachine {
                active,
                dataplane,
                internal_dns,
            }
        })
        .buffer_unordered(MAX_CONCURRENT_MACHINE_READS);
    let mut gathered = reads.collect::<Vec<_>>().await;
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
    gather_timeout: Duration,
    name: &InternalServiceName,
    machine_ids: Vec<MachineId>,
) -> Vec<NetworkResolveMachineTestimony> {
    let deadline = tokio::time::Instant::now() + gather_timeout;
    let answers = stream::iter(machine_ids)
        .map(|machine_id| async move {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let response = request_json::<_, DnsResolveRpcOk>(
                client,
                machine_service(&machine_id, MachineServiceEndpoint::DnsResolve),
                &DnsResolveRpcRequest { name: name.clone() },
                remaining,
            )
            .await;
            dns_resolve_testimony(machine_id, name, response)
        })
        .buffer_unordered(MAX_CONCURRENT_MACHINE_READS);

    let mut gathered = answers.collect::<Vec<_>>().await;
    gathered.sort_by(|left, right| testimony_machine_id(left).cmp(testimony_machine_id(right)));
    gathered
}

fn dns_resolve_testimony(
    machine_id: MachineId,
    name: &InternalServiceName,
    response: Result<DnsResolveRpcOk, NatsJsonServiceRequestError>,
) -> NetworkResolveMachineTestimony {
    match response {
        Ok(answer) if answer.machine_id == machine_id && answer.name == *name => {
            NetworkResolveMachineTestimony::Answered {
                machine_id,
                addresses: answer.addresses,
            }
        }
        Ok(answer) if answer.machine_id != machine_id => {
            NetworkResolveMachineTestimony::WrongResponder {
                machine_id,
                actual_machine_id: answer.machine_id,
            }
        }
        Ok(answer) => NetworkResolveMachineTestimony::ProtocolFailed {
            machine_id,
            message: format!(
                "resolver echoed {}, expected {}",
                answer.name.as_str(),
                name.as_str()
            ),
        },
        Err(error) => dns_resolve_error_testimony(machine_id, error),
    }
}

fn dns_status_testimony(
    expected_machine_id: &MachineId,
    response: Result<DnsStatusRpcOk, NatsJsonServiceRequestError>,
) -> NetworkInternalDnsTestimony {
    match response {
        Ok(DnsStatusRpcOk { machine_id, value }) if machine_id == *expected_machine_id => {
            NetworkInternalDnsTestimony::Answered { value }
        }
        Ok(DnsStatusRpcOk { machine_id, .. }) => NetworkInternalDnsTestimony::WrongResponder {
            actual_machine_id: machine_id,
        },
        Err(error) => match network_request_failure(error) {
            NetworkRequestFailure::NoAnswer => NetworkInternalDnsTestimony::NoAnswer,
            NetworkRequestFailure::TimedOut => NetworkInternalDnsTestimony::TimedOut,
            NetworkRequestFailure::RequestFailed { message } => {
                NetworkInternalDnsTestimony::RequestFailed { message }
            }
            NetworkRequestFailure::ProtocolFailed { message } => {
                NetworkInternalDnsTestimony::ProtocolFailed { message }
            }
            NetworkRequestFailure::DecodeFailed { message } => {
                NetworkInternalDnsTestimony::DecodeFailed { message }
            }
        },
    }
}

fn dns_resolve_error_testimony(
    machine_id: MachineId,
    error: NatsJsonServiceRequestError,
) -> NetworkResolveMachineTestimony {
    match network_request_failure(error) {
        NetworkRequestFailure::NoAnswer => NetworkResolveMachineTestimony::NoAnswer { machine_id },
        NetworkRequestFailure::TimedOut => NetworkResolveMachineTestimony::TimedOut { machine_id },
        NetworkRequestFailure::RequestFailed { message } => {
            NetworkResolveMachineTestimony::RequestFailed {
                machine_id,
                message,
            }
        }
        NetworkRequestFailure::ProtocolFailed { message } => {
            NetworkResolveMachineTestimony::ProtocolFailed {
                machine_id,
                message,
            }
        }
        NetworkRequestFailure::DecodeFailed { message } => {
            NetworkResolveMachineTestimony::DecodeFailed {
                machine_id,
                message,
            }
        }
    }
}

fn dataplane_testimony(
    expected_machine_id: &MachineId,
    response: Result<MachineDataplaneStatusRpcResponse, NatsJsonServiceRequestError>,
) -> NetworkDataplaneTestimony {
    match response {
        Ok(MachineRpcResponse::Ok(MachineDataplaneStatusRpcOk { machine_id, value }))
            if machine_id == *expected_machine_id =>
        {
            NetworkDataplaneTestimony::Answered { value }
        }
        Ok(MachineRpcResponse::Ok(MachineDataplaneStatusRpcOk { machine_id, .. })) => {
            NetworkDataplaneTestimony::WrongResponder {
                actual_machine_id: machine_id,
            }
        }
        Ok(MachineRpcResponse::DomainError { machine_id, .. })
            if machine_id != *expected_machine_id =>
        {
            NetworkDataplaneTestimony::WrongResponder {
                actual_machine_id: machine_id,
            }
        }
        Ok(MachineRpcResponse::DomainError {
            error: MachineDataplaneStatusDomainError::ReadFailed { message },
            ..
        }) => NetworkDataplaneTestimony::ReadFailed { message },
        Err(error) => match network_request_failure(error) {
            NetworkRequestFailure::NoAnswer => NetworkDataplaneTestimony::NoAnswer,
            NetworkRequestFailure::TimedOut => NetworkDataplaneTestimony::TimedOut,
            NetworkRequestFailure::RequestFailed { message } => {
                NetworkDataplaneTestimony::RequestFailed { message }
            }
            NetworkRequestFailure::ProtocolFailed { message } => {
                NetworkDataplaneTestimony::ProtocolFailed { message }
            }
            NetworkRequestFailure::DecodeFailed { message } => {
                NetworkDataplaneTestimony::DecodeFailed { message }
            }
        },
    }
}

const fn testimony_machine_id(testimony: &NetworkResolveMachineTestimony) -> &MachineId {
    match testimony {
        NetworkResolveMachineTestimony::Answered { machine_id, .. }
        | NetworkResolveMachineTestimony::NoAnswer { machine_id }
        | NetworkResolveMachineTestimony::WrongResponder { machine_id, .. }
        | NetworkResolveMachineTestimony::TimedOut { machine_id }
        | NetworkResolveMachineTestimony::RequestFailed { machine_id, .. }
        | NetworkResolveMachineTestimony::ProtocolFailed { machine_id, .. }
        | NetworkResolveMachineTestimony::DecodeFailed { machine_id, .. } => machine_id,
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use futures_util::StreamExt;
    use ployz_core::dataplane::{
        EbpfAttachmentStatus, MachineDataplaneStatus, MachineEndpointSubnet,
        WireGuardConfiguredMtu, WireGuardDetectedMtu, WireGuardInterfaceMtu, WireGuardStatus,
    };
    use ployz_core::internal_dns::{InternalDnsResolverStatus, InternalDnsStatus};
    use ployz_core::machine::MachineName;
    use ployz_core::machine_runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
    use ployz_core::ops::FailureMessage;
    use ployz_core::state::{ActiveMachineState, MachineLifecycle};
    use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
    use ployz_nats::service_protocol::NatsServiceErrorHeaderDecodeError;
    use ployz_nats::service_runtime::{NatsJsonServiceRequestError, NatsServiceRequestFailure};
    use ployz_sdk_types::{
        NetworkDataplaneTestimony, NetworkInternalDnsTestimony, NetworkResolveMachineTestimony,
    };
    use ployz_test_support::{
        containers,
        ids::{machine_id, operation_id},
    };
    use tokio::net::UdpSocket;

    use super::{
        dataplane_testimony, dns_resolve_error_testimony, dns_resolve_testimony,
        dns_status_testimony, gather_dns_answers, gather_network_status, normalize_internal_name,
    };
    use crate::fact_cache::FactCache;
    use crate::roles::dns::InternalResolverHealth;
    use crate::roles::dns::service::{DnsResolveRpcOk, DnsStatusRpcOk, start_dns_role_service};
    use crate::roles::machine::protocol::{
        MachineDataplaneStatusDomainError, MachineDataplaneStatusRpcOk,
        MachineDataplaneStatusRpcResponse, MachineRpcResponse,
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

    #[test]
    fn dataplane_testimony_preserves_failures_and_reserves_no_answer_for_silence() {
        let expected = machine_id("machine_a");
        let read_failed = dataplane_testimony(
            &expected,
            Ok(MachineRpcResponse::DomainError {
                machine_id: expected.clone(),
                error: MachineDataplaneStatusDomainError::ReadFailed {
                    message: FailureMessage::try_new("wireguard read failed")
                        .expect("failure message"),
                },
            }),
        );
        assert!(matches!(
            read_failed,
            NetworkDataplaneTestimony::ReadFailed { .. }
        ));

        let wrong = dataplane_testimony(
            &expected,
            Ok(MachineRpcResponse::DomainError {
                machine_id: machine_id("machine_b"),
                error: MachineDataplaneStatusDomainError::ReadFailed {
                    message: FailureMessage::try_new("wrong responder").expect("failure message"),
                },
            }),
        );
        assert!(matches!(
            wrong,
            NetworkDataplaneTestimony::WrongResponder { .. }
        ));

        let cases = [
            (
                NatsJsonServiceRequestError::Request {
                    failure: NatsServiceRequestFailure::NoResponders,
                },
                NetworkDataplaneTestimony::NoAnswer,
            ),
            (
                NatsJsonServiceRequestError::Request {
                    failure: NatsServiceRequestFailure::TimedOut,
                },
                NetworkDataplaneTestimony::TimedOut,
            ),
            (
                NatsJsonServiceRequestError::DecodeResponse {
                    message: "bad json".to_owned(),
                },
                NetworkDataplaneTestimony::DecodeFailed {
                    message: "bad json".to_owned(),
                },
            ),
            (
                NatsJsonServiceRequestError::ServiceProtocol {
                    error: NatsServiceErrorHeaderDecodeError::MissingMessage,
                },
                NetworkDataplaneTestimony::ProtocolFailed {
                    message: "missing service error message header".to_owned(),
                },
            ),
        ];
        for (error, expected_testimony) in cases {
            assert_eq!(
                dataplane_testimony(&expected, Err(error)),
                expected_testimony
            );
        }

        assert!(matches!(
            dns_resolve_error_testimony(
                expected.clone(),
                NatsJsonServiceRequestError::Request {
                    failure: NatsServiceRequestFailure::NoResponders,
                },
            ),
            NetworkResolveMachineTestimony::NoAnswer { .. }
        ));
        assert!(matches!(
            dns_resolve_error_testimony(
                expected.clone(),
                NatsJsonServiceRequestError::Request {
                    failure: NatsServiceRequestFailure::TimedOut,
                },
            ),
            NetworkResolveMachineTestimony::TimedOut { .. }
        ));
        assert!(matches!(
            dns_resolve_error_testimony(
                expected.clone(),
                NatsJsonServiceRequestError::DecodeResponse {
                    message: "bad json".to_owned(),
                },
            ),
            NetworkResolveMachineTestimony::DecodeFailed { .. }
        ));
        assert!(matches!(
            dns_resolve_error_testimony(
                expected,
                NatsJsonServiceRequestError::ServiceProtocol {
                    error: NatsServiceErrorHeaderDecodeError::MissingMessage,
                },
            ),
            NetworkResolveMachineTestimony::ProtocolFailed { .. }
        ));
    }

    #[test]
    fn dns_testimony_preserves_failures_and_reserves_no_answer_for_silence() {
        let expected = machine_id("machine_a");
        let name = normalize_internal_name("web.team-a").expect("internal name");
        let wrong_status = dns_status_testimony(
            &expected,
            Ok(DnsStatusRpcOk {
                machine_id: machine_id("machine_b"),
                value: InternalDnsStatus {
                    resolver: InternalDnsResolverStatus::NotConfigured,
                    fact_watermarks: Vec::new(),
                },
            }),
        );
        assert!(matches!(
            wrong_status,
            NetworkInternalDnsTestimony::WrongResponder { .. }
        ));

        let wrong_resolve = dns_resolve_testimony(
            expected.clone(),
            &name,
            Ok(DnsResolveRpcOk {
                machine_id: machine_id("machine_b"),
                name: name.clone(),
                addresses: Vec::new(),
            }),
        );
        assert!(matches!(
            wrong_resolve,
            NetworkResolveMachineTestimony::WrongResponder { .. }
        ));

        let cases = [
            (
                NatsJsonServiceRequestError::Request {
                    failure: NatsServiceRequestFailure::NoResponders,
                },
                NetworkInternalDnsTestimony::NoAnswer,
            ),
            (
                NatsJsonServiceRequestError::Request {
                    failure: NatsServiceRequestFailure::TimedOut,
                },
                NetworkInternalDnsTestimony::TimedOut,
            ),
            (
                NatsJsonServiceRequestError::DecodeResponse {
                    message: "bad json".to_owned(),
                },
                NetworkInternalDnsTestimony::DecodeFailed {
                    message: "bad json".to_owned(),
                },
            ),
            (
                NatsJsonServiceRequestError::ServiceProtocol {
                    error: NatsServiceErrorHeaderDecodeError::MissingMessage,
                },
                NetworkInternalDnsTestimony::ProtocolFailed {
                    message: "missing service error message header".to_owned(),
                },
            ),
        ];
        for (error, expected_testimony) in cases {
            assert_eq!(
                dns_status_testimony(&expected, Err(error)),
                expected_testimony
            );
        }
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
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind resolver");
        let bound = socket.local_addr().expect("resolver address");
        let resolver = tokio::spawn(async move {
            let mut request = [0_u8; 512];
            let (length, peer) = socket
                .recv_from(&mut request)
                .await
                .expect("receive DNS query");
            let question = request
                .get(12..length)
                .expect("DNS query includes a question");
            let [id_high, id_low, ..] = request.as_slice() else {
                panic!("DNS query includes a transaction id");
            };
            let mut response = Vec::from([
                *id_high, *id_low, 0x85, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
            ]);
            response.extend_from_slice(question);
            response.extend_from_slice(&[
                0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x05, 0x00, 0x04, 10, 42, 1,
                8,
            ]);
            socket
                .send_to(&response, peer)
                .await
                .expect("send DNS response");
        });
        let service = start_dns_role_service(
            nats.machine_client(&machine_id("dns_a")).await,
            machine_id("dns_a"),
            cache,
            Some(Arc::new(Mutex::new(InternalResolverHealth::Serving {
                bound,
            }))),
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
                    addresses: vec![Ipv4Addr::new(10, 42, 1, 8)],
                },
                NetworkResolveMachineTestimony::NoAnswer {
                    machine_id: machine_id("dns_b"),
                },
            ]
        );
        resolver.await.expect("resolver task");
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
                        interface_mtu: WireGuardInterfaceMtu::Detected { mtu: 1420 },
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
            ployz_core::dataplane::NetworkStatusMode::ProbePathMtu,
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
