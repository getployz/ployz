//! Dumb HTTP-facing gateway boundary.

use crate::gateway::GatewayUpstream;
use crate::gateway_runtime::{GatewayRouteSelectionError, GatewayRouteTable};
use ployz_core::ops::{RouteHostname, RouteHostnameError, RoutePort, RouteTarget};
use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

const MAX_HTTP_REQUEST_HEAD_BYTES: usize = 64 * 1024;
const GATEWAY_HTTP_PROXY_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn proxy_connection_by_first_http_host<S>(
    routes: &GatewayRouteTable,
    stream: &mut S,
    listener_port: RoutePort,
) -> Result<(), GatewayHttpProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    tokio::time::timeout(
        GATEWAY_HTTP_PROXY_TIMEOUT,
        proxy_connection_by_first_http_host_without_timeout(routes, stream, listener_port),
    )
    .await
    .map_err(|_| GatewayHttpProxyError::ProxyTimedOut {
        timeout: GATEWAY_HTTP_PROXY_TIMEOUT,
    })?
}

pub async fn write_gateway_http_error_response<S>(
    stream: &mut S,
    error: &GatewayHttpProxyError,
) -> Result<(), std::io::ErrorKind>
where
    S: AsyncWrite + Unpin,
{
    let Some(response) = gateway_http_error_response(error) else {
        return Ok(());
    };
    stream
        .write_all(&response)
        .await
        .map_err(|source| source.kind())
}

fn gateway_http_error_response(error: &GatewayHttpProxyError) -> Option<Vec<u8>> {
    let (status, body) = match error {
        GatewayHttpProxyError::RequestTooLarge { .. } => (
            "431 Request Header Fields Too Large",
            "request header too large\n",
        ),
        GatewayHttpProxyError::MissingHost
        | GatewayHttpProxyError::Route(GatewayHttpRouteError::InvalidTarget(_)) => {
            ("400 Bad Request", "bad request\n")
        }
        GatewayHttpProxyError::Route(GatewayHttpRouteError::RouteSelection(
            GatewayRouteSelectionError::NoRoute { .. },
        )) => ("404 Not Found", "not found\n"),
        GatewayHttpProxyError::Route(GatewayHttpRouteError::RouteSelection(
            GatewayRouteSelectionError::RouteTableUnavailable
            | GatewayRouteSelectionError::NoUpstream { .. },
        ))
        | GatewayHttpProxyError::ConnectUpstream { .. }
        | GatewayHttpProxyError::WriteUpstream { .. } => {
            ("503 Service Unavailable", "service unavailable\n")
        }
        GatewayHttpProxyError::ProxyTimedOut { .. } => ("504 Gateway Timeout", "gateway timeout\n"),
        GatewayHttpProxyError::ReadClient { .. } | GatewayHttpProxyError::Relay { .. } => {
            return None;
        }
    };

    Some(
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes(),
    )
}

async fn proxy_connection_by_first_http_host_without_timeout<S>(
    routes: &GatewayRouteTable,
    stream: &mut S,
    listener_port: RoutePort,
) -> Result<(), GatewayHttpProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let request = read_http_request_head(stream).await?;
    let authority =
        request_host_authority(request.head()).ok_or(GatewayHttpProxyError::MissingHost)?;
    let upstream = select_http_upstream(routes, authority, listener_port)
        .map_err(GatewayHttpProxyError::Route)?;

    proxy_buffered_http_request(stream, request.into_bytes(), upstream).await
}

pub fn select_http_upstream(
    routes: &GatewayRouteTable,
    authority: &str,
    listener_port: RoutePort,
) -> Result<GatewayUpstream, GatewayHttpRouteError> {
    let target = route_target_from_authority(authority, listener_port)
        .map_err(GatewayHttpRouteError::InvalidTarget)?;
    routes
        .select_upstream(&target)
        .map_err(GatewayHttpRouteError::RouteSelection)
}

