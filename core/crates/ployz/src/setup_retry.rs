//! Bounded retries for safe operations after setup has begun.

use std::{fmt::Display, time::Duration};

use tokio::time::{Instant, sleep, timeout_at};

use crate::failure::Failure;

pub(crate) const WAIT: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error<E> {
    #[error("{0}")]
    Permanent(E),
    #[error("{0}")]
    Exhausted(String),
}

impl<E: Display> From<Error<E>> for Failure {
    fn from(error: Error<E>) -> Self {
        Self::usage(error.to_string())
    }
}

/// Only pass reads or operations known to be safe to repeat. The deadline also
/// bounds any retries inside the operation; it must not wrap a whole setup flow.
pub(crate) async fn run<C, T, E: Display>(
    context: &mut C,
    operation: &str,
    wait: Duration,
    retryable: impl Fn(&E) -> bool,
    mut attempt: impl AsyncFnMut(&mut C) -> Result<T, E>,
) -> Result<T, Error<E>> {
    let deadline = Instant::now() + wait;
    let mut last = None;
    loop {
        match timeout_at(deadline, attempt(context)).await {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(error)) if !retryable(&error) => {
                return Err(Error::Permanent(error));
            }
            Ok(Err(error)) => {
                if last.is_none() {
                    eprintln!(
                        "{operation}: {error}; retrying for up to {}s. Check outbound firewall access if this connection is blocked.",
                        deadline.saturating_duration_since(Instant::now()).as_secs()
                    );
                }
                last = Some(error.to_string());
            }
            Err(_) => break,
        }
        if timeout_at(deadline, sleep(Duration::from_secs(1)))
            .await
            .is_err()
        {
            break;
        }
    }
    Err(Error::Exhausted(format!(
        "{operation} did not recover within {}s; last error: {}",
        wait.as_secs(),
        last.as_deref().unwrap_or("request timed out")
    )))
}

/// Reqwest categories also contain protocol and TLS failures. Retry only
/// timeouts and concrete connection/transfer failures in their source chain.
pub(crate) fn transient_http(error: &reqwest::Error) -> bool {
    use std::error::Error as _;
    use std::io::ErrorKind;
    if error.is_timeout() {
        return true;
    }
    let mut source = error.source();
    while let Some(cause) = source {
        if cause.downcast_ref::<std::io::Error>().is_some_and(|error| temporary_dns(error) || matches!(error.kind(),
            ErrorKind::ConnectionRefused | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
            | ErrorKind::NotConnected | ErrorKind::BrokenPipe | ErrorKind::UnexpectedEof
            | ErrorKind::TimedOut | ErrorKind::NetworkUnreachable | ErrorKind::HostUnreachable))
            // ponytail: Hyper hides IncompleteMessage; use its exact text until reqwest exposes an EOF classifier.
            || cause.to_string() == "connection closed before message completed"
        {
            return true;
        }
        source = cause.source();
    }
    false
}

/// Std discards EAI_AGAIN's code when wrapping getaddrinfo failures.
pub(crate) fn temporary_dns(error: &std::io::Error) -> bool {
    // ponytail: recognize glibc/BSD and musl English messages; use a typed resolver if localized errors need support.
    matches!(
        error.to_string().as_str(),
        "failed to lookup address information: Temporary failure in name resolution"
            | "failed to lookup address information: Try again"
    )
}

/// Include the transport cause, which reqwest's top-level Display omits.
pub(crate) fn detail(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        text.push_str(&format!(": {error}"));
        source = error.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn http_retry_recognizes_temporary_but_not_permanent_dns_failures() {
        struct Resolver(&'static str);
        impl reqwest::dns::Resolve for Resolver {
            fn resolve(&self, _: reqwest::dns::Name) -> reqwest::dns::Resolving {
                let message = self.0;
                Box::pin(async move { Err(std::io::Error::other(message).into()) })
            }
        }
        for (message, retry) in [
            (
                "failed to lookup address information: Temporary failure in name resolution",
                true,
            ),
            ("failed to lookup address information: Try again", true),
            (
                "failed to lookup address information: Name or service not known",
                false,
            ),
            (
                "failed to lookup address information: Non-recoverable failure in name resolution",
                false,
            ),
        ] {
            let http = reqwest::Client::builder()
                .no_proxy()
                .dns_resolver(std::sync::Arc::new(Resolver(message)))
                .build()
                .unwrap();
            let error = http
                .get("http://resolver.invalid")
                .send()
                .await
                .unwrap_err();
            assert_eq!(transient_http(&error), retry, "{}", detail(&error));
        }
    }

    #[tokio::test]
    async fn http_retry_accepts_dropped_transfers_but_rejects_malformed_protocol() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        for (reply, retry) in [
            ("", true),
            ("HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\nshort", true),
            ("NOT-HTTP\r\n\r\n", false),
            (
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nNOPE\r\n",
                false,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 2048];
                assert!(socket.read(&mut request).await.unwrap() > 0);
                socket.write_all(reply.as_bytes()).await.unwrap();
            });
            let http = reqwest::Client::builder().no_proxy().build().unwrap();
            let error = match http.get(format!("http://{address}")).send().await {
                Ok(response) => response.bytes().await.unwrap_err(),
                Err(error) => error,
            };
            assert_eq!(transient_http(&error), retry, "{}", detail(&error));
            server.await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retries_transient_failures_but_stops_on_permanent_errors_and_at_deadline() {
        let mut calls = 0;
        let result = run(
            &mut calls,
            "probe",
            WAIT,
            |_| true,
            async |calls| {
                *calls += 1;
                if *calls < 3 {
                    Err("connection refused")
                } else {
                    Ok(42)
                }
            },
        )
        .await
        .unwrap();
        assert_eq!((result, calls), (42, 3));

        calls = 0;
        let error = run(
            &mut calls,
            "probe",
            WAIT,
            |_| false,
            async |calls| {
                *calls += 1;
                Err::<(), _>("wrong identity")
            },
        )
        .await
        .unwrap_err();
        assert_eq!(calls, 1);
        assert!(error.to_string().contains("wrong identity"));

        let started = Instant::now();
        let error = run(
            &mut (),
            "probe",
            WAIT,
            |_| true,
            async |_| Err::<(), _>("connection refused"),
        )
        .await
        .unwrap_err();
        assert_eq!(Instant::now() - started, WAIT);
        assert!(error.to_string().contains("last error: connection refused"));

        let started = Instant::now();
        let error = run(
            &mut (),
            "probe",
            WAIT,
            |_| true,
            async |_| std::future::pending::<Result<(), &str>>().await,
        )
        .await
        .unwrap_err();
        assert_eq!(Instant::now() - started, WAIT);
        assert!(error.to_string().contains("request timed out"));
    }
}
