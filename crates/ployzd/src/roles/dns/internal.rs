//! Machine-local DNS resolver mechanics fed by a complete record snapshot.

use std::collections::BTreeMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hickory_proto::op::{Message, MessageType, OpCode, Query, ResponseCode};
use hickory_proto::rr::rdata::A;
use hickory_proto::rr::{DNSClass, Name, RData, Record, RecordType};
use ployz_core::network::internal_dns::InternalServiceName;
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, watch};
use tokio::task::{JoinHandle, JoinSet};

const DNS_PORT: u16 = 53;
const DNS_TTL_SECONDS: u32 = 5;
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(2);
const BIND_RETRY_INITIAL: Duration = Duration::from_millis(250);
const BIND_RETRY_CAP: Duration = Duration::from_secs(1);
const BIND_DIAGNOSTIC_REPEAT: Duration = Duration::from_secs(30);
const LOCAL_QUERY_TIMEOUT: Duration = Duration::from_millis(500);
const LOCAL_QUERY_ID: u16 = 0x504c;
const MAX_IN_FLIGHT_REQUESTS: usize = 32;

#[derive(Debug)]
struct DnsRequest {
    id: u16,
    recursion_desired: bool,
    query: Query,
}

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

#[must_use]
fn lookup_name(query: &Query) -> String {
    let name = query.name.to_ascii().to_ascii_lowercase();
    name.trim_end_matches('.').to_owned()
}

