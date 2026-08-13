use std::io;

use prometheus::{Encoder, IntGaugeVec, Opts, Registry, TextEncoder};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::watch,
};

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

pub async fn serve(
    listener: TcpListener,
    registry: Registry,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    loop {
        let (mut stream, _) = tokio::select! {
            accepted = listener.accept() => accepted?,
            changed = shutdown.changed() => {
                let _ = changed;
                return Ok(());
            }
        };
        // ponytail: scrapes are serialized; spawn per connection if scrape concurrency matters.
        let mut request = [0; 1024];
        let read = tokio::select! {
            read = stream.read(&mut request) => read?,
            changed = shutdown.changed() => {
                let _ = changed;
                return Ok(());
            }
        };
        let (status, body) = if request
            .get(..read)
            .is_some_and(|request| request.starts_with(b"GET /metrics "))
        {
            let mut body = Vec::new();
            TextEncoder::new()
                .encode(&registry.gather(), &mut body)
                .map_err(io::Error::other)?;
            ("200 OK", body)
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
        stream.shutdown().await?;
    }
}
