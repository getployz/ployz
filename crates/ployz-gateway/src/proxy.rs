use std::net::SocketAddr;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use bytes::Bytes;
use http::Method;
use pingora::prelude::*;
use tracing::info;

use crate::snapshot::SharedSnapshot;

// ---------------------------------------------------------------------------
// GatewayApp — pingora HTTP proxy
// ---------------------------------------------------------------------------

pub struct GatewayApp {
    snapshot: SharedSnapshot,
}

pub struct RequestCtx {
    route_id: Option<String>,
    selected_addr: Option<SocketAddr>,
    upstream_host: Option<String>,
    retry_allowed: bool,
    matched: bool,
    started_at: Option<Instant>,
    downstream_scheme: &'static str,
}

impl Default for RequestCtx {
    fn default() -> Self {
        Self {
            route_id: None,
            selected_addr: None,
            upstream_host: None,
            retry_allowed: false,
            matched: false,
            started_at: None,
            downstream_scheme: "http",
        }
    }
}

impl GatewayApp {
    #[must_use]
    pub fn new(snapshot: SharedSnapshot) -> Self {
        Self { snapshot }
    }
}

#[async_trait]
impl ProxyHttp for GatewayApp {
    type CTX = RequestCtx;

    fn new_ctx(&self) -> Self::CTX {
        RequestCtx::default()
    }

    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<bool> {
        let request = session.req_header();
        let host = request
            .headers
            .get("host")
            .and_then(|value| value.to_str().ok())
            .or_else(|| request.uri.authority().map(|authority| authority.as_str()));
        let path = request.uri.path();
        ctx.downstream_scheme = if session
            .as_downstream()
            .digest()
            .and_then(|digest| digest.ssl_digest.as_ref())
            .is_some()
        {
            "https"
        } else {
            "http"
        };
        let state = self.snapshot.load();
        if let Some(challenge) = state.match_acme_challenge(host, path) {
            ctx.matched = true;
            ctx.started_at = Some(Instant::now());
            session
                .respond_error_with_body(200, Bytes::from(challenge.key_authorization.clone()))
                .await?;
            return Ok(true);
        }

        let Some(route) = state.match_http_route(host, path) else {
            ctx.matched = false;
            ctx.started_at = Some(Instant::now());
            session.respond_error(404).await?;
            return Ok(true);
        };

        ctx.matched = true;
        ctx.route_id = Some(route.route_id.clone());
        ctx.retry_allowed = is_retryable_method(&request.method);
        ctx.upstream_host = host.map(ToOwned::to_owned);
        ctx.started_at = Some(Instant::now());

        Ok(false)
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<Box<HttpPeer>> {
        let Some(route_id) = ctx.route_id.as_deref() else {
            return Err(Error::explain(
                ErrorType::HTTPStatus(500),
                "missing route_id in upstream_peer",
            ));
        };

        let state = self.snapshot.load();
        let Some(lb) = state.load_balancers.get(route_id) else {
            return Err(Error::explain(
                ErrorType::HTTPStatus(503),
                "no load balancer for route",
            ));
        };

        let Some(backend) = lb.select(b"", 256) else {
            return Err(Error::explain(
                ErrorType::HTTPStatus(503),
                "no eligible backend",
            ));
        };

        ctx.selected_addr = backend.addr.as_inet().copied();

        let mut peer = HttpPeer::new(backend.addr, false, String::new());
        peer.options.connection_timeout = Some(Duration::from_secs(2));
        peer.options.total_connection_timeout = Some(Duration::from_secs(5));
        peer.options.read_timeout = Some(Duration::from_secs(30));
        peer.options.write_timeout = Some(Duration::from_secs(30));
        peer.options.idle_timeout = Some(Duration::from_secs(60));
        Ok(Box::new(peer))
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        if let Some(host) = ctx.upstream_host.as_deref() {
            upstream_request
                .insert_header("Host", host)
                .map_err(|err| {
                    Error::because(ErrorType::InternalError, "insert Host header", err)
                })?;
            upstream_request
                .insert_header("X-Forwarded-Host", host)
                .map_err(|err| {
                    Error::because(
                        ErrorType::InternalError,
                        "insert X-Forwarded-Host header",
                        err,
                    )
                })?;
        }
        if let Some(client_addr) = session.client_addr()
            && let Some(address) = client_addr.as_inet()
        {
            let forwarded_for = append_header_value(
                upstream_request,
                "X-Forwarded-For",
                &address.ip().to_string(),
            );
            upstream_request
                .insert_header("X-Forwarded-For", forwarded_for)
                .map_err(|err| {
                    Error::because(
                        ErrorType::InternalError,
                        "insert X-Forwarded-For header",
                        err,
                    )
                })?;
        }
        if let Some(server_addr) = session.server_addr()
            && let Some(address) = server_addr.as_inet()
        {
            upstream_request
                .insert_header("X-Forwarded-Port", address.port().to_string())
                .map_err(|err| {
                    Error::because(
                        ErrorType::InternalError,
                        "insert X-Forwarded-Port header",
                        err,
                    )
                })?;
        }
        upstream_request
            .insert_header("X-Forwarded-Proto", ctx.downstream_scheme)
            .map_err(|err| {
                Error::because(
                    ErrorType::InternalError,
                    "insert X-Forwarded-Proto header",
                    err,
                )
            })?;
        let via = append_header_value(upstream_request, "Via", "1.1 ployz-gateway");
        upstream_request
            .insert_header("Via", via)
            .map_err(|err| Error::because(ErrorType::InternalError, "insert Via header", err))?;
        Ok(())
    }

    fn fail_to_connect(
        &self,
        _session: &mut Session,
        _peer: &HttpPeer,
        ctx: &mut Self::CTX,
        e: Box<Error>,
    ) -> Box<Error> {
        if !ctx.retry_allowed {
            return e;
        }
        let mut retry_error = e;
        retry_error.set_retry(true);
        retry_error
    }

    async fn logging(&self, session: &mut Session, e: Option<&Error>, ctx: &mut Self::CTX) {
        let request = session.req_header();
        let route_id = ctx.route_id.as_deref().unwrap_or("-");

        let state = self.snapshot.load();
        let backend_label = ctx
            .selected_addr
            .and_then(|addr| state.backend_lookup.get(&addr))
            .map(|bv| bv.instance_id.0.as_str())
            .unwrap_or("-");

        info!(
            method = %request.method,
            path = %request.uri.path(),
            route_id,
            backend = backend_label,
            failed = e.is_some(),
            "gateway request completed"
        );

        let response_code = session
            .response_written()
            .map_or(0, |resp| resp.status.as_u16());
        let started_at = ctx.started_at.unwrap_or_else(Instant::now);
        crate::metrics::observe_request(
            request.method.as_str(),
            response_code,
            ctx.matched,
            started_at.elapsed(),
        );
    }
}

fn is_retryable_method(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn append_header_value(upstream_request: &RequestHeader, name: &str, value: &str) -> String {
    let mut combined = upstream_request
        .headers
        .get_all(name)
        .iter()
        .filter_map(|existing| existing.to_str().ok())
        .filter(|existing| !existing.is_empty())
        .collect::<Vec<_>>()
        .join(", ");

    if !combined.is_empty() {
        combined.push_str(", ");
    }
    combined.push_str(value);
    combined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ctx_defaults_downstream_scheme_to_http() {
        let ctx = RequestCtx::default();
        assert_eq!(ctx.downstream_scheme, "http");
    }
}
