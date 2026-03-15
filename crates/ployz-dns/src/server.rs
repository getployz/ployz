use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use async_trait::async_trait;
use hickory_server::ServerFuture;
use hickory_server::authority::MessageResponseBuilder;
use hickory_server::proto::op::{Header, ResponseCode};
use hickory_server::proto::rr::rdata::{A, TXT};
use hickory_server::proto::rr::{Name, RData, Record, RecordType};
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::oneshot;
use tracing::{info, trace, warn};

use crate::config::{DnsConfig, DnsError};
use crate::resolve::{ResolveResult, parse_query, resolve};
use crate::snapshot::{SharedDnsSnapshot, project_dns};
use crate::sync::DnsStore;

// TTL 0: clients always re-query, ensuring they never route to a
// drained or removed instance. The resolver is local and serves from
// an in-memory snapshot, so the cost per query is negligible.
const DNS_TTL: u32 = 0;
const TCP_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// DnsHandler — implements hickory-server RequestHandler
// ---------------------------------------------------------------------------

struct DnsHandler {
    snapshot: SharedDnsSnapshot,
}

#[async_trait]
impl RequestHandler for DnsHandler {
    async fn handle_request<R: ResponseHandler>(
        &self,
        request: &Request,
        mut response_handle: R,
    ) -> ResponseInfo {
        let src_ip = request.src().ip();

        let request_info = match request.request_info() {
            Ok(info) => info,
            Err(err) => {
                warn!(?err, "malformed dns request");
                return send_formerr(request, &mut response_handle).await;
            }
        };

        let name_str = request_info.query.name().to_string();
        let rtype = request_info.query.query_type();
        let query_name: Name = request_info.query.name().into();

        let snapshot = self.snapshot.load();

        // Determine caller namespace from source IPv4
        let caller_namespace = match src_ip {
            IpAddr::V4(ip) => snapshot.ip_to_namespace.get(&ip).cloned(),
            IpAddr::V6(_) => None,
        };

        let dns_query = parse_query(&name_str);
        let result = resolve(&snapshot, dns_query, caller_namespace.as_ref());

        trace!(
            src = %src_ip,
            query = %name_str,
            ?rtype,
            ns = ?caller_namespace,
            ?result,
            "dns query"
        );

        let builder = MessageResponseBuilder::from_message_request(request);
        let mut header = Header::response_from_request(request.header());

        match result {
            ResolveResult::Addresses(ips) if rtype == RecordType::A || rtype == RecordType::ANY => {
                let records: Vec<Record> = ips
                    .into_iter()
                    .map(|ip| Record::from_rdata(query_name.clone(), DNS_TTL, RData::A(A(ip))))
                    .collect();
                header.set_response_code(ResponseCode::NoError);
                let response = builder.build(header, records.iter(), &[], &[], &[]);
                send(&mut response_handle, response).await
            }
            ResolveResult::ServiceList(names)
                if rtype == RecordType::TXT || rtype == RecordType::ANY =>
            {
                let records: Vec<Record> = names
                    .into_iter()
                    .map(|name| {
                        Record::from_rdata(
                            query_name.clone(),
                            DNS_TTL,
                            RData::TXT(TXT::new(vec![name])),
                        )
                    })
                    .collect();
                header.set_response_code(ResponseCode::NoError);
                let response = builder.build(header, records.iter(), &[], &[], &[]);
                send(&mut response_handle, response).await
            }
            ResolveResult::Addresses(_) | ResolveResult::ServiceList(_) => {
                // Query type doesn't match (e.g., AAAA for an A-only record)
                header.set_response_code(ResponseCode::NoError);
                let response = builder.build_no_records(header);
                send(&mut response_handle, response).await
            }
            ResolveResult::NxDomain => {
                header.set_response_code(ResponseCode::NXDomain);
                let response = builder.build_no_records(header);
                send(&mut response_handle, response).await
            }
        }
    }
}

