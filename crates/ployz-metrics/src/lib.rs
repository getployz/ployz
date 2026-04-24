use std::io;
use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::Duration;

use httparse::{Request, Status};
use prometheus::{Encoder, IntGaugeVec, Opts, TextEncoder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

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
    const MAX_REQUEST_BYTES: usize = 8192;
    const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(5);
    let mut request_bytes = Vec::with_capacity(1024);

    loop {
        let mut read_buffer = [0_u8; 1024];
        let read_result = timeout(HEADER_READ_TIMEOUT, stream.read(&mut read_buffer)).await;
        let bytes_read = match read_result {
            Ok(value) => value?,
            Err(_) => {
                return write_response(&mut stream, 408, "Request Timeout", b"request timeout")
                    .await;
            }
        };
        if bytes_read == 0 {
            break;
        }
        let Some(chunk) = read_buffer.get(..bytes_read) else {
            return Err(io::Error::other("socket read exceeded buffer"));
        };
        request_bytes.extend_from_slice(chunk);
        if request_bytes.len() >= MAX_REQUEST_BYTES {
            return write_response(
                &mut stream,
                431,
                "Request Header Fields Too Large",
                b"request too large",
            )
            .await;
        }

        let mut headers = [httparse::EMPTY_HEADER; 16];
        let mut request = Request::new(&mut headers);
        let parse_status = request.parse(&request_bytes);
        match parse_status {
            Ok(Status::Complete(_)) => {
                let Some(method) = request.method else {
                    return write_response(&mut stream, 400, "Bad Request", b"bad request").await;
                };
                let Some(path) = request.path else {
                    return write_response(&mut stream, 400, "Bad Request", b"bad request").await;
                };
                let status = match (method, path) {
                    ("GET", "/") | ("GET", "/metrics") => {
                        let body = gather_metrics()?;
                        return write_ok_response(&mut stream, &body).await;
                    }
                    ("GET", _) => (404, "Not Found", b"not found".as_slice()),
                    _ => (405, "Method Not Allowed", b"method not allowed".as_slice()),
                };
                return write_response(&mut stream, status.0, status.1, status.2).await;
            }
            Ok(Status::Partial) => continue,
            Err(_) => {
                return write_response(&mut stream, 400, "Bad Request", b"bad request").await;
            }
        }
    }

    write_response(&mut stream, 400, "Bad Request", b"bad request").await
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

#[cfg(test)]
mod tests {
    use super::serve_connection;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn send_request_in_chunks(chunks: &[&[u8]]) -> Vec<u8> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let addr = listener.local_addr().expect("listener should have address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept should succeed");
            serve_connection(stream)
                .await
                .expect("serve should succeed");
        });

        let mut client = TcpStream::connect(addr)
            .await
            .expect("client should connect");
        for chunk in chunks {
            client
                .write_all(chunk)
                .await
                .expect("chunk should write successfully");
        }
        client.shutdown().await.expect("shutdown should succeed");

        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .await
            .expect("response should read");
        server.await.expect("server task should finish");
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
