//! Internal DNS on the wire: the request handler, forwarding of every other name to
//! the upstream resolvers, and the bounded drain at shutdown.

use std::{
    fs, io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use hickory_server::proto::{
    op::{Edns, Header, HeaderCounts, Message, Metadata, ResponseCode},
    rr::Record,
    serialize::binary::BinEncoder,
};
use hickory_server::{
    Server,
    net::{runtime::Time, xfer::Protocol},
    server::{Request, RequestHandler, ResponseHandler, ResponseInfo},
    zone_handler::MessageResponseBuilder,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    sync::watch,
};
use tokio_util::sync::CancellationToken;

use super::{
    PORT,
    projection::{Answers, ResponsePlan},
};

const FORWARD_TIMEOUT: Duration = Duration::from_secs(3);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub(super) struct Upstreams(Arc<UpstreamSource>);

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
    pub(super) fn of(fixed: &[SocketAddr], resolv_conf: &Path, listen: Ipv4Addr) -> Self {
        if fixed.is_empty() {
            Self::follow(resolv_conf, listen)
        } else {
            Self::fixed(fixed.to_vec())
        }
    }

    pub(super) fn fixed(addresses: Vec<SocketAddr>) -> Self {
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

    pub(super) fn snapshot(&self) -> Vec<SocketAddr> {
        match &*self.0 {
            UpstreamSource::Fixed(addresses) => addresses.clone(),
            UpstreamSource::ResolvConf { state, .. } => state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .nameservers
                .clone(),
        }
    }

    pub(super) fn reload_if_changed(&self) -> bool {
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
pub(super) struct InFlight(watch::Sender<usize>);

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

pub(super) struct Handler {
    answers: Answers,
    upstreams: Upstreams,
    in_flight: InFlight,
}

impl Handler {
    pub(super) fn new(answers: Answers, upstreams: Upstreams) -> Self {
        Self {
            answers,
            upstreams,
            in_flight: InFlight::new(),
        }
    }

    pub(super) fn in_flight(&self) -> InFlight {
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
        let plan = self
            .answers
            .settled_plan(
                info.query.original().name(),
                info.query.query_type(),
                info.src.ip(),
            )
            .await;
        let (code, answers) = match plan {
            ResponsePlan::Internal { code, answers } => (code, answers),
            // No projection named the caller within CALLER_WAIT.
            ResponsePlan::UnknownCaller => (ResponseCode::NXDomain, Vec::new()),
            ResponsePlan::Forward => return self.forwarded(request, response_handle).await,
        };
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
}

impl Handler {
    async fn forwarded<R: ResponseHandler>(
        &self,
        request: &Request,
        mut response_handle: R,
    ) -> ResponseInfo {
        match self.forward(request).await {
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
        }
    }

    async fn forward(&self, request: &Request) -> io::Result<Message> {
        let mut last_error = io::Error::other("no upstream DNS servers configured");
        for upstream in self.upstreams.snapshot() {
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
pub(super) async fn run_server(
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

pub(super) fn nameservers_from_resolv_conf(text: &str, listen: Ipv4Addr) -> Vec<SocketAddr> {
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
