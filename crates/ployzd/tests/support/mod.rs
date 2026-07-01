#![allow(dead_code)]

pub mod control;
pub mod machine_runtime;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub struct TestHttpUpstream {
    addr: std::net::SocketAddr,
    request: tokio::sync::oneshot::Receiver<String>,
}

impl TestHttpUpstream {
    pub async fn start(response_body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind upstream");
        let addr = listener.local_addr().expect("upstream local addr");
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("accept upstream");
                let mut request = Vec::new();
                if !read_until_http_head(&mut stream, &mut request).await {
                    continue;
                }
                let _ = request_tx.send(String::from_utf8(request).expect("request is utf8"));
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write upstream response");
                return;
            }
        });

        Self {
            addr,
            request: request_rx,
        }
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub async fn request(self) -> String {
        self.request.await.expect("upstream request received")
    }
}

async fn read_until_http_head(stream: &mut tokio::net::TcpStream, request: &mut Vec<u8>) -> bool {
    let mut chunk = [0; 128];
    loop {
        let read = stream.read(&mut chunk).await.expect("read upstream");
        if read == 0 {
            return false;
        }
        if let Some(bytes) = chunk.get(..read) {
            request.extend_from_slice(bytes);
        }
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return true;
        }
    }
}
