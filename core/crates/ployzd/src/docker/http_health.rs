//! HTTP health observations made directly against the inspected container.

use ployz_core::{ContainerAddress, HealthObservation, HttpCheckError, LastHealthCheck};
use std::time::Duration;

/// One Machine-local probe per inspection. Redirects and ambient proxies cannot
/// move the request away from the inspected container's bridge address.
pub(super) async fn probe(
    address: Option<ContainerAddress>,
    check: &ployz_core::HttpHealthcheck,
) -> (HealthObservation, Option<LastHealthCheck>) {
    let Some(address) = address else {
        return (
            HealthObservation::Unrecognized("HTTP probe address unavailable".into()),
            None,
        );
    };
    let client = match reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        // ponytail: 1s probes keep inspections responsive; add a separate probe timeout if slow endpoints need it.
        .timeout(Duration::from_secs(1))
        .build()
    {
        Ok(client) => client,
        Err(_) => {
            return (
                HealthObservation::Unrecognized("HTTP probe unavailable".into()),
                None,
            );
        }
    };
    match client
        .get(format!("http://{}:{}{}", address.0, check.port, check.path))
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            let health = if status.is_success() {
                HealthObservation::Healthy
            } else {
                HealthObservation::Starting
            };
            let status = status.as_u16();
            (health, Some(LastHealthCheck::HttpStatus { status }))
        }
        Err(error) => (
            HealthObservation::Starting,
            Some(LastHealthCheck::HttpUnreachable {
                error: unreachable_class(&error),
            }),
        ),
    }
}

fn unreachable_class(error: &reqwest::Error) -> HttpCheckError {
    if error.is_timeout() {
        return HttpCheckError::TimedOut;
    }
    let mut source: Option<&dyn std::error::Error> = Some(error);
    while let Some(cause) = source {
        if cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::ConnectionRefused)
        {
            return HttpCheckError::ConnectionRefused;
        }
        source = cause.source();
    }
    HttpCheckError::Other
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
            (200, HealthObservation::Healthy),
            (503, HealthObservation::Starting),
            (302, HealthObservation::Starting),
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
                (expected, Some(LastHealthCheck::HttpStatus { status }))
            );
            assert_eq!(requests.load(Ordering::SeqCst), 1);
            server.abort();
            assert!(matches!(
                probe(None, &check).await,
                (HealthObservation::Unrecognized(_), None)
            ));
            assert_eq!(
                super::super::create::docker_healthcheck(&ployz_core::HealthcheckSpec::Http(check))
                    .unwrap()
                    .test,
                Some(vec!["NONE".into()])
            );
        }
    }

    #[tokio::test]
    async fn http_probe_names_why_it_reached_no_response() {
        let loopback = || Some(ContainerAddress("127.0.0.1".parse().unwrap()));
        let check = |port: u16| ployz_core::HttpHealthcheck {
            path: "/ready".into(),
            port: port.try_into().unwrap(),
            timeout_seconds: 10,
        };

        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed_port = closed.local_addr().unwrap().port();
        drop(closed);
        assert_eq!(
            probe(loopback(), &check(closed_port)).await,
            (
                HealthObservation::Starting,
                Some(LastHealthCheck::HttpUnreachable {
                    error: HttpCheckError::ConnectionRefused,
                })
            )
        );

        let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let silent_port = silent.local_addr().unwrap().port();
        let held = tokio::spawn(async move {
            let (_socket, _) = silent.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        assert_eq!(
            probe(loopback(), &check(silent_port)).await,
            (
                HealthObservation::Starting,
                Some(LastHealthCheck::HttpUnreachable {
                    error: HttpCheckError::TimedOut,
                })
            )
        );
        held.abort();
    }
}
