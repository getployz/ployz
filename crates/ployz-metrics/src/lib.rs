use std::io;
use std::net::SocketAddr;
use std::sync::OnceLock;

use prometheus::{Encoder, IntGaugeVec, Opts, TextEncoder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

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
    tokio::spawn(async move {
        loop {
            let accepted = listener.accept().await;
            let (stream, _) = match accepted {
                Ok(value) => value,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let _ = serve_connection(stream).await;
            });
        }
    });
    Ok(local_addr)
}

async fn serve_connection(mut stream: TcpStream) -> io::Result<()> {
    let mut buffer = [0_u8; 4096];
    let bytes_read = stream.read(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let Some(request_line) = request.lines().next() else {
        return write_response(&mut stream, 400, "Bad Request", b"bad request").await;
    };

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let status = match (method, path) {
        ("GET", "/") | ("GET", "/metrics") => {
            let body = gather_metrics()?;
            return write_ok_response(&mut stream, &body).await;
        }
        ("GET", _) => (404, "Not Found", b"not found".as_slice()),
        _ => (405, "Method Not Allowed", b"method not allowed".as_slice()),
    };
    write_response(&mut stream, status.0, status.1, status.2).await
}

async fn write_ok_response(stream: &mut TcpStream, body: &[u8]) -> io::Result<()> {
    let encoder = TextEncoder::new();
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        encoder.format_type(),
        body.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await
}

async fn write_response(
    stream: &mut TcpStream,
    status_code: u16,
    status_text: &str,
    body: &[u8],
) -> io::Result<()> {
    let headers = format!(
        "HTTP/1.1 {status_code} {status_text}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await
}
