//! Machine-local DNS for service names projected directly from machine facts.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ployz_core::dataplane::INTERNAL_DNS_SUFFIX;
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::fact_cache::FactCache;
use crate::roles::dns::records::{InternalServiceName, internal_dns_records};

const DNS_HEADER_LEN: usize = 12;
const DNS_PORT: u16 = 53;
const DNS_TYPE_A: u16 = 1;
const DNS_CLASS_IN: u16 = 1;
const DNS_TTL_SECONDS: u32 = 5;
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(2);
const BIND_RETRY_INITIAL: Duration = Duration::from_millis(250);
const BIND_RETRY_CAP: Duration = Duration::from_secs(30);

/// A bounds-checked first DNS question parsed from a UDP datagram.
#[derive(Debug)]
struct DnsQuery {
    id: u16,
    name: String,
    qtype: u16,
    qclass: u16,
    recursion_desired: bool,
    question: Vec<u8>,
}

/// Parses the first uncompressed DNS question from a packet.
#[must_use]
fn parse_query(packet: &[u8]) -> Option<DnsQuery> {
    if packet.len() < DNS_HEADER_LEN {
        return None;
    }
    let id = read_u16(packet, 0)?;
    let flags = read_u16(packet, 2)?;
    if read_u16(packet, 4)? == 0 {
        return None;
    }

    let mut offset = DNS_HEADER_LEN;
    let mut labels = Vec::new();
    loop {
        let length = usize::from(*packet.get(offset)?);
        offset = offset.checked_add(1)?;
        if length == 0 {
            break;
        }
        if length & 0xc0 != 0 {
            return None;
        }
        let label_end = offset.checked_add(length)?;
        let label = std::str::from_utf8(packet.get(offset..label_end)?).ok()?;
        labels.push(label.to_ascii_lowercase());
        offset = label_end;
    }

    let qtype = read_u16(packet, offset)?;
    offset = offset.checked_add(2)?;
    let qclass = read_u16(packet, offset)?;
    offset = offset.checked_add(2)?;

    Some(DnsQuery {
        id,
        name: labels.join("."),
        qtype,
        qclass,
        recursion_desired: flags & 0x0100 != 0,
        question: packet.get(DNS_HEADER_LEN..offset)?.to_vec(),
    })
}

fn read_u16(packet: &[u8], offset: usize) -> Option<u16> {
    let bytes = packet.get(offset..offset.checked_add(2)?)?;
    let [high, low] = bytes else {
        return None;
    };
    Some(u16::from_be_bytes([*high, *low]))
}

/// DNS response codes emitted by the internal resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DnsRcode {
    NoError,
    ServFail,
}

impl DnsRcode {
    const fn code(self) -> u16 {
        match self {
            Self::NoError => 0,
            Self::ServFail => 2,
        }
    }
}

/// Builds an authoritative A-only DNS response that echoes the first question.
#[must_use]
fn build_response(query: &DnsQuery, rcode: DnsRcode, answers: &[Ipv4Addr]) -> Vec<u8> {
    let answer_count = u16::try_from(answers.len()).unwrap_or(u16::MAX);
    let flags = 0x8000 | 0x0400 | if query.recursion_desired { 0x0100 } else { 0 } | rcode.code();
    let mut response = Vec::new();
    response.extend_from_slice(&query.id.to_be_bytes());
    response.extend_from_slice(&flags.to_be_bytes());
    response.extend_from_slice(&1_u16.to_be_bytes());
    response.extend_from_slice(&answer_count.to_be_bytes());
    response.extend_from_slice(&0_u16.to_be_bytes());
    response.extend_from_slice(&0_u16.to_be_bytes());
    response.extend_from_slice(&query.question);
    for address in answers.iter().take(usize::from(answer_count)) {
        response.extend_from_slice(&[0xc0, 0x0c]);
        response.extend_from_slice(&DNS_TYPE_A.to_be_bytes());
        response.extend_from_slice(&DNS_CLASS_IN.to_be_bytes());
        response.extend_from_slice(&DNS_TTL_SECONDS.to_be_bytes());
        response.extend_from_slice(&4_u16.to_be_bytes());
        response.extend_from_slice(&address.octets());
    }
    response
}

/// Bind and serving state for the machine-local internal resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InternalResolverHealth {
    AwaitingBind { attempts: u64 },
    Serving { bound: SocketAddr },
}

