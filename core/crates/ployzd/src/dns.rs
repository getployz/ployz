//! Observer-local Internal DNS serving and answer projection.

use std::{
    collections::{HashMap, HashSet},
    fs, io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use hickory_server::proto::{
    op::{Edns, Header, HeaderCounts, Message, Metadata, ResponseCode},
    rr::{Name, RData, Record, RecordType, rdata::A},
    serialize::binary::BinEncoder,
};
use hickory_server::{
    Server,
    net::{runtime::Time, xfer::Protocol},
    server::{Request, RequestHandler, ResponseHandler, ResponseInfo},
    zone_handler::MessageResponseBuilder,
};
use ipnet::Ipv4Net;
use ployz_core::{
    ContainerObservation, MachineId, MembershipObservation, Namespace, QualifiedService, ServiceId,
    service_containers, serving_containers, synthesize_membership,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    sync::watch,
};
use tokio_util::sync::CancellationToken;

use crate::corrosion::{
    AdminClient, Error as CorrosionError, MachineView, membership_states_by_address,
};

mod listeners;
mod process;
mod query;
mod service;
mod spec;

pub use process::{ABDICATED, DnsExit, serve};
use query::{InternalQuery, MachineServiceTarget, Query, parse};
pub use service::{DnsService, DnsSupervision};

pub const PORT: u16 = 53;
const FORWARD_TIMEOUT: Duration = Duration::from_secs(3);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const MEMBERSHIP_SAMPLE_TIMEOUT: Duration = Duration::from_secs(1);
const TCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const TCP_RESPONSE_BUFFER: usize = 32;

#[derive(Debug, PartialEq)]
enum ResponsePlan {
    Forward,
    Internal {
        code: ResponseCode,
        answers: Vec<Record>,
    },
}

struct Projection {
    service_ids: HashMap<ServiceId, Vec<Ipv4Addr>>,
    identities: HashMap<QualifiedService, ServiceAddresses>,
    machine_identities: HashMap<MachineServiceTarget, Vec<Ipv4Addr>>,
    caller_namespaces: HashMap<Ipv4Addr, Namespace>,
}

struct ProjectionInputs {
    local_id: MachineId,
    observations: Vec<ContainerObservation>,
    down_machines: Option<HashSet<MachineId>>,
}

impl ProjectionInputs {
    fn build(&self) -> Projection {
        Projection::from_observations(
            &self.observations,
            &self.local_id,
            self.down_machines.as_ref(),
        )
    }

    fn update_membership(&mut self, loaded: Result<HashSet<MachineId>, CorrosionError>) -> bool {
        match loaded {
            Ok(down) if self.down_machines.as_ref() == Some(&down) => false,
            Ok(down) => {
                self.down_machines = Some(down);
                true
            }
            Err(error) => {
                let fallback = if self.down_machines.is_some() {
                    "keeping the last successful filter"
                } else {
                    "serving unfiltered answers"
                };
                eprintln!(
                    "failed to load Internal DNS membership; {fallback}: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
                false
            }
        }
    }
}

#[derive(Default)]
struct ServiceAddresses {
    eligible: Vec<Ipv4Addr>,
    next: AtomicUsize,
}

impl ServiceAddresses {
    fn rotated(&self) -> Vec<Ipv4Addr> {
        let mut addresses = self.eligible.clone();
        if !addresses.is_empty() {
            let offset = self.next.fetch_add(1, Ordering::Relaxed) % addresses.len();
            addresses.rotate_left(offset);
        }
        addresses
    }
}

impl Projection {
    fn from_observations(
        observations: &[ContainerObservation],
        local_id: &MachineId,
        down_machines: Option<&HashSet<MachineId>>,
    ) -> Self {
        let containers = service_containers(observations.iter().cloned());
        let mut service_ids = HashMap::<ServiceId, Vec<Ipv4Addr>>::new();
        let mut identities = HashMap::<QualifiedService, ServiceAddresses>::new();
        let mut machine_identities = HashMap::<MachineServiceTarget, Vec<Ipv4Addr>>::new();
        for serving in serving_containers(&containers) {
            let observation = serving.as_observation();
            if observation.machine_id != *local_id
                && down_machines.is_some_and(|down| down.contains(&observation.machine_id))
            {
                continue;
            }
            let address = serving.address();
            let identity = observation.identity();
            service_ids
                .entry(observation.service_id())
                .or_default()
                .push(address.0);
            identities
                .entry(identity.clone())
                .or_default()
                .eligible
                .push(address.0);
            machine_identities
                .entry(MachineServiceTarget {
                    machine_id: observation.machine_id,
                    identity,
                })
                .or_default()
                .push(address.0);
        }
        Self {
            service_ids,
            identities,
            machine_identities,
            caller_namespaces: unique_caller_namespaces(observations),
        }
    }

    fn plan(
        &self,
        name: &Name,
        record_type: RecordType,
        local_subnet: Ipv4Net,
        source: IpAddr,
    ) -> ResponsePlan {
        match parse(name) {
            Query::Forward => ResponsePlan::Forward,
            Query::Internal(query) => {
                self.plan_internal(name, record_type, local_subnet, query, source)
            }
        }
    }

    fn plan_internal(
        &self,
        name: &Name,
        record_type: RecordType,
        local_subnet: Ipv4Net,
        query: InternalQuery,
        source: IpAddr,
    ) -> ResponsePlan {
        if record_type != RecordType::A {
            // TODO: internal records remain A-only; other types return an authoritative
            // empty NOERROR response until a product decision adds them.
            return ResponsePlan::Internal {
                code: ResponseCode::NoError,
                answers: Vec::new(),
            };
        }
        let (mut addresses, nearest) = match query {
            InternalQuery::Empty | InternalQuery::Regional => (Vec::new(), false),
            InternalQuery::Service(identity) => (
                self.identities
                    .get(&identity)
                    .map(ServiceAddresses::rotated)
                    .unwrap_or_default(),
                false,
            ),
            InternalQuery::CallerService(service) => (
                self.caller_namespace(source)
                    .and_then(|namespace| {
                        self.identities
                            .get(&QualifiedService::new(namespace.clone(), service))
                    })
                    .map(ServiceAddresses::rotated)
                    .unwrap_or_default(),
                false,
            ),
            InternalQuery::Nearest(identity) => (
                self.identities
                    .get(&identity)
                    .map(|addresses| addresses.eligible.clone())
                    .unwrap_or_default(),
                true,
            ),
            InternalQuery::ServiceId(id) => (
                self.service_ids.get(&id).cloned().unwrap_or_default(),
                false,
            ),
            InternalQuery::Machine(target) => (
                self.machine_identities
                    .get(&target)
                    .cloned()
                    .unwrap_or_default(),
                false,
            ),
        };
        if nearest {
            addresses.sort_by_key(|address| !local_subnet.contains(address));
        }
        if addresses.is_empty() {
            return ResponsePlan::Internal {
                code: ResponseCode::NXDomain,
                answers: Vec::new(),
            };
        }
        // TODO: keep TTL zero rather than adding a DNS cache without a product decision.
        let answers = addresses
            .into_iter()
            .map(|address| Record::from_rdata(name.clone(), 0, RData::A(A(address))))
            .collect();
        ResponsePlan::Internal {
            code: ResponseCode::NoError,
            answers,
        }
    }

    fn caller_namespace(&self, source: IpAddr) -> Option<&Namespace> {
        let IpAddr::V4(address) = source else {
            return None;
        };
        self.caller_namespaces.get(&address)
    }
}

fn unique_caller_namespaces(containers: &[ContainerObservation]) -> HashMap<Ipv4Addr, Namespace> {
    let mut by_address = HashMap::<Ipv4Addr, Vec<Namespace>>::new();
    for container in containers {
        let Some(address) = container.address else {
            continue;
        };
        by_address
            .entry(address.0)
            .or_default()
            .push(container.namespace.clone());
    }
    by_address
        .into_iter()
        .filter_map(|(address, namespaces)| {
            <[Namespace; 1]>::try_from(namespaces)
                .ok()
                .map(|[namespace]| (address, namespace))
        })
        .collect()
}

#[derive(Clone)]
pub(crate) struct Answers(Arc<Shared>);

struct Shared {
    projection: RwLock<Projection>,
    local_subnet: Ipv4Net,
    upstreams: Upstreams,
}

impl Answers {
    fn loaded(projection: Projection, local_subnet: Ipv4Net, upstreams: Upstreams) -> Self {
        Self(Arc::new(Shared {
            projection: RwLock::new(projection),
            local_subnet,
            upstreams,
        }))
    }

    fn replace(&self, projection: Projection) -> io::Result<()> {
        *self
            .0
            .projection
            .write()
            .map_err(|_| io::Error::other("DNS projection lock poisoned"))? = projection;
        Ok(())
    }

    fn upstreams(&self) -> &Upstreams {
        &self.0.upstreams
    }

    fn plan(&self, name: &Name, query_type: RecordType, caller: IpAddr) -> ResponsePlan {
        self.0
            .projection
            .read()
            .map(|projection| projection.plan(name, query_type, self.0.local_subnet, caller))
            .unwrap_or(ResponsePlan::Internal {
                code: ResponseCode::ServFail,
                answers: Vec::new(),
            })
    }
}

#[derive(Clone)]
pub(crate) struct Upstreams(Arc<UpstreamSource>);

enum UpstreamSource {
    Fixed(Vec<SocketAddr>),
    ResolvConf {
        path: PathBuf,
        skip: Ipv4Addr,
        state: Mutex<ResolvConf>,
    },
}

#[derive(Default)]
struct ResolvConf {
    text: Option<String>,
    nameservers: Vec<SocketAddr>,
}

impl Upstreams {
    fn of(fixed: &[SocketAddr], resolv_conf: &Path, listen: Ipv4Addr) -> Self {
        if fixed.is_empty() {
            Self::follow(resolv_conf, listen)
        } else {
            Self::fixed(fixed.to_vec())
        }
    }

    fn fixed(addresses: Vec<SocketAddr>) -> Self {
        Self(Arc::new(UpstreamSource::Fixed(addresses)))
    }

    fn follow(path: &Path, listen: Ipv4Addr) -> Self {
        let upstreams = Self(Arc::new(UpstreamSource::ResolvConf {
            path: path.to_path_buf(),
            skip: listen,
            state: Mutex::new(ResolvConf::default()),
        }));
        upstreams.reload_if_changed();
        upstreams
    }

    fn snapshot(&self) -> Vec<SocketAddr> {
        match &*self.0 {
            UpstreamSource::Fixed(addresses) => addresses.clone(),
            UpstreamSource::ResolvConf { state, .. } => state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .nameservers
                .clone(),
        }
    }

    fn reload_if_changed(&self) -> bool {
        let UpstreamSource::ResolvConf { path, skip, state } = &*self.0 else {
            return false;
        };
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let text = match fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(error) => {
                if state.text.is_some() {
                    eprintln!(
                        "failed to load DNS upstreams from {}: {error}",
                        path.display(),
                        error = ployz_core::error_chain::inline(&error),
                    );
                }
                None
            }
        };
        if state.text == text {
            return false;
        }
        state.nameservers = text
            .as_deref()
            .map(|text| nameservers_from_resolv_conf(text, *skip))
            .unwrap_or_default();
        state.text = text;
        true
    }
}

#[derive(Clone)]
struct InFlight(watch::Sender<usize>);

impl InFlight {
    fn new() -> Self {
        Self(watch::Sender::new(0))
    }

    fn enter(&self) -> InFlightGuard {
        self.0.send_modify(|count| *count += 1);
        InFlightGuard(self.0.clone())
    }

    async fn drained(&self) {
        let _ = self.0.subscribe().wait_for(|count| *count == 0).await;
    }
}

struct InFlightGuard(watch::Sender<usize>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.send_modify(|count| *count -= 1);
    }
}