pub async fn query_bound_resolver(
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InternalResolverState {
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
    pub fn snapshot(&self) -> InternalResolverState {
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

    pub async fn await_bound(&self, timeout: Duration) -> Option<SocketAddr> {
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

#[derive(Debug, Clone, Default)]
pub struct InternalDnsRecords {
    records: Arc<Mutex<BTreeMap<InternalServiceName, Vec<Ipv4Addr>>>>,
}

impl InternalDnsRecords {
    pub fn replace(&self, records: BTreeMap<InternalServiceName, Vec<Ipv4Addr>>) {
        *self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = records;
    }

    fn addresses(&self, name: &InternalServiceName) -> Option<Vec<Ipv4Addr>> {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(name)
            .cloned()
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

#[must_use]
pub fn spawn_internal_resolver(
    records: InternalDnsRecords,
    bind: SocketAddr,
    shutdown: broadcast::Receiver<()>,
    health: InternalResolverHealth,
) -> JoinHandle<()> {
    let upstream = SocketAddr::new(load_upstream_nameserver(bind.ip()), DNS_PORT);
    spawn_internal_resolver_with_upstream(records, bind, upstream, shutdown, health)
}

fn spawn_internal_resolver_with_upstream(
    records: InternalDnsRecords,
    bind: SocketAddr,
    upstream: SocketAddr,
    mut shutdown: broadcast::Receiver<()>,
    health: InternalResolverHealth,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = BIND_RETRY_INITIAL;
        let mut diagnostics = BindFailureDiagnostics::new();
        let socket = loop {
            match UdpSocket::bind(bind).await {
                Ok(socket) => break socket,
                Err(error) => {
                    health.record_bind_failure();
                    if diagnostics.should_report(&error, tokio::time::Instant::now()) {
                        tracing::warn!(
                            phase = "bind",
                            address = %bind,
                            error = %error,
                            "internal DNS bind failed"
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
        let mut requests = JoinSet::new();
        let mut packet = [0_u8; 4096];
        loop {
            tokio::select! {
                biased;
                _ = shutdown.recv() => {
                    requests.abort_all();
                    while requests.join_next().await.is_some() {}
                    break;
                }
                Some(result) = requests.join_next(), if !requests.is_empty() => {
                    if let Err(error) = result
                        && !error.is_cancelled()
                    {
                        tracing::warn!(phase = "request", error = %error, "internal DNS request task failed");
                    }
                }
                received = socket.recv_from(&mut packet) => {
                    let (length, peer) = match received {
                        Ok(received) => received,
                        Err(error) => {
                            tracing::warn!(phase = "receive", error = %error, "internal DNS receive failed");
                            continue;
                        }
                    };
                    let Some(request) = packet.get(..length) else {
                        tracing::warn!(
                            phase = "receive",
                            invalid_length = length,
                            "internal DNS received a packet with an invalid length"
                        );
                        continue;
                    };
                    let request = request.to_vec();
                    if requests.len() >= MAX_IN_FLIGHT_REQUESTS {
                        if let Some(response) = overloaded_response(&request)
                            && let Err(error) = socket.send_to(&response, peer).await
                        {
                            tracing::warn!(
                                phase = "overload",
                                peer = %peer,
                                error = %error,
                                "internal DNS overload response failed"
                            );
                        }
                        continue;
                    }
                    let request_records = records.clone();
                    let response_socket = Arc::clone(&socket);
                    requests.spawn(async move {
                        let Some(response) =
                            response_for_request(&request_records, upstream, request).await
                        else {
                            return;
                        };
                        if let Err(error) = response_socket.send_to(&response, peer).await {
                            tracing::warn!(
                                phase = "respond",
                                peer = %peer,
                                error = %error,
                                "internal DNS response send failed"
                            );
                        }
                    });
                }
            }
        }
    })
}

fn overloaded_response(packet: &[u8]) -> Option<Vec<u8>> {
    let request = parse_request(packet)?;
    build_response(&request, ResponseCode::ServFail, &[])
}

async fn response_for_request(
    records: &InternalDnsRecords,
    upstream: SocketAddr,
    packet: Vec<u8>,
) -> Option<Vec<u8>> {
    let request = parse_request(&packet)?;
    let name = lookup_name(&request.query);
    let internal_name = InternalServiceName::try_new(&name).ok();
    if let Some(addresses) = internal_name
        .as_ref()
        .and_then(|name| records.addresses(name))
    {
        let answers = if request.query.query_type == RecordType::A
            && request.query.query_class == DNSClass::IN
        {
            addresses
        } else {
            Vec::new()
        };
        return build_response(&request, ResponseCode::NoError, &answers);
    }

    match forward_to_upstream(upstream, &packet).await {
        Ok(response) => Some(response),
        Err(error) => {
            tracing::warn!(
                phase = "forward",
                upstream = %upstream,
                error = %error,
                "internal DNS upstream forward failed"
            );
            build_response(&request, ResponseCode::ServFail, &[])
        }
    }
}

async fn forward_to_upstream(upstream: SocketAddr, packet: &[u8]) -> Result<Vec<u8>, io::Error> {
    let Some(request) = parse_request(packet) else {
        return Err(invalid_dns_response("DNS request was malformed"));
    };
    let local = match upstream.ip() {
        IpAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
        IpAddr::V6(_) => SocketAddr::from(([0_u16; 8], 0)),
    };
    let socket = UdpSocket::bind(local).await?;
    socket.connect(upstream).await?;
    socket.send(packet).await?;
    tokio::time::timeout(UPSTREAM_TIMEOUT, async {
        let mut response = vec![0_u8; 4096];
        loop {
            let length = socket.recv(&mut response).await?;
            let Some(candidate) = response.get(..length) else {
                continue;
            };
            let Ok(message) = Message::from_vec(candidate) else {
                continue;
            };
            if message.metadata.id != request.id
                || message.metadata.message_type != MessageType::Response
                || message.queries.first() != Some(&request.query)
            {
                continue;
            }
            return Ok(candidate.to_vec());
        }
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "DNS upstream timed out"))?
}

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
    std::fs::read_to_string("/etc/resolv.conf")
        .ok()
        .and_then(|contents| upstream_from_resolv_conf(&contents, own_bind))
        .unwrap_or_else(|| {
            let fallback = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));
            tracing::warn!(
                phase = "upstream",
                upstream = %fallback,
                "no usable resolv.conf nameserver; using fallback upstream"
            );
            fallback
        })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    async fn known_internal_name_without_endpoints_returns_noerror_without_answers() {
        let records = InternalDnsRecords::default();
        records.replace(BTreeMap::from([(
            InternalServiceName::try_new("missing.default.internal").expect("internal name"),
            Vec::new(),
        )]));
        let response = response_for_request(
            &records,
            SocketAddr::from((Ipv4Addr::new(1, 1, 1, 1), DNS_PORT)),
            query_packet("missing.default.internal", TEST_QTYPE_A),
        )
        .await
        .expect("internal query has a response");

        assert_eq!(response_header(&response), (0x8500, 0));
    }

    #[tokio::test]
    async fn cloud_internal_name_is_forwarded_when_it_is_not_a_projected_service() {
        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind upstream");
        let upstream_addr = upstream.local_addr().expect("upstream address");
        let upstream_task = tokio::spawn(async move {
            let mut packet = [0_u8; 512];
            let (length, peer) = upstream.recv_from(&mut packet).await.expect("query");
            let request =
                Message::from_vec(packet.get(..length).expect("length")).expect("DNS query");
            let Some(query) = request.queries.first() else {
                panic!("DNS query has a question");
            };
            let mut response = Message::response(request.metadata.id, OpCode::Query);
            response.metadata.response_code = ResponseCode::NoError;
            response.add_query(query.clone());
            upstream
                .send_to(&response.to_vec().expect("response"), peer)
                .await
                .expect("send");
        });

        let response = response_for_request(
            &InternalDnsRecords::default(),
            upstream_addr,
            query_packet("metadata.compute.internal", TEST_QTYPE_A),
        )
        .await
        .expect("forwarded response");

        assert_eq!(response_header(&response), (0x8000, 0));
        upstream_task.await.expect("upstream task");
    }

    #[tokio::test]
    async fn upstream_mismatches_are_ignored_until_a_matching_response_arrives() {
        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind upstream");
        let upstream_addr = upstream.local_addr().expect("upstream address");
        let upstream_task = tokio::spawn(async move {
            let mut packet = [0_u8; 512];
            let (length, peer) = upstream.recv_from(&mut packet).await.expect("query");
            let request =
                Message::from_vec(packet.get(..length).expect("length")).expect("DNS query");
            let Some(query) = request.queries.first() else {
                panic!("DNS query has a question");
            };

            let mut wrong_id =
                Message::response(request.metadata.id.wrapping_add(1), OpCode::Query);
            wrong_id.add_query(query.clone());
            upstream
                .send_to(&wrong_id.to_vec().expect("wrong id"), peer)
                .await
                .expect("send");

            let mut query_not_response = request.clone();
            query_not_response.metadata.message_type = MessageType::Query;
            upstream
                .send_to(&query_not_response.to_vec().expect("query response"), peer)
                .await
                .expect("send");

            let mut wrong_question = Message::response(request.metadata.id, OpCode::Query);
            wrong_question.add_query(Query::query(
                Name::from_ascii("other.example.").expect("name"),
                RecordType::A,
            ));
            upstream
                .send_to(&wrong_question.to_vec().expect("wrong question"), peer)
                .await
                .expect("send");

            let mut valid = Message::response(request.metadata.id, OpCode::Query);
            valid.add_query(query.clone());
            upstream
                .send_to(&valid.to_vec().expect("valid"), peer)
                .await
                .expect("send");
        });

        let request = query_packet("example.com", TEST_QTYPE_A);
        let response = forward_to_upstream(upstream_addr, &request)
            .await
            .expect("matching response");

        let parsed = Message::from_vec(&response).expect("response");
        assert_eq!(parsed.metadata.id, 0x1234);
        assert_eq!(parsed.metadata.message_type, MessageType::Response);
        upstream_task.await.expect("upstream task");
    }

    #[tokio::test]
    async fn resolver_answers_record_snapshot_over_udp() {
        let reservation = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("reserve UDP port");
        let bind = reservation.local_addr().expect("reserved address");
        drop(reservation);

        let records = InternalDnsRecords::default();
        records.replace(BTreeMap::from([(
            InternalServiceName::try_new("db.default.internal").expect("internal name"),
            vec![Ipv4Addr::new(10, 42, 2, 8)],
        )]));
        let (shutdown, receiver) = broadcast::channel(1);
        let health = InternalResolverHealth::awaiting_bind();
        let task = spawn_internal_resolver(records, bind, receiver, health.clone());
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
                received_response = packet.get(..length).map(<[u8]>::to_vec);
                break;
            }
        }
        let response = received_response.expect("resolver answers before deadline");

        assert_eq!(response_header(&response), (0x8500, 1));
        assert_eq!(
            response.get(response.len().saturating_sub(4)..),
            Some([10, 42, 2, 8].as_slice())
        );
        assert_eq!(
            health.snapshot(),
            InternalResolverState::Serving { bound: bind }
        );
        let _ = shutdown.send(());
        task.await.expect("resolver task exits");
    }

    #[tokio::test]
    async fn resolver_sheds_requests_above_its_concurrency_budget() {
        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind blackhole upstream");
        let reservation = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("reserve resolver port");
        let bind = reservation.local_addr().expect("resolver address");
        drop(reservation);
        let (shutdown, receiver) = broadcast::channel(1);
        let health = InternalResolverHealth::awaiting_bind();
        let task = spawn_internal_resolver_with_upstream(
            InternalDnsRecords::default(),
            bind,
            upstream.local_addr().expect("upstream address"),
            receiver,
            health.clone(),
        );
        health
            .await_bound(Duration::from_secs(1))
            .await
            .expect("resolver binds");
        let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind test client");
        let request = query_packet("example.com", TEST_QTYPE_A);

        for _ in 0..=MAX_IN_FLIGHT_REQUESTS {
            client
                .send_to(&request, bind)
                .await
                .expect("send external query");
        }

        let mut forwarded = [0_u8; 512];
        for _ in 0..MAX_IN_FLIGHT_REQUESTS {
            tokio::time::timeout(Duration::from_secs(1), upstream.recv_from(&mut forwarded))
                .await
                .expect("forward arrives before deadline")
                .expect("receive forwarded query");
        }
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                upstream.recv_from(&mut forwarded),
            )
            .await
            .is_err(),
            "request above the budget must not allocate an upstream socket"
        );
        let mut overloaded = [0_u8; 512];
        let (length, _) =
            tokio::time::timeout(Duration::from_secs(1), client.recv_from(&mut overloaded))
                .await
                .expect("overload response arrives")
                .expect("receive overload response");
        assert_eq!(
            response_header(overloaded.get(..length).expect("response length")).0 & 0x000f,
            2
        );

        let _ = shutdown.send(());
        task.await.expect("resolver task exits");
    }

    #[tokio::test]
    async fn resolver_shutdown_cancels_and_joins_forwarding_children() {
        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind blackhole upstream");
        let reservation = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("reserve resolver port");
        let bind = reservation.local_addr().expect("resolver address");
        drop(reservation);
        let (shutdown, receiver) = broadcast::channel(1);
        let health = InternalResolverHealth::awaiting_bind();
        let task = spawn_internal_resolver_with_upstream(
            InternalDnsRecords::default(),
            bind,
            upstream.local_addr().expect("upstream address"),
            receiver,
            health.clone(),
        );
        health
            .await_bound(Duration::from_secs(1))
            .await
            .expect("resolver binds");
        let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind test client");
        client
            .send_to(&query_packet("example.com", TEST_QTYPE_A), bind)
            .await
            .expect("send external query");
        let mut forwarded = [0_u8; 512];
        let (_, child_upstream_peer) =
            tokio::time::timeout(Duration::from_secs(1), upstream.recv_from(&mut forwarded))
                .await
                .expect("forward arrives before deadline")
                .expect("receive forwarded query");

        let _ = shutdown.send(());
        tokio::time::timeout(Duration::from_millis(500), task)
            .await
            .expect("resolver joins children during shutdown")
            .expect("resolver task exits");
        upstream
            .send_to(b"late upstream response", child_upstream_peer)
            .await
            .expect("send late response");
        let mut response = [0_u8; 512];
        assert!(
            tokio::time::timeout(Duration::from_millis(100), client.recv_from(&mut response))
                .await
                .is_err(),
            "cancelled child must not forward a late upstream response"
        );
    }

    #[tokio::test]
    async fn aaaa_query_for_known_name_has_no_answers() {
        let records = InternalDnsRecords::default();
        records.replace(BTreeMap::from([(
            InternalServiceName::try_new("db.default.internal").expect("internal name"),
            vec![Ipv4Addr::new(10, 42, 2, 8)],
        )]));

        let response = response_for_request(
            &records,
            SocketAddr::from((Ipv4Addr::new(1, 1, 1, 1), DNS_PORT)),
            query_packet("db.default.internal", TEST_QTYPE_AAAA),
        )
        .await
        .expect("internal query has a response");

        assert_eq!(response_header(&response), (0x8500, 0));
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
            read_u16(response, 2).expect("response flags"),
            read_u16(response, 6).expect("answer count"),
        )
    }

    fn read_u16(packet: &[u8], offset: usize) -> Option<u16> {
        let [high, low] = packet.get(offset..offset.checked_add(2)?)? else {
            return None;
        };
        Some(u16::from_be_bytes([*high, *low]))
    }
}
