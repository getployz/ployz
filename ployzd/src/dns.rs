use std::{
    collections::{HashMap, hash_map::Entry},
    fs, io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, RwLock},
    time::Duration,
};

use async_trait::async_trait;
use hickory_server::proto::{
    op::{Header, HeaderCounts, Message, Metadata, ResponseCode},
    rr::{Name, RData, Record, RecordType, rdata::A},
};
use hickory_server::{
    Server,
    net::{runtime::Time, xfer::Protocol},
    server::{Request, RequestHandler, ResponseHandler, ResponseInfo},
    zone_handler::MessageResponseBuilder,
};
use ipnet::Ipv4Net;
use ployz_core::{
    ContainerObservation, Machine, ProjectName, QualifiedService, ServiceId, service_containers,
    serving_replicas,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
};
use tokio_util::sync::CancellationToken;

use crate::corrosion::{ReplicatedStore, Subscription};

mod query;

use query::{InternalQuery, MachineServiceTarget, Query, Target, parse};

pub const PORT: u16 = 53;
const FORWARD_TIMEOUT: Duration = Duration::from_secs(3);
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

/// Observer-relative Caller Project from a source Container Address.
enum CallerProject {
    Unique(ProjectName),
    Ambiguous,
}

struct Projection {
    service_ids: HashMap<ServiceId, Vec<Ipv4Addr>>,
    identities: HashMap<QualifiedService, Vec<Ipv4Addr>>,
    machine_identities: HashMap<MachineServiceTarget, Vec<Ipv4Addr>>,
    callers: HashMap<Ipv4Addr, CallerProject>,
}

impl Projection {
    fn from_observations(observations: &[ContainerObservation]) -> Self {
        // TODO(UT-117, UT-118): keep Membership Observations out of DNS projection until a
        // product decision replaces the baseline's deliberately membership-blind behavior.
        let containers = service_containers(observations.iter().cloned());
        let mut service_ids = HashMap::<ServiceId, Vec<Ipv4Addr>>::new();
        let mut identities = HashMap::<QualifiedService, Vec<Ipv4Addr>>::new();
        let mut machine_identities = HashMap::<MachineServiceTarget, Vec<Ipv4Addr>>::new();
        let mut callers = HashMap::<Ipv4Addr, CallerProject>::new();
        // Caller matching uses every addressed Service Container, not Serving Containers.
        for container in &containers {
            let observation = container.as_observation();
            let Some(address) = observation.address else {
                continue;
            };
            match callers.entry(address.0) {
                Entry::Vacant(entry) => {
                    entry.insert(CallerProject::Unique(observation.project_name.clone()));
                }
                Entry::Occupied(mut entry) => {
                    *entry.get_mut() = CallerProject::Ambiguous;
                }
            }
        }
        for container in serving_replicas(&containers) {
            let observation = container.as_observation();
            let address = observation
                .address
                .expect("Serving Container has a Container Address");
            let identity = observation.identity();
            service_ids
                .entry(observation.service_id)
                .or_default()
                .push(address.0);
            identities
                .entry(identity.clone())
                .or_default()
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
            callers,
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
                self.plan_internal(name, record_type, local_subnet, source, query)
            }
        }
    }

    fn plan_internal(
        &self,
        name: &Name,
        record_type: RecordType,
        local_subnet: Ipv4Net,
        source: IpAddr,
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
        let target = match query.target {
            Target::ServiceName(service_name) => self
                .caller_project(source)
                .map(|project| {
                    Target::Identity(QualifiedService::new(project.clone(), service_name))
                })
                .unwrap_or(Target::Empty),
            Target::MachineServiceName(target) => self
                .caller_project(source)
                .map(|project| {
                    Target::MachineIdentity(MachineServiceTarget {
                        machine_id: target.machine_id,
                        identity: QualifiedService::new(project.clone(), target.service_name),
                    })
                })
                .unwrap_or(Target::Empty),
            target @ (Target::Empty
            | Target::ServiceId(_)
            | Target::Identity(_)
            | Target::MachineIdentity(_)) => target,
        };
        let mut addresses = self.addresses(&target);
        if query.nearest {
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

    fn addresses(&self, target: &Target) -> Vec<Ipv4Addr> {
        match target {
            Target::Empty | Target::ServiceName(_) | Target::MachineServiceName(_) => Vec::new(),
            Target::ServiceId(id) => self.service_ids.get(id).cloned().unwrap_or_default(),
            Target::Identity(identity) => {
                self.identities.get(identity).cloned().unwrap_or_default()
            }
            Target::MachineIdentity(target) => self
                .machine_identities
                .get(target)
                .cloned()
                .unwrap_or_default(),
        }
    }

    fn caller_project(&self, source: IpAddr) -> Option<&ProjectName> {
        let IpAddr::V4(source) = source else {
            return None;
        };
        match self.callers.get(&source) {
            Some(CallerProject::Unique(project)) => Some(project),
            Some(CallerProject::Ambiguous) | None => None,
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
                    info.src.ip(),
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
                let response = MessageResponseBuilder::from_message_request(request).build(
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

pub async fn run(
    machine: Machine,
    replicated: ReplicatedStore,
    upstreams: Option<Vec<SocketAddr>>,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let gateway = machine.subnet.gateway().0;
    let listen_address = SocketAddr::new(IpAddr::V4(gateway), PORT);
    let mut changes = replicated
        .subscribe_container_changes()
        .await
        .map_err(io::Error::other)?;
    let observations = replicated.containers().await.map_err(io::Error::other)?;
    let projection = Arc::new(RwLock::new(Projection::from_observations(
        &observations.observations,
    )));
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
    let projection = watch_projection(replicated, projection, &mut changes, shutdown);
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
    projection: Arc<RwLock<Projection>>,
    changes: &mut Subscription,
    shutdown: CancellationToken,
) -> io::Result<()> {
    loop {
        tokio::select! {
            changed = changes.changed() => {
                changed.map_err(io::Error::other)?;
                match replicated.containers().await {
                    Ok(observations) => {
                        *projection
                            .write()
                            .map_err(|_| io::Error::other("DNS projection lock poisoned"))? =
                            Projection::from_observations(&observations.observations);
                    }
                    Err(error) => eprintln!("failed to rebuild DNS projection: {error}"),
                }
            }
            () = shutdown.cancelled() => return Ok(()),
        }
    }
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