pub(crate) struct Handler {
    answers: Answers,
    in_flight: InFlight,
}

impl Handler {
    fn new(answers: Answers) -> Self {
        Self {
            answers,
            in_flight: InFlight::new(),
        }
    }

    fn in_flight(&self) -> InFlight {
        self.in_flight.clone()
    }
}

#[async_trait]
impl RequestHandler for Handler {
    async fn handle_request<R: ResponseHandler, T: Time>(
        &self,
        request: &Request,
        mut response_handle: R,
    ) -> ResponseInfo {
        let _guard = self.in_flight.enter();
        let Ok(info) = request.request_info() else {
            return send_error(request, response_handle, ResponseCode::FormErr).await;
        };
        let plan = self.answers.plan(
            info.query.original().name(),
            info.query.query_type(),
            info.src.ip(),
        );
        match plan {
            ResponsePlan::Internal { code, answers } => {
                let mut metadata = Metadata::response_from_request(&request.metadata);
                metadata.authoritative = true;
                metadata.recursion_available = true;
                metadata.response_code = code;
                let edns = request.edns.as_ref();
                let (answers, truncated) = fit_udp_answers(request, &metadata, edns, &answers);
                metadata.truncation = truncated;
                let response = MessageResponseBuilder::new(&request.queries, edns).build(
                    metadata,
                    answers.iter(),
                    [].iter(),
                    [].iter(),
                    [].iter(),
                );
                response_handle
                    .send_response(response)
                    .await
                    .unwrap_or_else(|_| failed_response(request, ResponseCode::ServFail))
            }
            ResponsePlan::Forward => match self.forward(request).await {
                Ok(message) => {
                    let mut builder = MessageResponseBuilder::from_message_request(request);
                    if let Some(edns) = message.edns.as_ref() {
                        builder.edns(edns);
                    }
                    let response = builder.build(
                        message.metadata,
                        message.answers.iter(),
                        message.authorities.iter(),
                        [].iter(),
                        message.additionals.iter(),
                    );
                    response_handle
                        .send_response(response)
                        .await
                        .unwrap_or_else(|_| failed_response(request, ResponseCode::ServFail))
                }
                Err(error) => {
                    eprintln!(
                        "failed to forward DNS query: {error}",
                        error = ployz_core::error_chain::inline(&error),
                    );
                    send_error(request, response_handle, ResponseCode::ServFail).await
                }
            },
        }
    }
}

