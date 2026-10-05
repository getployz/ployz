//! HTTP health observations made directly against the inspected container.

use ployz_core::ContainerAddress;
use std::time::Duration;

/// What one probe saw.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Outcome {
    Up,
    /// The probe has no retries or start period, so before a first pass this is still startup.
    Down,
    Unavailable(&'static str),
}

/// One Machine-local probe per inspection. Redirects and ambient proxies cannot
/// move the request away from the inspected container's bridge address.
pub(super) async fn probe(
    address: Option<ContainerAddress>,
    check: &ployz_core::HttpHealthcheck,
) -> Outcome {
    let Some(address) = address else {
        return Outcome::Unavailable("HTTP probe address unavailable");
    };
    let client = match reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        // ponytail: 1s probes keep inspections responsive; add a separate probe timeout if slow endpoints need it.
        .timeout(Duration::from_secs(1))
        .build()
    {
        Ok(client) => client,
        Err(_) => return Outcome::Unavailable("HTTP probe unavailable"),
    };
    match client
        .get(format!("http://{}:{}{}", address.0, check.port, check.path))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => Outcome::Up,
        Ok(_) | Err(_) => Outcome::Down,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn http_probe_checks_status_without_following_redirects() {
        for (status, expected) in [
            (200, Outcome::Up),
            (503, Outcome::Down),
            (302, Outcome::Down),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let requests = Arc::new(AtomicUsize::new(0));
            let counted = requests.clone();
            let server = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = [0u8; 2048];
                    let n = socket.read(&mut request).await.unwrap();
                    assert!(
                        String::from_utf8_lossy(request.get(..n).unwrap())
                            .starts_with("GET /ready ")
                    );
                    counted.fetch_add(1, Ordering::SeqCst);
                    socket.write_all(format!("HTTP/1.1 {status} Test\r\nLocation: http://127.0.0.1:{port}/ready\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
                }
            });
            let check = ployz_core::HttpHealthcheck {
                path: "/ready".into(),
                port: port.try_into().unwrap(),
                timeout_seconds: 10,
            };
            assert_eq!(
                probe(Some(ContainerAddress("127.0.0.1".parse().unwrap())), &check).await,
                expected
            );
            assert_eq!(requests.load(Ordering::SeqCst), 1);
            server.abort();
            assert!(matches!(probe(None, &check).await, Outcome::Unavailable(_)));
            assert_eq!(
                super::super::create::docker_healthcheck(&ployz_core::HealthcheckSpec::Http(check))
                    .unwrap()
                    .test,
                Some(vec!["NONE".into()])
            );
        }
    }
}
