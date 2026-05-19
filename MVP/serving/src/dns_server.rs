use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use hickory_proto::op::{Message, OpCode, ResponseCode};
use hickory_proto::rr::{Name, RData, Record, RecordType};
use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};

use crate::{WireMetricsRecorder, WireRoleMetrics, WireServingState};

const MAX_DNS_PACKET_BYTES: usize = 1232;
const MAX_DNS_REQUESTS: usize = 512;
const MAX_LATENCY_SAMPLES: usize = 256;

pub type DnsServerResult<T> = Result<T, DnsServerError>;

#[derive(Debug, Error)]
pub enum DnsServerError {
    #[error("bind DNS listener at {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        source: std::io::Error,
    },
    #[error("read DNS listener local address: {0}")]
    LocalAddr(std::io::Error),
    #[error("receive DNS packet: {0}")]
    Receive(std::io::Error),
}

pub struct DnsServerHandle {
    listen_addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<DnsServerResult<()>>>,
    metrics: WireMetricsRecorder,
}

impl DnsServerHandle {
    #[must_use]
    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    #[must_use]
    pub fn metrics(&self) -> WireRoleMetrics {
        self.metrics.snapshot()
    }

    pub async fn shutdown(mut self) -> DnsServerResult<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let Some(task) = self.task.take() else {
            return Ok(());
        };
        match task.await {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(DnsServerError::Receive(std::io::Error::other(format!(
                "dns task join: {error}"
            )))),
        }
    }
}

impl Drop for DnsServerHandle {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

pub async fn spawn_dns_server(
    listen_addr: SocketAddr,
    state: WireServingState,
) -> DnsServerResult<DnsServerHandle> {
    let socket = UdpSocket::bind(listen_addr)
        .await
        .map_err(|source| DnsServerError::Bind {
            addr: listen_addr,
            source,
        })?;
    let listen_addr = socket.local_addr().map_err(DnsServerError::LocalAddr)?;
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let metrics = WireMetricsRecorder::new(MAX_LATENCY_SAMPLES);
    let server_metrics = metrics.clone();
    let socket = Arc::new(socket);
    let task = tokio::spawn(async move {
        let mut packet = [0_u8; MAX_DNS_PACKET_BYTES];
        let mut requests = JoinSet::new();
        let state = Arc::new(state);
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    requests.abort_all();
                    while requests.join_next().await.is_some() {}
                    return Ok(());
                },
                Some(joined) = requests.join_next(), if !requests.is_empty() => {
                    let _ = joined;
                },
                received = socket.recv_from(&mut packet) => {
                    let (len, peer) = received.map_err(DnsServerError::Receive)?;
                    if requests.len() >= MAX_DNS_REQUESTS {
                        continue;
                    }
                    let packet = packet[..len].to_vec();
                    let socket = Arc::clone(&socket);
                    let state = Arc::clone(&state);
                    let metrics = server_metrics.clone();
                    requests.spawn(async move {
                        let response = handle_packet(&packet, state.as_ref(), &metrics).await;
                        if let Some(response) = response {
                            let _ = socket.send_to(&response, peer).await;
                        }
                    });
                }
            }
        }
    });
    Ok(DnsServerHandle {
        listen_addr,
        shutdown: Some(shutdown_tx),
        task: Some(task),
        metrics,
    })
}

async fn handle_packet(
    packet: &[u8],
    state: &WireServingState,
    metrics: &WireMetricsRecorder,
) -> Option<Vec<u8>> {
    let started = Instant::now();
    let request = match Message::from_vec(packet) {
        Ok(request) => request,
        Err(_) => {
            metrics.record_malformed_dns();
            metrics.record_request(started.elapsed());
            return None;
        }
    };
    let response = response_for_request(request, state).await;
    metrics.record_request(started.elapsed());
    response.to_vec().ok()
}

async fn response_for_request(request: Message, state: &WireServingState) -> Message {
    let mut response = Message::response(request.metadata.id, request.metadata.op_code);
    response.metadata.authoritative = true;
    response.metadata.recursion_desired = request.metadata.recursion_desired;
    response.metadata.recursion_available = false;
    if request.metadata.op_code != OpCode::Query {
        response.metadata.response_code = ResponseCode::NotImp;
        return response;
    }
    let Some(query) = request.queries.first().cloned() else {
        response.metadata.response_code = ResponseCode::FormErr;
        return response;
    };
    response.add_query(query.clone());
    let lookup_name = lookup_name(query.name());
    let record_type = match query.query_type() {
        RecordType::A => "A",
        RecordType::AAAA => "AAAA",
        _ => {
            response.metadata.response_code = ResponseCode::NoError;
            return response;
        }
    };
    let records = match state.dns_records(lookup_name, record_type).await {
        Ok(records) => records,
        Err(_) => {
            response.metadata.response_code = ResponseCode::ServFail;
            return response;
        }
    };
    if records.is_empty() {
        response.metadata.response_code = ResponseCode::NXDomain;
        return response;
    }
    for record in records {
        match record.record_type.as_str() {
            "A" => {
                let Ok(addr) = record.value.parse::<Ipv4Addr>() else {
                    continue;
                };
                response.add_answer(Record::from_rdata(
                    query.name().clone(),
                    record.ttl_seconds,
                    RData::A(addr.into()),
                ));
            }
            "AAAA" => {
                let Ok(addr) = record.value.parse::<Ipv6Addr>() else {
                    continue;
                };
                response.add_answer(Record::from_rdata(
                    query.name().clone(),
                    record.ttl_seconds,
                    RData::AAAA(addr.into()),
                ));
            }
            _ => {}
        }
    }
    response
}

fn lookup_name(name: &Name) -> String {
    name.to_ascii().trim_end_matches('.').to_string()
}