impl Handler {
    async fn forward(&self, request: &Request) -> io::Result<Message> {
        let mut last_error = io::Error::other("no upstream DNS servers configured");
        for upstream in self.answers.upstreams().snapshot() {
            let result = tokio::time::timeout(FORWARD_TIMEOUT, async {
                match request.protocol() {
                    Protocol::Udp => forward_udp(request.as_slice(), upstream).await,
                    Protocol::Tcp => forward_tcp(request.as_slice(), upstream).await,
                    protocol => Err(io::Error::other(format!(
                        "unsupported forwarding transport {protocol}"
                    ))),
                }
            })
            .await;
            match result {
                Ok(Ok(message)) => return Ok(message),
                Ok(Err(error)) => last_error = error,
                Err(_) => {
                    last_error = io::Error::new(io::ErrorKind::TimedOut, "DNS upstream timed out")
                }
            }
        }
        Err(last_error)
    }
}

/// Hickory aborts request tasks when its loops stop, so the loops keep
/// accepting during the bounded drain; arrivals in that window are answered too.
async fn run_server(
    mut server: Server<Handler>,
    in_flight: InFlight,
    listen: SocketAddr,
    shutdown: CancellationToken,
) -> io::Result<()> {
    tokio::select! {
        result = server.block_until_done() => result.map_err(io::Error::other),
        () = shutdown.cancelled() => {
            let drained = async {
                in_flight.drained().await;
                flush_udp(listen).await
            };
            match tokio::time::timeout(DRAIN_TIMEOUT, drained).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => eprintln!("failed to flush Internal DNS UDP responses: {error}"),
                Err(_) => eprintln!("stopping Internal DNS with responses still undelivered after {DRAIN_TIMEOUT:?}"),
            }
            server.shutdown_gracefully().await.map_err(io::Error::other)
        }
    }
}

