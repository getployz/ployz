//! Prometheus scrape endpoint for the daemon.

use std::io;

use axum::{
    Router, extract::State, http::header::CONTENT_TYPE, response::IntoResponse, routing::get,
};
use prometheus::{Encoder, IntGaugeVec, Opts, Registry, TextEncoder};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Prometheus registry with `ployzd_build_info` set.
///
/// # Errors
///
/// If the registry or metric cannot be created.
pub fn registry(version: &str) -> Result<Registry, prometheus::Error> {
    let registry = Registry::new_custom(Some("ployz".to_owned()), None)?;
    let build = IntGaugeVec::new(
        Opts::new("ployzd_build_info", "Build information."),
        &["version"],
    )?;
    build.with_label_values(&[version]).set(1);
    registry.register(Box::new(build))?;
    Ok(registry)
}

/// Serve `GET /metrics` until `shutdown` is cancelled.
///
/// # Errors
///
/// If the HTTP server fails.
pub async fn serve(
    listener: TcpListener,
    registry: Registry,
    shutdown: CancellationToken,
) -> io::Result<()> {
    axum::serve(
        listener,
        Router::new()
            .route("/metrics", get(scrape))
            .with_state(registry),
    )
    .with_graceful_shutdown(shutdown.cancelled_owned())
    .await
}

async fn scrape(State(registry): State<Registry>) -> impl IntoResponse {
    let mut body = Vec::new();
    TextEncoder::new()
        .encode(&registry.gather(), &mut body)
        .expect("prometheus text encoder");
    ([(CONTENT_TYPE, "text/plain; version=0.0.4")], body)
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    use super::{registry, serve};

    async fn start() -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(serve(
            listener,
            registry("test").unwrap(),
            CancellationToken::new(),
        ));
        address
    }

    #[tokio::test]
    async fn scrape_returns_build_info() {
        let address = start().await;
        let body = reqwest::get(format!("http://{address}/metrics?foo=1"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(body.contains("ployz_ployzd_build_info{version=\"test\"} 1"));
    }

    #[tokio::test]
    async fn unknown_path_is_not_found() {
        let address = start().await;
        let status = reqwest::get(format!("http://{address}/healthz"))
            .await
            .unwrap()
            .status();
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
