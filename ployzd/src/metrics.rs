//! Prometheus scrape endpoint for the daemon.

use std::io;

use axum::{
    Router, extract::State, http::header::CONTENT_TYPE, response::IntoResponse, routing::get,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Serve `GET /metrics` until `shutdown` is cancelled.
///
/// # Errors
///
/// If the HTTP server fails.
pub async fn serve(
    listener: TcpListener,
    version: &str,
    shutdown: CancellationToken,
) -> io::Result<()> {
    axum::serve(
        listener,
        Router::new()
            .route("/metrics", get(scrape))
            .with_state(version.to_owned()),
    )
    .with_graceful_shutdown(shutdown.cancelled_owned())
    .await
}

async fn scrape(State(version): State<String>) -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/plain; version=0.0.4")],
        format!("ployz_ployzd_build_info{{version=\"{version}\"}} 1\n"),
    )
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    use super::serve;

    async fn start() -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, "test", CancellationToken::new()));
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
}