/// Spawns the machine-local resolver. It reads only [`FactCache`], which is
/// fed by machine fact broadcasts and retains each machine's last snapshot;
/// it never reads core intent or gates records on health or gateway status.
pub(super) fn spawn_internal_resolver(
    facts: FactCache,
    bind: SocketAddr,
    mut shutdown: broadcast::Receiver<()>,
    health: Arc<Mutex<InternalResolverHealth>>,
) -> JoinHandle<()> {
    let upstream = load_upstream_nameserver();
    tokio::spawn(async move {
        let mut backoff = BIND_RETRY_INITIAL;
        let socket = loop {
            match UdpSocket::bind(bind).await {
                Ok(socket) => break socket,
                Err(error) => {
                    {
                        let mut health = health
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        let InternalResolverHealth::AwaitingBind { attempts } = &mut *health else {
                            unreachable!("resolver can only serve after a successful bind");
                        };
                        *attempts = attempts.saturating_add(1);
                    }
                    eprintln!(
                        "ployzd internal DNS warning: phase=bind address={bind} error={error}"
                    );
                    tokio::select! {
                        () = tokio::time::sleep(backoff) => {
                            backoff = backoff.saturating_mul(2).min(BIND_RETRY_CAP);
                        }
                        _ = shutdown.recv() => return,
                    }
                }
            }
        };
        *health
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            InternalResolverHealth::Serving { bound: bind };
        let socket = Arc::new(socket);
        let mut packet = [0_u8; 4096];
        loop {
            tokio::select! {
                received = socket.recv_from(&mut packet) => {
                    let (length, peer) = match received {
                        Ok(received) => received,
                        Err(error) => {
                            eprintln!("ployzd internal DNS warning: phase=receive error={error}");
                            continue;
                        }
                    };
                    let Some(request) = packet.get(..length) else {
                        eprintln!(
                            "ployzd internal DNS warning: phase=receive invalid_length={length}"
                        );
                        continue;
                    };
                    let request = request.to_vec();
                    let request_facts = facts.clone();
                    let response_socket = Arc::clone(&socket);
                    // ponytail: unbounded task per datagram; add a semaphore if a container ever floods the resolver.
                    tokio::spawn(async move {
                        let Some(response) = response_for_request(&request_facts, upstream, request).await else {
                            return;
                        };
                        if let Err(error) = response_socket.send_to(&response, peer).await {
                            eprintln!(
                                "ployzd internal DNS warning: phase=respond peer={peer} error={error}"
                            );
                        }
                    });
                }
                _ = shutdown.recv() => break,
            }
        }
    })
}

async fn response_for_request(
    facts: &FactCache,
    upstream: IpAddr,
    packet: Vec<u8>,
) -> Option<Vec<u8>> {
    let query = parse_query(&packet)?;
    if query
        .name
        .strip_suffix(INTERNAL_DNS_SUFFIX)
        .is_some_and(|prefix| prefix.ends_with('.'))
    {
        // ponytail: full projection rebuild per query; cache on fact change if a machine's query rate ever matters.
        let records = internal_dns_records(&facts.machine_facts_all());
        let answers = if query.qtype == DNS_TYPE_A && query.qclass == DNS_CLASS_IN {
            InternalServiceName::parse(&query.name)
                .and_then(|name| records.get(&name))
                .map(Vec::as_slice)
                .unwrap_or(&[])
        } else {
            &[]
        };
        return Some(build_response(&query, DnsRcode::NoError, answers));
    }

    match forward_to_upstream(upstream, &packet).await {
        Ok(response) => Some(response),
        Err(error) => {
            eprintln!(
                "ployzd internal DNS warning: phase=forward upstream={upstream} error={error}"
            );
            Some(build_response(&query, DnsRcode::ServFail, &[]))
        }
    }
}

async fn forward_to_upstream(upstream: IpAddr, packet: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let local = match upstream {
        IpAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
        IpAddr::V6(_) => SocketAddr::from(([0_u16; 8], 0)),
    };
    let socket = UdpSocket::bind(local).await?;
    socket.connect(SocketAddr::new(upstream, DNS_PORT)).await?;
    socket.send(packet).await?;
    let mut response = vec![0_u8; 4096];
    let length = tokio::time::timeout(UPSTREAM_TIMEOUT, socket.recv(&mut response))
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "DNS upstream timed out")
        })??;
    response.truncate(length);
    Ok(response)
}

