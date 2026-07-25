//! Machine-local DNS for service names projected directly from machine facts.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hickory_proto::op::{Message, MessageType, OpCode, Query, ResponseCode};
use hickory_proto::rr::rdata::A;
use hickory_proto::rr::{DNSClass, Name, RData, Record, RecordType};
use ployz_core::network::INTERNAL_DNS_SUFFIX;
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use crate::role_testimony::RoleTestimonyCache;
use ployz_core::intent::IntentSnapshot;
use ployz_core::network::internal_dns::{InternalServiceName, internal_dns_records};

const DNS_PORT: u16 = 53;
const DNS_TTL_SECONDS: u32 = 5;
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(2);
const BIND_RETRY_INITIAL: Duration = Duration::from_millis(250);
const BIND_RETRY_CAP: Duration = Duration::from_secs(1);
const BIND_DIAGNOSTIC_REPEAT: Duration = Duration::from_secs(30);
const LOCAL_QUERY_TIMEOUT: Duration = Duration::from_millis(500);
const LOCAL_QUERY_ID: u16 = 0x504c;

/// The first question of a decoded request, with the header fields the
/// response must echo.
#[derive(Debug)]
struct DnsRequest {
    id: u16,
    recursion_desired: bool,
    query: Query,
}

/// Decodes a request datagram and takes its first question; datagrams that are
/// not well-formed DNS queries with at least one question get no response.
#[must_use]
fn parse_request(packet: &[u8]) -> Option<DnsRequest> {
    let message = Message::from_vec(packet).ok()?;
    let query = message.queries.first()?.clone();
    Some(DnsRequest {
        id: message.metadata.id,
        recursion_desired: message.metadata.recursion_desired,
        query,
    })
}

/// The question name in the dotted lowercase form used for record lookup,
/// without the FQDN trailing dot.
#[must_use]
fn lookup_name(query: &Query) -> String {
    let name = query.name.to_ascii().to_ascii_lowercase();
    name.trim_end_matches('.').to_owned()
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
    let name = Name::from_ascii(format!("{}.", name.as_str()))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    let mut query = Message::new(LOCAL_QUERY_ID, MessageType::Query, OpCode::Query);
    query.metadata.recursion_desired = true;
    query.add_query(Query::query(name, RecordType::A));
    query
        .to_vec()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))
}

