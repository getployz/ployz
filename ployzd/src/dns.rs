//! Observer-local Internal DNS serving and answer projection.

use std::{
    collections::{HashMap, HashSet},
    fs, io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures_util::StreamExt;
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
    ContainerObservation, Machine, MachineId, MembershipObservation, QualifiedService, ServiceId,
    service_containers, serving_containers, synthesize_membership,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
};
use tokio_stream::wrappers::IntervalStream;
use tokio_util::sync::CancellationToken;

use crate::corrosion::{
    AdminClient, Error as CorrosionError, ReplicatedStore, Subscription,
    membership_states_by_address,
};

mod query;

use query::{InternalQuery, MachineServiceTarget, Query, parse};

pub const PORT: u16 = 53;
const FORWARD_TIMEOUT: Duration = Duration::from_secs(3);
const MEMBERSHIP_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
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
                eprintln!("failed to load Internal DNS membership; {fallback}: {error}");
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
                .entry(observation.service_id)
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
        }
    }

    fn plan(&self, name: &Name, record_type: RecordType, local_subnet: Ipv4Net) -> ResponsePlan {
        match parse(name) {
            Query::Forward => ResponsePlan::Forward,
            Query::Internal(query) => self.plan_internal(name, record_type, local_subnet, query),
        }
    }

    fn plan_internal(
        &self,
        name: &Name,
        record_type: RecordType,
        local_subnet: Ipv4Net,
        query: InternalQuery,
    ) -> ResponsePlan {
        if record_type != RecordType::A {
            // TODO(UT-110): internal records remain A-only; other types return an authoritative
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
        // TODO(UT-111): keep TTL zero rather than adding a DNS cache without a product decision.
        let answers = addresses
            .into_iter()
            .map(|address| Record::from_rdata(name.clone(), 0, RData::A(A(address))))
            .collect();
        ResponsePlan::Internal {
            code: ResponseCode::NoError,
            answers,
        }
    }
}

struct Handler {
    projection: Arc<RwLock<Projection>>,
    local_subnet: Ipv4Net,
    upstreams: Vec<SocketAddr>,
}

#[async_trait]
impl RequestHandler for Handler {
    async fn handle_request<R: ResponseHandler, T: Time>(
        &self,
        request: &Request,
        mut response_handle: R,
    ) -> ResponseInfo {
        let Ok(info) = request.request_info() else {
            return send_error(request, response_handle, ResponseCode::FormErr).await;
        };
        let plan = self
            .projection
            .read()
            .map(|projection| {
                projection.plan(
                    info.query.original().name(),
                    info.query.query_type(),
                    self.local_subnet,
                )
            })
            .unwrap_or(ResponsePlan::Internal {
                code: ResponseCode::ServFail,
                answers: Vec::new(),
            });
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
                    eprintln!("failed to forward DNS query: {error}");
                    send_error(request, response_handle, ResponseCode::ServFail).await
                }
            },
        }
    }
}