fn load_upstream_nameserver() -> IpAddr {
    // ponytail: resolv.conf plus a public fallback is the ceiling; per-machine
    // upstream configuration is the upgrade path.
    std::fs::read_to_string("/etc/resolv.conf")
        .ok()
        .and_then(|contents| {
            contents.lines().find_map(|line| {
                let mut fields = line.split_whitespace();
                if fields.next()? != "nameserver" {
                    return None;
                }
                fields.next()?.parse::<IpAddr>().ok()
            })
        })
        .filter(|address| !address.is_loopback())
        .unwrap_or(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::{
        ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId,
        StepId,
    };
    use ployz_core::machine_runtime::{
        ContainerHealth, ContainerRuntimeState, MachineContainerObservationSnapshot,
        MachineDiskSpace, MachineFactsSnapshot, ManagedContainerIdentity, ManagedContainerKind,
        ManagedContainerObservation,
    };
    use std::collections::BTreeMap;

    #[test]
    fn projection_sorts_and_deduplicates_running_service_ipv4_addresses() {
        let snapshots = [facts(
            "machine_a",
            [
                observation("ctr_1", ManagedContainerKind::Service, running("10.42.2.8")),
                observation("ctr_2", ManagedContainerKind::Service, running("10.42.1.9")),
                observation("ctr_3", ManagedContainerKind::Service, running("10.42.2.8")),
            ],
        )];

        assert_eq!(
            internal_dns_records(&snapshots),
            BTreeMap::from([(
                internal_name(),
                vec![Ipv4Addr::new(10, 42, 1, 9), Ipv4Addr::new(10, 42, 2, 8)]
            )])
        );
    }

    #[test]
    fn projection_excludes_exited_unaddressed_ipv6_and_non_service_containers() {
        let snapshots = [facts(
            "machine_a",
            [
                observation(
                    "ctr_1",
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::Exited,
                ),
                observation(
                    "ctr_2",
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::running_unroutable(),
                ),
                observation(
                    "ctr_3",
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::running_at("2001:db8::1".parse().expect("valid IPv6")),
                ),
                observation("ctr_4", ManagedContainerKind::Job, running("10.42.1.9")),
            ],
        )];

        assert!(internal_dns_records(&snapshots).is_empty());
    }

    #[test]
    fn projection_does_not_gate_running_service_on_health() {
        let snapshots = [facts(
            "machine_a",
            [observation(
                "ctr_1",
                ManagedContainerKind::Service,
                ContainerRuntimeState::running_at_with_health(
                    Some(IpAddr::V4(Ipv4Addr::new(10, 42, 1, 9))),
                    ContainerHealth::Unhealthy,
                ),
            )],
        )];

        let expected = [Ipv4Addr::new(10, 42, 1, 9)];
        assert_eq!(
            internal_dns_records(&snapshots)
                .get(&internal_name())
                .map(Vec::as_slice),
            Some(expected.as_slice())
        );
    }

    #[test]
    fn projection_removes_service_missing_from_fresh_facts() {
        let present = [facts(
            "machine_a",
            [observation(
                "ctr_1",
                ManagedContainerKind::Service,
                running("10.42.1.9"),
            )],
        )];
        assert!(internal_dns_records(&present).contains_key(&internal_name()));

        let absent = [facts("machine_a", [])];
        assert!(!internal_dns_records(&absent).contains_key(&internal_name()));
    }

    #[test]
    fn parser_reads_first_question_and_lowercases_name() {
        let packet = query_packet("DB.Default.Internal", DNS_TYPE_A);
        let query = parse_query(&packet).expect("valid query");

        assert_eq!(
            (query.id, query.name.as_str(), query.qtype, query.qclass),
            (0x1234, "db.default.internal", DNS_TYPE_A, DNS_CLASS_IN)
        );
    }

    #[test]
    fn parser_rejects_compressed_question_name() {
        let mut compressed = query_packet("db.default.internal", DNS_TYPE_A);
        let Some(length) = compressed.get_mut(DNS_HEADER_LEN) else {
            panic!("test query has a first label");
        };
        *length = 0xc0;

        assert!(parse_query(&compressed).is_none());
    }

    #[test]
    fn parser_rejects_truncated_question() {
        let mut truncated = query_packet("db.default.internal", DNS_TYPE_A);
        truncated.pop();

        assert!(parse_query(&truncated).is_none());
    }

    #[tokio::test]
    async fn a_query_for_unknown_internal_name_returns_noerror_without_answers() {
        let packet = query_packet("missing.default.internal", DNS_TYPE_A);
        let response = response_for_request(
            &FactCache::default(),
            IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
            packet,
        )
        .await
        .expect("internal query has a response");

        assert_eq!(response_header(&response), (0x8500, 0));
    }

    #[tokio::test]
    async fn aaaa_query_for_known_internal_name_returns_noerror_without_answers() {
        let cache = FactCache::default();
        cache.record_machine_facts(facts(
            "machine_a",
            [observation(
                "ctr_1",
                ManagedContainerKind::Service,
                running("10.42.2.8"),
            )],
        ));
        let packet = query_packet("db.default.internal", 28);
        let response = response_for_request(&cache, IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), packet)
            .await
            .expect("internal query has a response");

        assert_eq!(response_header(&response), (0x8500, 0));
    }

    #[tokio::test]
    async fn resolver_answers_known_internal_service_over_udp() {
        let reservation = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("reserve UDP port");
        let bind = reservation.local_addr().expect("reserved address");
        drop(reservation);

        let cache = FactCache::default();
        cache.record_machine_facts(facts(
            "machine_a",
            [observation(
                "ctr_1",
                ManagedContainerKind::Service,
                running("10.42.2.8"),
            )],
        ));
        let (shutdown, receiver) = broadcast::channel(1);
        let health = Arc::new(Mutex::new(InternalResolverHealth::AwaitingBind {
            attempts: 0,
        }));
        let task = spawn_internal_resolver(cache, bind, receiver, Arc::clone(&health));
        let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind test client");
        let request = query_packet("db.default.internal", DNS_TYPE_A);

        let mut received_response = None;
        for _ in 0..20 {
            client
                .send_to(&request, bind)
                .await
                .expect("send DNS query");
            let mut packet = [0_u8; 512];
            if let Ok(Ok((length, _))) =
                tokio::time::timeout(Duration::from_millis(100), client.recv_from(&mut packet))
                    .await
            {
                let Some(response) = packet.get(..length) else {
                    panic!("UDP response length fits receive buffer");
                };
                received_response = Some(response.to_vec());
                break;
            }
        }
        let Some(response) = received_response else {
            panic!("resolver did not answer before the test deadline");
        };

        assert_eq!(response_header(&response), (0x8500, 1));
        let expected_address = [10, 42, 2, 8];
        assert_eq!(
            response.get(response.len().saturating_sub(4)..),
            Some(expected_address.as_slice())
        );
        assert_eq!(
            *health.lock().expect("resolver health lock is not poisoned"),
            InternalResolverHealth::Serving { bound: bind }
        );
        let _ = shutdown.send(());
        task.await.expect("resolver task exits");
    }

    fn query_packet(name: &str, qtype: u16) -> Vec<u8> {
        let mut packet = Vec::from([
            0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
        for label in name.split('.') {
            packet.push(u8::try_from(label.len()).expect("test label fits"));
            packet.extend_from_slice(label.as_bytes());
        }
        packet.push(0);
        packet.extend_from_slice(&qtype.to_be_bytes());
        packet.extend_from_slice(&DNS_CLASS_IN.to_be_bytes());
        packet
    }

    fn response_header(response: &[u8]) -> (u16, u16) {
        (
            read_u16(response, 2).expect("response flags"),
            read_u16(response, 6).expect("answer count"),
        )
    }

    fn internal_name() -> InternalServiceName {
        InternalServiceName::new(
            &ServiceId::try_new("db").expect("service id"),
            &NamespaceId::try_new("default").expect("namespace id"),
        )
    }

    fn facts(
        machine: &str,
        containers: impl IntoIterator<Item = ManagedContainerObservation>,
    ) -> MachineFactsSnapshot {
        let machine_id = MachineId::try_new(machine).expect("machine id");
        MachineFactsSnapshot::try_new(
            machine_id.clone(),
            MachineContainerObservationSnapshot::try_new(machine_id, containers)
                .expect("container snapshot"),
            None,
            MachineDiskSpace {
                available_bytes: 40,
                total_bytes: 100,
            },
            1,
        )
        .expect("machine facts")
    }

    fn observation(
        container: &str,
        kind: ManagedContainerKind,
        state: ContainerRuntimeState,
    ) -> ManagedContainerObservation {
        ManagedContainerObservation {
            machine_id: MachineId::try_new("machine_a").expect("machine id"),
            container_id: ContainerId::try_new(container).expect("container id"),
            identity: ManagedContainerIdentity {
                namespace_id: NamespaceId::try_new("default").expect("namespace id"),
                service_id: ServiceId::try_new("db").expect("service id"),
                namespace_revision_entry_id: NamespaceRevisionEntryId::try_new("entry_db")
                    .expect("entry id"),
                operation_id: OperationId::try_new("op_1").expect("operation id"),
                step_id: StepId::try_new("step_1").expect("step id"),
                kind,
            },
            state,
            health_status: None,
            resolved_image_identity: None,
            created_at_unix_seconds: None,
        }
    }

    fn running(address: &str) -> ContainerRuntimeState {
        ContainerRuntimeState::running_at(address.parse().expect("valid IPv4"))
    }
}
