//! Bounded retries for safe operations after setup has begun.

use std::time::Duration;

use ployz_core::RpcErrorCode;
use tokio::time::{Instant, sleep, timeout_at};

use crate::failure::Failure;

pub(crate) const WAIT: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error<E> {
    #[error(transparent)]
    Permanent(E),
    #[error("{operation} did not recover within {}s.", wait.as_secs())]
    Exhausted {
        operation: String,
        wait: Duration,
        #[source]
        last: Option<E>,
    },
}

impl<E: Into<Failure> + std::error::Error + Send + Sync + 'static> From<Error<E>> for Failure {
    fn from(error: Error<E>) -> Self {
        match error {
            Error::Permanent(error) => error.into(),
            exhausted @ Error::Exhausted { .. } => {
                Self::caused(RpcErrorCode::Unavailable, exhausted.to_string(), exhausted)
            }
        }
    }
}

/// What to print on the first failure when the caller anticipates the outage.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Expected(pub(crate) &'static str);

/// Only pass reads or operations known to be safe to repeat. The deadline also
/// bounds any retries inside the operation; it must not wrap a whole setup flow.
pub(crate) async fn run<C, T, E: std::error::Error + 'static>(
    context: &mut C,
    operation: &str,
    wait: Duration,
    retryable: impl Fn(&E) -> bool,
    attempt: impl AsyncFnMut(&mut C) -> Result<T, E>,
) -> Result<T, Error<E>> {
    run_expecting(context, operation, None, wait, retryable, attempt).await
}

/// [`run`], announcing an anticipated outage, such as a daemon restart, in
/// place of the connectivity warning.
pub(crate) async fn run_expecting<C, T, E: std::error::Error + 'static>(
    context: &mut C,
    operation: &str,
    expected: Option<Expected>,
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
                    match expected {
                        Some(Expected(notice)) => crate::ui::note(format_args!("{notice}")),
                        None => crate::ui::retrying(
                            operation,
                            &error,
                            deadline.saturating_duration_since(Instant::now()).as_secs(),
                        ),
                    }
                }
                last = Some(error);
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
    Err(Error::Exhausted {
        operation: operation.to_owned(),
        wait,
        last,
    })
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
            assert_eq!(
                transient_http(&error),
                retry,
                "{}",
                crate::ui::chain_text(&error)
            );
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
            assert_eq!(
                transient_http(&error),
                retry,
                "{}",
                crate::ui::chain_text(&error)
            );
            server.await.unwrap();
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("{0}")]
    struct Probe(&'static str);

    #[test]
    fn a_timeout_is_unavailable_and_a_refusal_keeps_its_own_code() {
        let timeout = Failure::from(Error::Exhausted {
            operation: "Probe".into(),
            wait: WAIT,
            last: Some(crate::connect::ConnectError::EntryNotReady),
        });
        assert_eq!(timeout.report().code, RpcErrorCode::Unavailable);
        assert_eq!(timeout.to_string(), "Probe did not recover within 60s.");
        assert_eq!(
            timeout.causes(),
            [crate::connect::ConnectError::EntryNotReady.to_string()]
        );
        let refused = Failure::from(Error::Permanent(
            crate::connect::ConnectError::ClientRefused,
        ));
        assert_eq!(refused.report().code, RpcErrorCode::Unauthenticated);
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
                    Err(Probe("connection refused"))
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
                Err::<(), _>(Probe("wrong identity"))
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
            async |_| Err::<(), _>(Probe("connection refused")),
        )
        .await
        .unwrap_err();
        assert_eq!(Instant::now() - started, WAIT);
        assert_eq!(error.to_string(), "probe did not recover within 60s.");
        assert_eq!(crate::ui::causes(&error), ["connection refused"]);

        let started = Instant::now();
        let error = run(
            &mut (),
            "probe",
            WAIT,
            |_| true,
            async |_| std::future::pending::<Result<(), Probe>>().await,
        )
        .await
        .unwrap_err();
        assert_eq!(Instant::now() - started, WAIT);
        assert!(crate::ui::causes(&error).is_empty());
    }
}
