//! Process-lifetime signal ownership for commands spanning synchronous and async work.

use tokio_util::sync::CancellationToken;

use crate::failure::Failure as Error;

/// Cancel read-only work without abandoning mutation or cleanup futures.
///
/// # Errors
/// Reports cancellation or the read's own failure.
pub(crate) async fn read<T>(
    cancellation: &CancellationToken,
    work: impl std::future::Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(Error::usage("command cancelled")),
        result = work => result,
    }
}

/// Read a prompt response without holding command cancellation behind stdin.
///
/// # Errors
/// Reports cancellation, input errors, or failure to start the input thread.
pub(crate) async fn read_line(
    cancellation: &CancellationToken,
    mut input: impl std::io::BufRead + Send + 'static,
) -> Result<String, Error> {
    read(cancellation, async {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        // ponytail: a cancelled terminal read can linger until CLI exit; use
        // cancellable OS I/O if interactive commands become reusable.
        std::thread::Builder::new()
            .name("confirmation-input".into())
            .spawn(move || {
                let mut line = String::new();
                let result = input.read_line(&mut line).map(|_| line);
                let _ = sender.send(result);
            })?;
        Ok(receiver
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))??)
    })
    .await
}

/// Subscribe to Ctrl-C for async-only commands already running on Tokio.
pub(crate) fn on_ctrl_c() -> CancellationToken {
    let cancellation = CancellationToken::new();
    let signal = cancellation.clone();
    tokio::spawn(async move {
        tokio::select! {
            () = signal.cancelled() => {}
            result = tokio::signal::ctrl_c() => if result.is_ok() {
                signal.cancel();
            }
        }
    });
    cancellation
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_stops_a_prompt_with_no_input() {
        let (input, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let cancellation = CancellationToken::new();
        let response = read_line(&cancellation, std::io::BufReader::new(input));
        tokio::pin!(response);
        tokio::select! {
            result = &mut response => panic!("input unexpectedly completed: {result:?}"),
            () = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
        cancellation.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), response)
            .await
            .unwrap();
        assert!(result.unwrap_err().to_string().contains("cancelled"));
    }
}
