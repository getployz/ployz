use std::error::Error;
use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub struct TestUpstream {
    addr: SocketAddr,
    requests: tokio::sync::mpsc::Receiver<String>,
    expected_requests: usize,
}

impl TestUpstream {
    pub async fn start() -> Self {
        Self::start_with_expected_requests(1).await
    }

    pub async fn start_with_expected_requests(expected_requests: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind upstream");
        let addr = listener.local_addr().expect("upstream local addr");
        let (request_tx, request_rx) = tokio::sync::mpsc::channel(expected_requests);
        tokio::spawn(async move {
            let mut received_requests = 0;
            while received_requests < expected_requests {
                let (mut stream, _) = listener.accept().await.expect("accept upstream");
                let mut request = Vec::new();
                if !read_until_http_head(&mut stream, &mut request).await {
                    continue;
                }
                let _ = request_tx
                    .send(String::from_utf8(request).expect("request is utf8"))
                    .await;
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke")
                    .await
                    .expect("write upstream response");
                received_requests += 1;
            }
        });

        Self {
            addr,
            requests: request_rx,
            expected_requests,
        }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub async fn request(self) -> String {
        let [request] = self.requests().await.try_into().unwrap_or_else(|_| {
            panic!("expected one upstream request");
        });
        request
    }

    pub async fn requests(mut self) -> Vec<String> {
        let mut requests = Vec::with_capacity(self.expected_requests);
        for _ in 0..self.expected_requests {
            requests.push(
                self.requests
                    .recv()
                    .await
                    .expect("upstream receives request"),
            );
        }
        requests
    }
}

pub async fn http_get_with_host(
    addr: SocketAddr,
    host: &str,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let mut client = TcpStream::connect(addr).await?;
    client
        .write_all(
            format!("GET /smoke HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await?;
    let mut response = String::new();
    client.read_to_string(&mut response).await?;
    Ok(response)
}

pub async fn free_loopback_port() -> Result<u16, std::io::Error> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

async fn read_until_http_head(stream: &mut TcpStream, request: &mut Vec<u8>) -> bool {
    let mut chunk = [0; 1024];
    loop {
        let read = stream
            .read(&mut chunk)
            .await
            .expect("read upstream request");
        if read == 0 {
            return false;
        }
        request.extend_from_slice(
            chunk
                .get(..read)
                .expect("read byte count is within buffer length"),
        );
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return true;
        }
    }
}