fn parse_a_response(response: &[u8]) -> io::Result<Vec<Ipv4Addr>> {
    let message = Message::from_vec(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if message.metadata.id != LOCAL_QUERY_ID {
        return Err(invalid_dns_response(
            "DNS response transaction id did not match",
        ));
    }
    if message.metadata.message_type != MessageType::Response
        || message.metadata.response_code != ResponseCode::NoError
    {
        return Err(invalid_dns_response(
            "DNS resolver returned an unsuccessful response",
        ));
    }
    let mut addresses = message
        .answers
        .iter()
        .filter_map(|record| {
            if record.dns_class != DNSClass::IN {
                return None;
            }
            let RData::A(address) = &record.data else {
                return None;
            };
            Some(Ipv4Addr::from(*address))
        })
        .collect::<Vec<_>>();
    addresses.sort_unstable();
    addresses.dedup();
    Ok(addresses)
}

fn invalid_dns_response(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Builds an authoritative A-only DNS response that echoes the first question.
#[must_use]
fn build_response(
    request: &DnsRequest,
    response_code: ResponseCode,
    answers: &[Ipv4Addr],
) -> Option<Vec<u8>> {
    let mut response = Message::response(request.id, OpCode::Query);
    response.metadata.authoritative = true;
    response.metadata.recursion_desired = request.recursion_desired;
    response.metadata.response_code = response_code;
    response.add_query(request.query.clone());
    response.add_answers(answers.iter().map(|address| {
        Record::from_rdata(
            request.query.name.clone(),
            DNS_TTL_SECONDS,
            RData::A(A::from(*address)),
        )
    }));
    response.to_vec().ok()
}

/// Bind and serving state for the machine-local internal resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum InternalResolverState {
    AwaitingBind { attempts: u64 },
    Serving { bound: SocketAddr },
}

#[derive(Debug, Clone)]
pub struct InternalResolverHealth {
    state: watch::Sender<InternalResolverState>,
}

impl InternalResolverHealth {
    #[must_use]
    pub fn awaiting_bind() -> Self {
        let (state, _) = watch::channel(InternalResolverState::AwaitingBind { attempts: 0 });
        Self { state }
    }

    #[must_use]
    #[cfg(test)]
    pub fn serving(bound: SocketAddr) -> Self {
        let (state, _) = watch::channel(InternalResolverState::Serving { bound });
        Self { state }
    }

    pub(super) fn snapshot(&self) -> InternalResolverState {
        self.state.borrow().clone()
    }

    fn record_bind_failure(&self) {
        self.state.send_modify(|state| {
            let InternalResolverState::AwaitingBind { attempts } = state else {
                unreachable!("resolver can only serve after a successful bind");
            };
            *attempts = attempts.saturating_add(1);
        });
    }

    fn record_serving(&self, bound: SocketAddr) {
        self.state
            .send_replace(InternalResolverState::Serving { bound });
    }

    pub(super) async fn await_bound(&self, timeout: Duration) -> Option<SocketAddr> {
        let mut states = self.state.subscribe();
        let Ok(Ok(state)) = tokio::time::timeout(
            timeout,
            states.wait_for(|state| matches!(state, InternalResolverState::Serving { .. })),
        )
        .await
        else {
            return None;
        };
        match &*state {
            InternalResolverState::Serving { bound } => Some(*bound),
            InternalResolverState::AwaitingBind { .. } => None,
        }
    }
}

struct BindFailureDiagnostics {
    last_error: Option<String>,
    next_repeat: tokio::time::Instant,
}

impl BindFailureDiagnostics {
    fn new() -> Self {
        Self {
            last_error: None,
            next_repeat: tokio::time::Instant::now(),
        }
    }

    fn should_report(&mut self, error: &io::Error, now: tokio::time::Instant) -> bool {
        let error = error.to_string();
        if self.last_error.as_ref() == Some(&error) && now < self.next_repeat {
            return false;
        }
        self.last_error = Some(error);
        self.next_repeat = now + BIND_DIAGNOSTIC_REPEAT;
        true
    }
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
        snapshots: &[ployz_core::machine::runtime::MachineFactsSnapshot],
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
    health: InternalResolverHealth,
) -> JoinHandle<()> {
    let upstream = load_upstream_nameserver(bind.ip());
    tokio::spawn(async move {
        let mut backoff = BIND_RETRY_INITIAL;
        let mut diagnostics = BindFailureDiagnostics::new();
        let socket = loop {
            match UdpSocket::bind(bind).await {
                Ok(socket) => break socket,
                Err(error) => {
                    health.record_bind_failure();
                    if diagnostics.should_report(&error, tokio::time::Instant::now()) {
                        eprintln!(
                            "ployzd internal DNS warning: phase=bind address={bind} error={error}"
                        );
                    }
                    tokio::select! {
                        () = tokio::time::sleep(backoff) => {
                            backoff = backoff.saturating_mul(2).min(BIND_RETRY_CAP);
                        }
                        _ = shutdown.recv() => return,
                    }
                }
            }
        };
        health.record_serving(bind);
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
    let request = parse_request(&packet)?;
    let name = lookup_name(&request.query);
    if name
        .strip_suffix(INTERNAL_DNS_SUFFIX)
        .is_some_and(|prefix| prefix.ends_with('.'))
    {
        // ponytail: full projection rebuild per query; cache on fact change if a machine's query rate ever matters.
        let records = intent.records(&facts.machine_facts_all());
        let answers = if request.query.query_type == RecordType::A
            && request.query.query_class == DNSClass::IN
        {
            InternalServiceName::try_new(&name)
                .ok()
                .and_then(|name| records.get(&name))
                .map(Vec::as_slice)
                .unwrap_or(&[])
        } else {
            &[]
        };
        return build_response(&request, ResponseCode::NoError, answers);
    }

    match forward_to_upstream(upstream, &packet).await {
        Ok(response) => Some(response),
        Err(error) => {
            eprintln!(
                "ployzd internal DNS warning: phase=forward upstream={upstream} error={error}"
            );
            build_response(&request, ResponseCode::ServFail, &[])
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
    use ployz_core::ids::{
        ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId,
        StepId,
    };
    use ployz_core::intent::ActiveMachineState;
    use ployz_core::intent::IntentSnapshot;
    use ployz_core::intent::recovery::ControlPlaneEpoch;
    use ployz_core::machine::MachineLifecycle;
    use ployz_core::machine::runtime::{
        ContainerRuntimeState, MachineContainerObservationSnapshot, MachineDiskSpace,
        MachineFactsSnapshot, ManagedContainerIdentity, ManagedContainerKind,
        ManagedContainerObservation,
    };
    use ployz_core::network::MachineEndpointSubnet;
    use ployz_core::roles::InstallRolePolicy;

    use ployz_test_support::fixtures::serving_target_entry;
    use ployz_test_support::ids::{machine_id, machine_name, operation_id};

    const TEST_QTYPE_A: u16 = 1;
    const TEST_QTYPE_AAAA: u16 = 28;
    const TEST_QCLASS_IN: u16 = 1;
    const TEST_HEADER_LEN: usize = 12;

    #[test]
    fn parser_reads_first_question_and_lowercases_lookup_name() {
        let packet = query_packet("DB.Default.Internal", TEST_QTYPE_A);
        let request = parse_request(&packet).expect("valid query");

        assert_eq!(
            (
                request.id,
                lookup_name(&request.query).as_str(),
                request.query.query_type,
                request.query.query_class,
            ),
            (0x1234, "db.default.internal", RecordType::A, DNSClass::IN)
        );
    }

    #[test]
    fn parser_rejects_compressed_question_name() {
        let mut compressed = query_packet("db.default.internal", TEST_QTYPE_A);
        let Some(length) = compressed.get_mut(TEST_HEADER_LEN) else {
            panic!("test query has a first label");
        };
        *length = 0xc0;

        assert!(parse_request(&compressed).is_none());
    }

    #[test]
    fn parser_rejects_truncated_question() {
        let mut truncated = query_packet("db.default.internal", TEST_QTYPE_A);
        truncated.pop();

        assert!(parse_request(&truncated).is_none());
    }

    #[tokio::test]
    async fn a_query_for_unknown_internal_name_returns_noerror_without_answers() {
        let packet = query_packet("missing.default.internal", TEST_QTYPE_A);
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
        let packet = query_packet("db.default.internal", TEST_QTYPE_AAAA);
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
        let health = InternalResolverHealth::awaiting_bind();
        let task = spawn_internal_resolver(
            cache,
            internal_dns_intent("machine_a", "entry_db"),
            bind,
            receiver,
            health.clone(),
        );
        let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind test client");
        let request = query_packet("db.default.internal", TEST_QTYPE_A);

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
            health.snapshot(),
            InternalResolverState::Serving { bound: bind }
        );
        let _ = shutdown.send(());
        task.await.expect("resolver task exits");
    }

    #[tokio::test(start_paused = true)]
    async fn resolver_bind_retry_stays_at_a_one_second_cadence() {
        let reservation = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("reserve UDP port");
        let bind = reservation.local_addr().expect("reserved address");
        let (shutdown, receiver) = broadcast::channel(1);
        let health = InternalResolverHealth::awaiting_bind();
        let task = spawn_internal_resolver(
            RoleTestimonyCache::default(),
            InternalDnsIntentCache::default(),
            bind,
            receiver,
            health.clone(),
        );
        tokio::task::yield_now().await;

        for delay in [
            Duration::from_millis(250),
            Duration::from_millis(500),
            Duration::from_secs(1),
            Duration::from_secs(1),
        ] {
            tokio::time::advance(delay).await;
            tokio::task::yield_now().await;
        }

        assert_eq!(
            health.snapshot(),
            InternalResolverState::AwaitingBind { attempts: 5 }
        );
        let _ = shutdown.send(());
        task.await.expect("resolver task exits");
        drop(reservation);
    }

    #[tokio::test(start_paused = true)]
    async fn unchanged_bind_failure_diagnostics_repeat_every_thirty_seconds() {
        let mut diagnostics = BindFailureDiagnostics::new();
        let address_in_use = io::Error::new(io::ErrorKind::AddrInUse, "address in use");
        let address_unavailable =
            io::Error::new(io::ErrorKind::AddrNotAvailable, "address unavailable");

        assert!(diagnostics.should_report(&address_in_use, tokio::time::Instant::now()));
        assert!(!diagnostics.should_report(&address_in_use, tokio::time::Instant::now()));
        tokio::time::advance(Duration::from_secs(29)).await;
        assert!(!diagnostics.should_report(&address_in_use, tokio::time::Instant::now()));
        assert!(diagnostics.should_report(&address_unavailable, tokio::time::Instant::now()));
        assert!(!diagnostics.should_report(&address_unavailable, tokio::time::Instant::now()));
        tokio::time::advance(BIND_DIAGNOSTIC_REPEAT).await;
        assert!(diagnostics.should_report(&address_unavailable, tokio::time::Instant::now()));
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
        packet.extend_from_slice(&TEST_QCLASS_IN.to_be_bytes());
        packet
    }

    fn response_header(response: &[u8]) -> (u16, u16) {
        (
            test_read_u16(response, 2).expect("response flags"),
            test_read_u16(response, 6).expect("answer count"),
        )
    }

    fn test_read_u16(packet: &[u8], offset: usize) -> Option<u16> {
        let [high, low] = packet.get(offset..offset.checked_add(2)?)? else {
            return None;
        };
        Some(u16::from_be_bytes([*high, *low]))
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
            None,
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
            named_volume_names: Default::default(),
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
                wireguard_public_key: ployz_core::network::WireGuardPublicKey::try_new(format!(
                    "public-{machine}"
                ))
                .expect("public key"),
            }],
            dataplane_projection: ployz_core::network::DataplaneProjection::try_new(
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
