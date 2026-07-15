//! Machine-local DNS for service names projected directly from machine facts.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ployz_core::dataplane::INTERNAL_DNS_SUFFIX;
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::role_testimony::RoleTestimonyCache;
use ployz_core::internal_dns::{InternalServiceName, internal_dns_records};
use ployz_core::state::IntentSnapshot;

const DNS_HEADER_LEN: usize = 12;
const DNS_PORT: u16 = 53;
const DNS_TYPE_A: u16 = 1;
const DNS_CLASS_IN: u16 = 1;
const DNS_TTL_SECONDS: u32 = 5;
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(2);
const BIND_RETRY_INITIAL: Duration = Duration::from_millis(250);
const BIND_RETRY_CAP: Duration = Duration::from_secs(30);
const LOCAL_QUERY_TIMEOUT: Duration = Duration::from_millis(500);
const LOCAL_QUERY_ID: u16 = 0x504c;

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

pub(super) async fn query_bound_resolver(
    bound: SocketAddr,
    name: &InternalServiceName,
) -> io::Result<Vec<Ipv4Addr>> {
    tokio::time::timeout(LOCAL_QUERY_TIMEOUT, async {
        let local = match bound {
            SocketAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
            SocketAddr::V6(_) => SocketAddr::from(([0_u16; 8], 0)),
        };
        let socket = UdpSocket::bind(local).await?;
        socket.connect(bound).await?;
        socket.send(&a_query_packet(name)?).await?;
        let mut response = [0_u8; 4096];
        let length = socket.recv(&mut response).await?;
        let Some(response) = response.get(..length) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "DNS response length exceeded buffer",
            ));
        };
        parse_a_response(response)
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "local DNS resolver timed out"))?
}

fn a_query_packet(name: &InternalServiceName) -> io::Result<Vec<u8>> {
    let mut packet = Vec::from([
        0x50, 0x4c, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]);
    for label in name.as_str().split('.') {
        let length = u8::try_from(label.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "DNS label exceeds 255 bytes")
        })?;
        packet.push(length);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0);
    packet.extend_from_slice(&DNS_TYPE_A.to_be_bytes());
    packet.extend_from_slice(&DNS_CLASS_IN.to_be_bytes());
    Ok(packet)
}

fn parse_a_response(response: &[u8]) -> io::Result<Vec<Ipv4Addr>> {
    if read_u16(response, 0) != Some(LOCAL_QUERY_ID) {
        return Err(invalid_dns_response(
            "DNS response transaction id did not match",
        ));
    }
    let flags = response_u16(response, 2)?;
    if flags & 0x8000 == 0 || flags & 0x000f != 0 {
        return Err(invalid_dns_response(
            "DNS resolver returned an unsuccessful response",
        ));
    }
    let question_count = response_u16(response, 4)?;
    let answer_count = response_u16(response, 6)?;
    let mut offset = DNS_HEADER_LEN;
    for _ in 0..question_count {
        offset = skip_dns_name(response, offset)?;
        offset = response
            .get(offset..offset.saturating_add(4))
            .map(|_| offset + 4)
            .ok_or_else(|| invalid_dns_response("DNS question was truncated"))?;
    }

    let mut addresses = Vec::new();
    for _ in 0..answer_count {
        offset = skip_dns_name(response, offset)?;
        let record_type = response_u16(response, offset)?;
        let class = response_u16(response, offset + 2)?;
        let data_length = usize::from(response_u16(response, offset + 8)?);
        offset = offset
            .checked_add(10)
            .ok_or_else(|| invalid_dns_response("DNS answer offset overflowed"))?;
        let data = response
            .get(offset..offset.saturating_add(data_length))
            .ok_or_else(|| invalid_dns_response("DNS answer was truncated"))?;
        if record_type == DNS_TYPE_A && class == DNS_CLASS_IN {
            let [a, b, c, d] = data else {
                return Err(invalid_dns_response(
                    "DNS A record length was not four bytes",
                ));
            };
            addresses.push(Ipv4Addr::new(*a, *b, *c, *d));
        }
        offset = offset
            .checked_add(data_length)
            .ok_or_else(|| invalid_dns_response("DNS answer offset overflowed"))?;
    }
    addresses.sort_unstable();
    addresses.dedup();
    Ok(addresses)
}

fn skip_dns_name(packet: &[u8], mut offset: usize) -> io::Result<usize> {
    loop {
        let length = *packet
            .get(offset)
            .ok_or_else(|| invalid_dns_response("DNS name was truncated"))?;
        if length & 0xc0 == 0xc0 {
            return packet
                .get(offset..offset.saturating_add(2))
                .map(|_| offset + 2)
                .ok_or_else(|| invalid_dns_response("DNS name pointer was truncated"));
        }
        if length & 0xc0 != 0 {
            return Err(invalid_dns_response("DNS name label was invalid"));
        }
        offset = offset
            .checked_add(1)
            .ok_or_else(|| invalid_dns_response("DNS name offset overflowed"))?;
        if length == 0 {
            return Ok(offset);
        }
        offset = offset
            .checked_add(usize::from(length))
            .filter(|end| *end <= packet.len())
            .ok_or_else(|| invalid_dns_response("DNS name label was truncated"))?;
    }
}

