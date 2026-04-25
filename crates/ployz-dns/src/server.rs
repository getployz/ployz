use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use hickory_server::ServerFuture;
use hickory_server::authority::MessageResponseBuilder;
use hickory_server::proto::op::{Header, ResponseCode};
use hickory_server::proto::rr::rdata::{A, TXT};
use hickory_server::proto::rr::{Name, RData, Record, RecordType};
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};
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
const STORE_READY_TIMEOUT: Duration = Duration::from_secs(30);
const STORE_READY_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
const STORE_READY_POLL: Duration = Duration::from_millis(250);

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
        let started_at = Instant::now();
        let src_ip = request.src().ip();

        let request_info = match request.request_info() {
            Ok(info) => info,
            Err(err) => {
                warn!(?err, "malformed dns request");
                let response = send_formerr(request, &mut response_handle).await;
                crate::metrics::observe_query(
                    "UNKNOWN",
                    response.response_code(),
                    started_at.elapsed(),
                );
                return response;
            }
        };

        let name_str = request_info.query.name().to_string();
        let rtype = request_info.query.query_type();
        let query_type = rtype.to_string();
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

        let response = match result {
            ResolveResult::Addresses(ips) if rtype == RecordType::A || rtype == RecordType::ANY => {
                let records: Vec<Record> = ips
                    .into_iter()
                    .map(|ip| Record::from_rdata(query_name.clone(), DNS_TTL, RData::A(A(ip))))
                    .collect();
                header.set_response_code(ResponseCode::NoError);
                let response = builder.build(header, records.iter(), &[], &[], &[]);
                send(&mut response_handle, response).await
            }
            ResolveResult::ServiceList(names) | ResolveResult::InstanceList(names)
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
            ResolveResult::Addresses(_)
            | ResolveResult::ServiceList(_)
            | ResolveResult::InstanceList(_) => {
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
        };
        crate::metrics::observe_query(&query_type, response.response_code(), started_at.elapsed());
        response
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

pub fn run_dns_process_with_store<S>(config: DnsConfig, store: S) -> Result<(), DnsError>
where
    S: DnsStore + Send + Sync + 'static,
{
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("ployz-dns-async")
        .worker_threads(2)
        .build()
        .map_err(|err| DnsError::Runtime(err.to_string()))?;
    run_dns_process_on_runtime(runtime, config, store)
}

/// Run the DNS process on an externally-provided tokio runtime.
///
/// Corrosion's `reqwest::Client` pins its HTTP/2 connection driver to the
/// runtime that first used it, so the store must stay on a single runtime
/// for the entire lifetime of the process.
pub fn run_dns_process_on_runtime<S>(
    runtime: tokio::runtime::Runtime,
    config: DnsConfig,
    store: S,
) -> Result<(), DnsError>
where
    S: DnsStore + Send + Sync + 'static,
{
    runtime.block_on(async {
        if let Some(metrics_listen_addr) = config.metrics_listen_addr.as_deref() {
            let metrics_addr = ployz_metrics::spawn_metrics_listener(metrics_listen_addr)
                .await
                .map_err(|err| {
                    DnsError::Runtime(format!(
                        "start dns metrics listener on {metrics_listen_addr}: {err}"
                    ))
                })?;
            info!(listen = %metrics_addr, "dns metrics listener running");
        }

        let state = wait_for_initial_routing_state(&store).await?;
        let initial_snapshot = project_dns(&state);
        let shared = SharedDnsSnapshot::new(initial_snapshot);

        tokio::spawn(crate::sync::run_sync_loop(store, shared.clone()));

        let mut listen_addrs = vec![config.overlay_listen_addr.parse().map_err(|err| {
            DnsError::Config(format!(
                "invalid overlay listen addr '{}': {err}",
                config.overlay_listen_addr
            ))
        })?];
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

async fn wait_for_initial_routing_state<S>(
    store: &S,
) -> Result<ployz_types::model::RoutingState, DnsError>
where
    S: DnsStore + Send + Sync,
{
    let deadline = tokio::time::Instant::now() + STORE_READY_TIMEOUT;
    loop {
        match timeout(
            STORE_READY_ATTEMPT_TIMEOUT,
            DnsStore::load_routing_state(store),
        )
        .await
        {
            Ok(Ok(state)) => return Ok(state),
            Ok(Err(error)) if tokio::time::Instant::now() < deadline => {
                warn!(?error, "dns waiting for corrosion query readiness");
            }
            Err(_) if tokio::time::Instant::now() < deadline => {
                warn!("dns timed out loading initial routing state; retrying");
            }
            Ok(Err(error)) => {
                return Err(DnsError::Store(format!(
                    "corrosion query API did not become ready within {:?}: {error}",
                    STORE_READY_TIMEOUT
                )));
            }
            Err(_) => {
                return Err(DnsError::Store(format!(
                    "corrosion query API did not return initial routing state within {:?}",
                    STORE_READY_TIMEOUT
                )));
            }
        }
        sleep(STORE_READY_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::run_dns_server;
    use crate::{DnsInstanceDiagnostic, SharedDnsSnapshot};
    use hickory_server::proto::op::{Message, MessageType, OpCode, Query, ResponseCode};
    use hickory_server::proto::rr::rdata::A;
    use hickory_server::proto::rr::{Name, RData, RecordType};
    use std::collections::HashMap;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpStream, UdpSocket};
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn dns_metrics_listener_reports_queries_and_rcodes() {
        ployz_metrics::set_build_info("ployz-dns", env!("CARGO_PKG_VERSION"));
        let metrics_addr = free_local_addr();
        let dns_addr = free_local_addr();
        let (_shutdown_tx, shutdown_rx) = oneshot::channel();

        ployz_metrics::spawn_metrics_listener(&metrics_addr.to_string())
            .await
            .expect("metrics listener should start");

        let mut services = HashMap::new();
        services.insert(
            ployz_types::spec::Namespace("prod".into()),
            HashMap::from([("web".into(), vec![Ipv4Addr::new(10, 42, 0, 2)])]),
        );
        let snapshot = crate::DnsSnapshot {
            services,
            ip_to_namespace: HashMap::new(),
            service_names: HashMap::from([(
                ployz_types::spec::Namespace("prod".into()),
                vec!["web".into()],
            )]),
            instances: HashMap::new(),
        };
        let listen_addrs = vec![dns_addr];

        let server = tokio::spawn(async move {
            run_dns_server(&listen_addrs, SharedDnsSnapshot::new(snapshot), shutdown_rx).await
        });

        send_dns_query(dns_addr, "web.prod.ployz.internal.", RecordType::A).await;
        send_dns_query(dns_addr, "missing.prod.ployz.internal.", RecordType::A).await;

        let metrics = fetch_http_body(metrics_addr, "/metrics").await;
        assert!(metrics.contains("ployz_dns_queries_total{qtype=\"A\",rcode=\"NOERROR\"}"));
        assert!(metrics.contains("ployz_dns_queries_total{qtype=\"A\",rcode=\"NXDOMAIN\"}"));
        assert!(
            metrics
                .contains("ployz_dns_query_duration_seconds_count{qtype=\"A\",rcode=\"NOERROR\"}")
        );
        assert!(
            metrics
                .contains("ployz_dns_query_duration_seconds_count{qtype=\"A\",rcode=\"NXDOMAIN\"}")
        );

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn dns_server_returns_instance_txt_records() {
        let dns_addr = free_local_addr();
        let (_shutdown_tx, shutdown_rx) = oneshot::channel();
        let namespace = ployz_types::spec::Namespace("prod".into());
        let instance = DnsInstanceDiagnostic {
            service: "web".into(),
            instance_id: "inst-1".into(),
            machine_id: "machine-1".into(),
            slot_id: "slot-1".into(),
            overlay_ip: Ipv4Addr::new(10, 42, 0, 2),
        };
        let snapshot = crate::DnsSnapshot {
            services: HashMap::from([(
                namespace.clone(),
                HashMap::from([("web".into(), vec![instance.overlay_ip])]),
            )]),
            ip_to_namespace: HashMap::new(),
            service_names: HashMap::from([(namespace.clone(), vec!["web".into()])]),
            instances: HashMap::from([(namespace, vec![instance])]),
        };
        let listen_addrs = vec![dns_addr];

        let server = tokio::spawn(async move {
            run_dns_server(&listen_addrs, SharedDnsSnapshot::new(snapshot), shutdown_rx).await
        });

        let response = send_dns_query(
            dns_addr,
            "_instances.web.ns.prod.ployz.internal.",
            RecordType::TXT,
        )
        .await;

        assert_eq!(response.response_code(), ResponseCode::NoError);
        assert_eq!(
            txt_answers(&response),
            vec!["service=web,instance=inst-1,machine=machine-1,slot=slot-1,ip=10.42.0.2"]
        );

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn dns_server_returns_direct_instance_a_record() {
        let dns_addr = free_local_addr();
        let (_shutdown_tx, shutdown_rx) = oneshot::channel();
        let namespace = ployz_types::spec::Namespace("prod".into());
        let instance = DnsInstanceDiagnostic {
            service: "web".into(),
            instance_id: "inst-1".into(),
            machine_id: "machine-1".into(),
            slot_id: "slot-1".into(),
            overlay_ip: Ipv4Addr::new(10, 42, 0, 2),
        };
        let snapshot = crate::DnsSnapshot {
            services: HashMap::from([(
                namespace.clone(),
                HashMap::from([("web".into(), vec![instance.overlay_ip])]),
            )]),
            ip_to_namespace: HashMap::new(),
            service_names: HashMap::from([(namespace.clone(), vec!["web".into()])]),
            instances: HashMap::from([(namespace, vec![instance])]),
        };
        let listen_addrs = vec![dns_addr];

        let server = tokio::spawn(async move {
            run_dns_server(&listen_addrs, SharedDnsSnapshot::new(snapshot), shutdown_rx).await
        });

        let response = send_dns_query(
            dns_addr,
            "inst-1.instance.web.prod.ployz.internal.",
            RecordType::A,
        )
        .await;

        assert_eq!(response.response_code(), ResponseCode::NoError);
        assert_eq!(a_answers(&response), vec![Ipv4Addr::new(10, 42, 0, 2)]);

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn dns_server_returns_no_records_for_known_name_wrong_type() {
        let dns_addr = free_local_addr();
        let (_shutdown_tx, shutdown_rx) = oneshot::channel();
        let namespace = ployz_types::spec::Namespace("prod".into());
        let instance = DnsInstanceDiagnostic {
            service: "web".into(),
            instance_id: "inst-1".into(),
            machine_id: "machine-1".into(),
            slot_id: "slot-1".into(),
            overlay_ip: Ipv4Addr::new(10, 42, 0, 2),
        };
        let snapshot = crate::DnsSnapshot {
            services: HashMap::from([(
                namespace.clone(),
                HashMap::from([("web".into(), vec![instance.overlay_ip])]),
            )]),
            ip_to_namespace: HashMap::new(),
            service_names: HashMap::from([(namespace.clone(), vec!["web".into()])]),
            instances: HashMap::from([(namespace, vec![instance])]),
        };
        let listen_addrs = vec![dns_addr];

        let server = tokio::spawn(async move {
            run_dns_server(&listen_addrs, SharedDnsSnapshot::new(snapshot), shutdown_rx).await
        });

        let response = send_dns_query(
            dns_addr,
            "inst-1.instance.web.prod.ployz.internal.",
            RecordType::TXT,
        )
        .await;

        assert_eq!(response.response_code(), ResponseCode::NoError);
        assert!(response.answers().is_empty());

        server.abort();
        let _ = server.await;
    }

    async fn send_dns_query(addr: SocketAddr, name: &str, record_type: RecordType) -> Message {
        let mut message = Message::new();
        message
            .set_id(7)
            .set_message_type(MessageType::Query)
            .set_op_code(OpCode::Query)
            .set_recursion_desired(true)
            .add_query(Query::query(
                Name::from_ascii(name).expect("dns name should parse"),
                record_type,
            ));

        let bytes = message.to_vec().expect("query should encode");
        let socket = UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("udp socket should bind");
        socket
            .send_to(&bytes, addr)
            .await
            .expect("query should send");

        let mut response = [0_u8; 512];
        let (received, _) = socket
            .recv_from(&mut response)
            .await
            .expect("response should arrive");
        let Some(response_bytes) = response.get(..received) else {
            panic!("response length exceeded buffer");
        };
        Message::from_vec(response_bytes).expect("response should decode")
    }

    fn a_answers(response: &Message) -> Vec<Ipv4Addr> {
        response
            .answers()
            .iter()
            .filter_map(|record| match record.data() {
                RData::A(A(ip)) => Some(*ip),
                _ => None,
            })
            .collect()
    }

    fn txt_answers(response: &Message) -> Vec<String> {
        response
            .answers()
            .iter()
            .filter_map(|record| match record.data() {
                RData::TXT(txt) => Some(
                    txt.iter()
                        .flat_map(|bytes| bytes.iter().copied())
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .map(|bytes| String::from_utf8(bytes).expect("TXT answer should be valid utf-8"))
            .collect()
    }

    async fn fetch_http_body(addr: SocketAddr, path: &str) -> String {
        for _ in 0..50 {
            if let Ok(mut stream) = TcpStream::connect(addr).await {
                let request = format!(
                    "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                    addr
                );
                stream
                    .write_all(request.as_bytes())
                    .await
                    .expect("request write should succeed");
                let mut response = Vec::new();
                stream
                    .read_to_end(&mut response)
                    .await
                    .expect("response read should succeed");
                let response = String::from_utf8(response).expect("response should be utf-8");
                let Some((_, body)) = response.split_once("\r\n\r\n") else {
                    panic!("http response should contain headers");
                };
                return body.to_string();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out fetching metrics from {addr}");
    }

    fn free_local_addr() -> SocketAddr {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("should bind ephemeral port")
            .local_addr()
            .expect("ephemeral listener should have local addr")
    }
}
