use std::{io, time::Duration};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;

pub async fn serve(
    listener: TcpListener,
    version: &str,
    shutdown: CancellationToken,
) -> io::Result<()> {
    loop {
        let (mut stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("metrics listener failed: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = shutdown.cancelled() => return Ok(()),
        };
        // ponytail: scrapes are serialized; spawn per connection if scrape concurrency matters.
        tokio::select! {
            result = respond(&mut stream, version) => {
                if let Err(error) = result {
                    eprintln!("metrics client {peer} failed: {error}");
                }
            }
            () = shutdown.cancelled() => return Ok(()),
        }
    }
}

async fn respond(stream: &mut tokio::net::TcpStream, version: &str) -> io::Result<()> {
    let mut request = [0; 1024];
    let read = stream.read(&mut request).await?;
    let (status, body) = if request
        .get(..read)
        .is_some_and(|request| request.starts_with(b"GET /metrics "))
    {
        (
            "200 OK",
            format!("ployz_ployzd_build_info{{version=\"{version}\"}} 1\n").into_bytes(),
        )
    } else {
        ("404 Not Found", b"not found\n".to_vec())
    };
    stream
        .write_all(
            format!(
                "HTTP/1.0 {status}\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .await?;
    stream.write_all(&body).await?;
    stream.shutdown().await
}