/// A finished handler has only queued its UDP response; Hickory's receive loop
/// sends it later, and shutdown can stop that loop first. The loop sends its
/// queue in order before reading the next datagram, and Hickory answers a
/// header-only query itself, so that answer arriving proves the queue sent.
async fn flush_udp(listen: SocketAddr) -> io::Result<()> {
    const HEADER_ONLY_QUERY: [u8; 12] = [0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    let socket = UdpSocket::bind(SocketAddr::new(listen.ip(), 0)).await?;
    socket.connect(listen).await?;
    socket.send(&HEADER_ONLY_QUERY).await?;
    socket.recv(&mut [0; 512]).await?;
    Ok(())
}

async fn load_down_machines(
    machines: &MachineView,
    admin: &AdminClient,
    local_id: &MachineId,
) -> Result<HashSet<MachineId>, CorrosionError> {
    let machines = machines.current().ok_or_else(|| {
        CorrosionError::Io(io::Error::new(
            io::ErrorKind::NotConnected,
            "the Machine view has not observed the store yet",
        ))
    })?;
    let states = tokio::time::timeout(MEMBERSHIP_SAMPLE_TIMEOUT, admin.membership_states())
        .await
        .map_err(|_| {
            CorrosionError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "Internal DNS membership sample timed out",
            ))
        })??;
    let states = membership_states_by_address(states);
    Ok(
        synthesize_membership(machines.observations.clone(), local_id, &states)
            .into_iter()
            .filter_map(|observation| {
                (observation.membership == MembershipObservation::Down)
                    .then_some(observation.machine.id)
            })
            .collect(),
    )
}