impl Handler {
    async fn forward(&self, request: &Request) -> io::Result<Message> {
        let mut last_error = io::Error::other("no upstream DNS servers configured");
        for upstream in &self.upstreams {
            let result = tokio::time::timeout(FORWARD_TIMEOUT, async {
                match request.protocol() {
                    Protocol::Udp => forward_udp(request.as_slice(), *upstream).await,
                    Protocol::Tcp => forward_tcp(request.as_slice(), *upstream).await,
                    protocol => Err(io::Error::other(format!(
                        "unsupported forwarding transport {protocol:?}"
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

/// Serve Internal DNS from this Machine's observer-local Container and membership views.
///
/// # Errors
///
/// Returns an I/O error when replicated observations cannot be read or watched, the UDP listener
/// cannot bind, the DNS server fails, or the projection lock is poisoned.
pub async fn run(
    machine: Machine,
    replicated: ReplicatedStore,
    admin: AdminClient,
    upstreams: Option<Vec<SocketAddr>>,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let gateway = machine.subnet.gateway().0;
    let listen_address = SocketAddr::new(IpAddr::V4(gateway), PORT);
    let mut changes = replicated
        .subscribe_container_changes()
        .await
        .map_err(io::Error::other)?;
    let local_id = machine.id;
    let observations = replicated.containers().await.map_err(io::Error::other)?;
    let inputs = ProjectionInputs {
        local_id,
        observations: observations.observations,
        down_machines: None,
    };
    let projection = Arc::new(RwLock::new(inputs.build()));
    let handler = Handler {
        projection: Arc::clone(&projection),
        local_subnet: machine.subnet.into(),
        upstreams: configured_upstreams(upstreams, gateway),
    };
    let udp = UdpSocket::bind(listen_address).await?;
    let tcp = match TcpListener::bind(listen_address).await {
        Ok(listener) => Some(listener),
        Err(error) => {
            eprintln!("failed to bind best-effort DNS TCP listener on {listen_address}: {error}");
            None
        }
    };
    let mut server = Server::new(handler);
    server.register_socket(udp);
    if let Some(tcp) = tcp {
        server.register_listener(tcp, TCP_REQUEST_TIMEOUT, TCP_RESPONSE_BUFFER);
    }
    let server = run_server(server, shutdown.clone());
    let projection = watch_projection(
        replicated,
        admin,
        projection,
        inputs,
        &mut changes,
        shutdown,
    );
    tokio::try_join!(server, projection).map(|_| ())
}

async fn run_server(mut server: Server<Handler>, shutdown: CancellationToken) -> io::Result<()> {
    tokio::select! {
        result = server.block_until_done() => result.map_err(io::Error::other),
        () = shutdown.cancelled() => {
            server.shutdown_gracefully().await.map_err(io::Error::other)
        }
    }
}

async fn watch_projection(
    replicated: ReplicatedStore,
    admin: AdminClient,
    projection: Arc<RwLock<Projection>>,
    mut inputs: ProjectionInputs,
    changes: &mut Subscription,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let local_id = inputs.local_id;
    let mut interval =
        tokio::time::interval_at(tokio::time::Instant::now(), MEMBERSHIP_SAMPLE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // `Then` retains an in-flight membership read when another select branch wins, so slow
    // membership I/O never delays Container-change withdrawal.
    let membership =
        IntervalStream::new(interval).then(|_| load_down_machines(&replicated, &admin, &local_id));
    tokio::pin!(membership);
    loop {
        let rebuild = tokio::select! {
            changed = changes.changed() => {
                changed.map_err(io::Error::other)?;
                match replicated.containers().await {
                    Ok(next) => {
                        inputs.observations = next.observations;
                        true
                    }
                    Err(error) => {
                        eprintln!("failed to rebuild DNS projection: {error}");
                        false
                    }
                }
            }
            Some(result) = membership.next() => inputs.update_membership(result),
            () = shutdown.cancelled() => return Ok(()),
        };
        if rebuild {
            *projection
                .write()
                .map_err(|_| io::Error::other("DNS projection lock poisoned"))? = inputs.build();
        }
    }
}

async fn load_down_machines(
    replicated: &ReplicatedStore,
    admin: &AdminClient,
    local_id: &MachineId,
) -> Result<HashSet<MachineId>, CorrosionError> {
    let (machines, states) = tokio::time::timeout(MEMBERSHIP_SAMPLE_TIMEOUT, async {
        tokio::try_join!(replicated.machines(), admin.membership_states())
    })
    .await
    .map_err(|_| {
        CorrosionError::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            "Internal DNS membership sample timed out",
        ))
    })??;
    let states = membership_states_by_address(states);
    Ok(
        synthesize_membership(machines.observations, local_id, &states)
            .into_iter()
            .filter_map(|observation| {
                (observation.membership == MembershipObservation::Down)
                    .then_some(observation.machine.id)
            })
            .collect(),
    )
}

fn configured_upstreams(
    upstreams: Option<Vec<SocketAddr>>,
    listen_address: Ipv4Addr,
) -> Vec<SocketAddr> {
    upstreams.unwrap_or_else(|| system_upstreams(listen_address))
}

fn system_upstreams(listen_address: Ipv4Addr) -> Vec<SocketAddr> {
    match fs::read_to_string("/etc/resolv.conf") {
        Ok(text) => nameservers_from_resolv_conf(&text, listen_address),
        Err(error) => {
            eprintln!("failed to load DNS upstreams from /etc/resolv.conf: {error}");
            Vec::new()
        }
    }
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