async fn send<'a, R: ResponseHandler>(
    response_handle: &mut R,
    response: hickory_server::authority::MessageResponse<
        '_,
        'a,
        impl Iterator<Item = &'a Record> + Send + 'a,
        impl Iterator<Item = &'a Record> + Send + 'a,
        impl Iterator<Item = &'a Record> + Send + 'a,
        impl Iterator<Item = &'a Record> + Send + 'a,
    >,
) -> ResponseInfo {
    match response_handle.send_response(response).await {
        Ok(info) => info,
        Err(err) => {
            warn!(?err, "failed to send DNS response");
            servfail_response()
        }
    }
}

async fn send_formerr<R: ResponseHandler>(
    request: &Request,
    response_handle: &mut R,
) -> ResponseInfo {
    let builder = MessageResponseBuilder::from_message_request(request);
    let mut header = Header::response_from_request(request.header());
    header.set_response_code(ResponseCode::FormErr);
    let response = builder.build_no_records(header);
    match response_handle.send_response(response).await {
        Ok(info) => info,
        Err(_) => servfail_response(),
    }
}

fn servfail_response() -> ResponseInfo {
    let mut header = Header::new();
    header.set_response_code(ResponseCode::ServFail);
    ResponseInfo::from(header)
}

// ---------------------------------------------------------------------------
// Server startup
// ---------------------------------------------------------------------------

pub async fn run_dns_server(
    listen_addrs: &[SocketAddr],
    snapshot: SharedDnsSnapshot,
    mut shutdown_rx: oneshot::Receiver<()>,
) -> Result<(), DnsError> {
    let handler = DnsHandler { snapshot };
    let mut server = ServerFuture::new(handler);

    for listen_addr in listen_addrs {
        let udp_socket = UdpSocket::bind(*listen_addr)
            .await
            .map_err(|err| DnsError::Runtime(format!("bind UDP {listen_addr}: {err}")))?;
        server.register_socket(udp_socket);

        let tcp_listener = TcpListener::bind(*listen_addr)
            .await
            .map_err(|err| DnsError::Runtime(format!("bind TCP {listen_addr}: {err}")))?;
        server.register_listener(tcp_listener, TCP_TIMEOUT);

        info!(listen = %listen_addr, "dns server listening");
    }

    tokio::select! {
        result = server.block_until_done() => {
            result.map_err(|err| DnsError::Runtime(format!("dns server error: {err}")))
        }
        _ = &mut shutdown_rx => {
            info!("dns server shutting down");
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Standalone process entry point
// ---------------------------------------------------------------------------

pub fn run_dns_process() -> Result<(), DnsError> {
    let config = DnsConfig::from_env()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| DnsError::Runtime(err.to_string()))?;

    runtime.block_on(async {
        let store =
            ployz_corrosion::CorrosionStore::connect_for_network(&config.data_dir, &config.network)
                .await
                .map_err(|err| DnsError::Store(err.to_string()))?;

        let state = DnsStore::load_routing_state(&store).await?;
        let initial_snapshot = project_dns(&state);
        let shared = SharedDnsSnapshot::new(initial_snapshot);

        tokio::spawn(crate::sync::run_sync_loop(store, shared.clone()));

        let mut listen_addrs = vec![
            config
                .overlay_listen_addr
                .parse()
                .map_err(|err| {
                    DnsError::Config(format!(
                        "invalid overlay listen addr '{}': {err}",
                        config.overlay_listen_addr
                    ))
                })?,
        ];
        if let Some(bridge_listen_addr) = &config.bridge_listen_addr {
            listen_addrs.push(bridge_listen_addr.parse().map_err(|err| {
                DnsError::Config(format!(
                    "invalid bridge listen addr '{}': {err}",
                    bridge_listen_addr
                ))
            })?);
        }
        listen_addrs.sort();
        listen_addrs.dedup();

        let (_shutdown_tx, shutdown_rx) = oneshot::channel();
        run_dns_server(&listen_addrs, shared, shutdown_rx).await
    })
}