pub fn route_target_from_authority(
    authority: &str,
    listener_port: RoutePort,
) -> Result<RouteTarget, HttpRouteTargetError> {
    let authority = authority.trim();
    if authority.is_empty() {
        return Err(HttpRouteTargetError::EmptyHost);
    }
    if authority.contains('@') {
        return Err(HttpRouteTargetError::UserInfo);
    }

    let (hostname, port) = host_and_port(authority, listener_port)?;
    let hostname = RouteHostname::try_new(hostname)
        .map_err(|source| HttpRouteTargetError::InvalidHostname { source })?;

    Ok(RouteTarget::try_new(hostname, port))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayHttpRouteError {
    InvalidTarget(HttpRouteTargetError),
    RouteSelection(GatewayRouteSelectionError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpRouteTargetError {
    EmptyHost,
    UserInfo,
    MissingHost,
    MissingPort,
    InvalidHostname { source: RouteHostnameError },
    InvalidPort,
    UnsupportedIpv6,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayHttpProxyError {
    RequestTooLarge {
        limit: usize,
    },
    MissingHost,
    ReadClient {
        source: std::io::ErrorKind,
    },
    ProxyTimedOut {
        timeout: Duration,
    },
    Relay {
        source: std::io::ErrorKind,
    },
    Route(GatewayHttpRouteError),
    ConnectUpstream {
        upstream: GatewayUpstream,
        source: std::io::ErrorKind,
    },
    WriteUpstream {
        upstream: GatewayUpstream,
        source: std::io::ErrorKind,
    },
}

impl fmt::Display for GatewayHttpProxyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RequestTooLarge { limit } => {
                write!(formatter, "HTTP request header exceeded {limit} bytes")
            }
            Self::MissingHost => formatter.write_str("HTTP request is missing Host header"),
            Self::ReadClient { source } => write!(formatter, "failed to read client: {source}"),
            Self::ProxyTimedOut { timeout } => {
                write!(
                    formatter,
                    "HTTP proxy timed out after {}s",
                    timeout.as_secs()
                )
            }
            Self::Relay { source } => write!(formatter, "failed to relay HTTP stream: {source}"),
            Self::Route(error) => write!(formatter, "failed to route HTTP request: {error:?}"),
            Self::ConnectUpstream { upstream, source } => {
                write!(
                    formatter,
                    "failed to connect upstream {upstream:?}: {source}"
                )
            }
            Self::WriteUpstream { upstream, source } => {
                write!(formatter, "failed to write upstream {upstream:?}: {source}")
            }
        }
    }
}

fn host_and_port(
    authority: &str,
    listener_port: RoutePort,
) -> Result<(&str, RoutePort), HttpRouteTargetError> {
    let Some((hostname, port)) = authority.rsplit_once(':') else {
        return Ok((authority, listener_port));
    };
    if hostname.contains(':') {
        return Err(HttpRouteTargetError::UnsupportedIpv6);
    }
    if hostname.is_empty() {
        return Err(HttpRouteTargetError::MissingHost);
    }
    if port.is_empty() {
        return Err(HttpRouteTargetError::MissingPort);
    }
    let port = port
        .parse::<u16>()
        .ok()
        .and_then(|value| RoutePort::try_new(value).ok())
        .ok_or(HttpRouteTargetError::InvalidPort)?;

    Ok((hostname, port))
}

async fn proxy_buffered_http_request<S>(
    stream: &mut S,
    request: Vec<u8>,
    upstream: GatewayUpstream,
) -> Result<(), GatewayHttpProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let upstream_addr = SocketAddr::new(upstream.endpoint.ip, upstream.endpoint.port.get());
    let mut upstream_stream = TcpStream::connect(upstream_addr).await.map_err(|source| {
        GatewayHttpProxyError::ConnectUpstream {
            upstream: upstream.clone(),
            source: source.kind(),
        }
    })?;
    upstream_stream
        .write_all(&request)
        .await
        .map_err(|source| GatewayHttpProxyError::WriteUpstream {
            upstream: upstream.clone(),
            source: source.kind(),
        })?;

    tokio::time::timeout(
        GATEWAY_HTTP_PROXY_TIMEOUT,
        tokio::io::copy_bidirectional(stream, &mut upstream_stream),
    )
    .await
    .map_err(|_| GatewayHttpProxyError::ProxyTimedOut {
        timeout: GATEWAY_HTTP_PROXY_TIMEOUT,
    })?
    .map_err(|source| GatewayHttpProxyError::Relay {
        source: source.kind(),
    })?;

    Ok(())
}

struct BufferedHttpRequest {
    bytes: Vec<u8>,
    head_len: usize,
}

impl BufferedHttpRequest {
    fn head(&self) -> &[u8] {
        self.bytes
            .get(..self.head_len)
            .expect("HTTP request head length is produced from buffered request bytes")
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

async fn read_http_request_head<S>(
    stream: &mut S,
) -> Result<BufferedHttpRequest, GatewayHttpProxyError>
where
    S: AsyncRead + Unpin,
{
    let mut request = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let read =
            stream
                .read(&mut chunk)
                .await
                .map_err(|source| GatewayHttpProxyError::ReadClient {
                    source: source.kind(),
                })?;
        if read == 0 {
            return Err(GatewayHttpProxyError::ReadClient {
                source: std::io::ErrorKind::UnexpectedEof,
            });
        }
        let Some(bytes) = chunk.get(..read) else {
            return Err(GatewayHttpProxyError::ReadClient {
                source: std::io::ErrorKind::InvalidData,
            });
        };
        request.extend_from_slice(bytes);
        if request.len() > MAX_HTTP_REQUEST_HEAD_BYTES {
            return Err(GatewayHttpProxyError::RequestTooLarge {
                limit: MAX_HTTP_REQUEST_HEAD_BYTES,
            });
        }
        if let Some(head_len) = request_head_len(&request) {
            return Ok(BufferedHttpRequest {
                bytes: request,
                head_len,
            });
        }
    }
}

fn request_head_len(request: &[u8]) -> Option<usize> {
    request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

fn request_host_authority(request: &[u8]) -> Option<&str> {
    let headers = std::str::from_utf8(request).ok()?;
    headers
        .lines()
        .find_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.eq_ignore_ascii_case("host"))
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
}