fn response_u16(packet: &[u8], offset: usize) -> io::Result<u16> {
    read_u16(packet, offset).ok_or_else(|| invalid_dns_response("DNS response was truncated"))
}

fn invalid_dns_response(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
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

#[derive(Debug, Clone, Default)]
pub(super) struct InternalDnsIntentCache {
    intent: Arc<Mutex<Option<IntentSnapshot>>>,
}

impl InternalDnsIntentCache {
    pub(super) fn record_if_available(&self, intent: Option<IntentSnapshot>) {
        let Some(intent) = intent else {
            return;
        };
        let mut current = self
            .intent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if current
            .as_ref()
            .is_some_and(|current| intent.epoch < current.epoch)
        {
            return;
        }
        *current = Some(intent);
    }

    fn records(
        &self,
        snapshots: &[ployz_core::machine_runtime::MachineFactsSnapshot],
    ) -> std::collections::BTreeMap<InternalServiceName, Vec<Ipv4Addr>> {
        self.intent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|intent| internal_dns_records(intent, snapshots))
            .unwrap_or_default()
    }
}

/// Spawns the machine-local resolver. Queries read cached facts and last-known-good
/// intent; core availability, health, and gateway status are not query dependencies.
pub(super) fn spawn_internal_resolver(
    facts: RoleTestimonyCache,
    intent: InternalDnsIntentCache,
    bind: SocketAddr,
    mut shutdown: broadcast::Receiver<()>,
    health: Arc<Mutex<InternalResolverHealth>>,
) -> JoinHandle<()> {
    let upstream = load_upstream_nameserver(bind.ip());
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
                    let request_intent = intent.clone();
                    let response_socket = Arc::clone(&socket);
                    // ponytail: unbounded task per datagram; add a semaphore if a container ever floods the resolver.
                    tokio::spawn(async move {
                        let Some(response) = response_for_request(&request_facts, &request_intent, upstream, request).await else {
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
    facts: &RoleTestimonyCache,
    intent: &InternalDnsIntentCache,
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
        let records = intent.records(&facts.machine_facts_all());
        let answers = if query.qtype == DNS_TYPE_A && query.qclass == DNS_CLASS_IN {
            InternalServiceName::try_new(&query.name)
                .ok()
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

/// The first resolv.conf `nameserver` usable as an upstream: any address that
/// is not the resolver's own bind. A loopback stub (systemd-resolved, dnsmasq,
/// Docker's embedded resolver) is a valid upstream; only forwarding to our own
/// bind address is a self-loop, so that entry is skipped to the next candidate.
fn upstream_from_resolv_conf(contents: &str, own_bind: IpAddr) -> Option<IpAddr> {
    contents.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        if fields.next()? != "nameserver" {
            return None;
        }
        let address = fields.next()?.parse::<IpAddr>().ok()?;
        (address != own_bind).then_some(address)
    })
}

fn load_upstream_nameserver(own_bind: IpAddr) -> IpAddr {
    // ponytail: resolv.conf plus a public fallback is the ceiling; per-machine
    // upstream configuration is the upgrade path.
    std::fs::read_to_string("/etc/resolv.conf")
        .ok()
        .and_then(|contents| upstream_from_resolv_conf(&contents, own_bind))
        .unwrap_or_else(|| {
            let fallback = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));
            eprintln!(
                "ployzd internal DNS warning: phase=upstream reason=no-usable-resolv-conf-nameserver upstream={fallback}"
            );
            fallback
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::dataplane::MachineEndpointSubnet;
    use ployz_core::ids::{
        ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId,
        StepId,
    };
    use ployz_core::machine_runtime::{
        ContainerRuntimeState, MachineContainerObservationSnapshot, MachineDiskSpace,
        MachineFactsSnapshot, ManagedContainerIdentity, ManagedContainerKind,
        ManagedContainerObservation,
    };
    use ployz_core::roles::InstallRolePolicy;
    use ployz_core::state::{
        ActiveMachineState, ControlPlaneEpoch, IntentSnapshot, MachineLifecycle,
    };
    use ployz_test_support::fixtures::serving_target_entry;
    use ployz_test_support::ids::{machine_id, machine_name, operation_id};

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
            &RoleTestimonyCache::default(),
            &InternalDnsIntentCache::default(),
            IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
            packet,
        )
        .await
        .expect("internal query has a response");

        assert_eq!(response_header(&response), (0x8500, 0));
    }

    #[tokio::test]
    async fn aaaa_query_for_known_internal_name_returns_noerror_without_answers() {
        let cache = RoleTestimonyCache::default();
        cache.record_machine_facts(facts(
            "machine_a",
            [observation(
                "ctr_1",
                ManagedContainerKind::Service,
                running("10.42.2.8"),
            )],
        ));
        let packet = query_packet("db.default.internal", 28);
        let response = response_for_request(
            &cache,
            &internal_dns_intent("machine_a", "entry_db"),
            IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
            packet,
        )
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

        let cache = RoleTestimonyCache::default();
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
        let task = spawn_internal_resolver(
            cache,
            internal_dns_intent("machine_a", "entry_db"),
            bind,
            receiver,
            Arc::clone(&health),
        );
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

    #[test]
    fn upstream_keeps_loopback_stub_and_skips_own_bind() {
        let own_bind = IpAddr::V4(Ipv4Addr::new(10, 42, 0, 1));

        assert_eq!(
            upstream_from_resolv_conf("nameserver 127.0.0.53\n", own_bind),
            Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 53)))
        );
        assert_eq!(
            upstream_from_resolv_conf("nameserver 10.42.0.1\nnameserver 9.9.9.9\n", own_bind),
            Some(IpAddr::V4(Ipv4Addr::new(9, 9, 9, 9)))
        );
        assert_eq!(
            upstream_from_resolv_conf("nameserver 10.42.0.1\n", own_bind),
            None
        );
    }

    #[test]
    fn intent_cache_retains_last_known_good_when_refresh_is_unavailable() {
        let cache = RoleTestimonyCache::default();
        cache.record_machine_facts(facts(
            "machine_a",
            [observation(
                "ctr_1",
                ManagedContainerKind::Service,
                running("10.42.2.8"),
            )],
        ));
        let intent = internal_dns_intent("machine_a", "entry_db");

        intent.record_if_available(None);

        assert_eq!(
            intent.records(&cache.machine_facts_all()),
            std::collections::BTreeMap::from([(
                InternalServiceName::try_new("db.default.internal").expect("internal name"),
                vec![Ipv4Addr::new(10, 42, 2, 8)]
            )])
        );
    }

    #[test]
    fn intent_cache_rejects_a_lower_control_plane_epoch() {
        let intent = InternalDnsIntentCache::default();
        intent.record_if_available(Some(internal_dns_intent_snapshot(
            ControlPlaneEpoch::initial().next(),
            "machine_a",
            "entry_current",
        )));
        intent.record_if_available(Some(internal_dns_intent_snapshot(
            ControlPlaneEpoch::initial(),
            "machine_a",
            "entry_stale",
        )));

        assert_eq!(
            intent
                .intent
                .lock()
                .expect("intent cache lock")
                .as_ref()
                .map(|intent| intent.epoch),
            Some(ControlPlaneEpoch::initial().next())
        );
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
            ployz_core::image::OciPlatform::current(),
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

    fn internal_dns_intent(machine: &str, entry: &str) -> InternalDnsIntentCache {
        let intent = InternalDnsIntentCache::default();
        intent.record_if_available(Some(internal_dns_intent_snapshot(
            ControlPlaneEpoch::initial(),
            machine,
            entry,
        )));
        intent
    }

    fn internal_dns_intent_snapshot(
        epoch: ControlPlaneEpoch,
        machine: &str,
        entry: &str,
    ) -> IntentSnapshot {
        IntentSnapshot {
            epoch,
            core_machine_id: machine_id("machine_a"),
            active_machines: vec![ActiveMachineState {
                machine_id: machine_id(machine),
                name: machine_name(machine),
                activated_by: operation_id("op_activate"),
                roles: InstallRolePolicy::install_all(),
                lifecycle: MachineLifecycle::Active,
                control_endpoints: Vec::new(),
                mesh_endpoints: Vec::new(),
                endpoint_subnet: MachineEndpointSubnet::try_new("10.198.0.0/24")
                    .expect("endpoint subnet"),
                wireguard_public_key: ployz_core::dataplane::WireGuardPublicKey::try_new(format!(
                    "public-{machine}"
                ))
                .expect("public key"),
            }],
            dataplane_projection: ployz_core::dataplane::DataplaneProjection::try_new(
                Vec::new(),
                None,
            )
            .expect("empty projection"),
            route_bindings: Vec::new(),
            serving_target_entries: vec![serving_target_entry("db", entry)],
            volume_pins: Vec::new(),
            nats_authorizations: Vec::new(),
            automatic_hostname_configuration:
                ployz_core::ingress::AutomaticHostnameConfiguration::Ployz,
            ployz_dns_target: ployz_core::ingress::PloyzDnsTargetIntent::Enabled,
            active_certificates: Vec::new(),
        }
    }
}
