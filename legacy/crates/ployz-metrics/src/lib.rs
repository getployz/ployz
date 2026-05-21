use std::io;
use std::net::SocketAddr;
use std::sync::OnceLock;

use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use prometheus::{Encoder, IntGaugeVec, Opts, TextEncoder};
use tokio::net::TcpListener;

static BUILD_INFO: OnceLock<IntGaugeVec> = OnceLock::new();

#[must_use]
pub fn register_metric<T>(cell: &'static OnceLock<T>, init: impl FnOnce() -> T) -> &'static T {
    cell.get_or_init(init)
}

pub fn set_build_info(component: &'static str, version: &'static str) {
    let metric = BUILD_INFO.get_or_init(|| {
        let metric = IntGaugeVec::new(
            Opts::new(
                "ployz_build_info",
                "Build information for Ployz components.",
            ),
            &["component", "version"],
        )
        .expect("build info metric should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("build info metric should register");
        metric
    });
    metric.with_label_values(&[component, version]).set(1);
}

pub fn gather_metrics() -> io::Result<Vec<u8>> {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    encoder
        .encode(&metric_families, &mut buffer)
        .map_err(io::Error::other)?;
    Ok(buffer)
}

pub async fn spawn_metrics_listener(listen_addr: &str) -> io::Result<SocketAddr> {
    let listener = TcpListener::bind(listen_addr).await?;
    let local_addr = listener.local_addr()?;
    let app = Router::new()
        .route("/", get(metrics_response))
        .route("/metrics", get(metrics_response));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(local_addr)
}

async fn metrics_response() -> Response {
    match gather_metrics() {
        Ok(body) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, TextEncoder::new().format_type())],
            body,
        )
            .into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::spawn_metrics_listener;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    async fn send_request_in_chunks(chunks: &[&[u8]]) -> Vec<u8> {
        let addr = spawn_metrics_listener("127.0.0.1:0")
            .await
            .expect("metrics listener should bind");

        let mut client = TcpStream::connect(addr)
            .await
            .expect("client should connect");
        for chunk in chunks {
            client
                .write_all(chunk)
                .await
                .expect("chunk should write successfully");
        }

        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .await
            .expect("response should read");
        response
    }

    #[tokio::test]
    async fn metrics_route_handles_fragmented_request_line() {
        let response = send_request_in_chunks(&[
            b"GET /met",
            b"rics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        ])
        .await;

        let response_text = String::from_utf8_lossy(&response);
        assert!(
            response_text.starts_with("HTTP/1.1 200 OK"),
            "unexpected response: {response_text}"
        );
    }

    #[tokio::test]
    async fn metrics_route_handles_fragmented_headers() {
        let response = send_request_in_chunks(&[
            b"GET /metrics HTTP/1.1\r\nHost: local",
            b"host\r\nConnection: close",
            b"\r\n\r\n",
        ])
        .await;

        let response_text = String::from_utf8_lossy(&response);
        assert!(
            response_text.starts_with("HTTP/1.1 200 OK"),
            "unexpected response: {response_text}"
        );
    }
}