fn nameservers_from_resolv_conf(text: &str, listen: Ipv4Addr) -> Vec<SocketAddr> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            if words.next() != Some("nameserver") {
                return None;
            }
            let ip: IpAddr = words.next()?.parse().ok()?;
            (ip != IpAddr::V4(listen)).then_some(SocketAddr::new(ip, PORT))
        })
        .collect()
}

async fn forward_udp(request: &[u8], upstream: SocketAddr) -> io::Result<Message> {
    let bind = if upstream.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind).await?;
    socket.connect(upstream).await?;
    socket.send(request).await?;
    let mut response = vec![0; u16::MAX as usize];
    let length = socket.recv(&mut response).await?;
    response.truncate(length);
    decode_message(&response)
}

async fn forward_tcp(request: &[u8], upstream: SocketAddr) -> io::Result<Message> {
    let length = u16::try_from(request.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "DNS request exceeds TCP framing",
        )
    })?;
    let mut stream = TcpStream::connect(upstream).await?;
    stream.write_u16(length).await?;
    stream.write_all(request).await?;
    let length = stream.read_u16().await?;
    let mut response = vec![0; usize::from(length)];
    stream.read_exact(&mut response).await?;
    decode_message(&response)
}

fn decode_message(bytes: &[u8]) -> io::Result<Message> {
    Message::from_vec(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

// Hickory encodes UDP with no response EDNS at 4096 bytes, so TC never fires
// for a classic 512-byte client. Probe at request.max_payload() first.
fn fit_udp_answers<'a>(
    request: &Request,
    metadata: &Metadata,
    edns: Option<&Edns>,
    answers: &'a [Record],
) -> (&'a [Record], bool) {
    if request.protocol() != Protocol::Udp {
        return (answers, false);
    }
    let mut bytes = Vec::new();
    let mut encoder = BinEncoder::new(&mut bytes);
    encoder.set_max_size(request.max_payload());
    let Ok(info) = MessageResponseBuilder::new(&request.queries, edns)
        .build(*metadata, answers.iter(), [].iter(), [].iter(), [].iter())
        .destructive_emit(&mut encoder)
    else {
        return (&[], true);
    };
    let n = usize::from(info.counts().answers).min(answers.len());
    (answers.get(..n).unwrap_or(&[]), info.truncation)
}

async fn send_error<R: ResponseHandler>(
    request: &Request,
    mut response_handle: R,
    code: ResponseCode,
) -> ResponseInfo {
    let response =
        MessageResponseBuilder::from_message_request(request).error_msg(&request.metadata, code);
    response_handle
        .send_response(response)
        .await
        .unwrap_or_else(|_| failed_response(request, code))
}

fn failed_response(request: &Request, code: ResponseCode) -> ResponseInfo {
    let mut metadata = Metadata::response_from_request(&request.metadata);
    metadata.response_code = code;
    Header {
        metadata,
        counts: HeaderCounts::default(),
    }
    .into()
}

#[cfg(test)]
mod tests;
