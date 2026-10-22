//! Process-lifetime signal ownership for commands spanning synchronous and async work.

use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};

use tokio_util::sync::CancellationToken;

/// Whether SIGINT still ends the process the default way; cleared while a
/// prompt reads keys, and for good once a command listens for Ctrl-C itself.
static DIES: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static GRACEFUL: AtomicBool = AtomicBool::new(false);

/// Subscribe to Ctrl-C for async-only commands already running on Tokio.
/// The listener is registered before SIGINT stops ending the process, so no
/// Ctrl-C falls between the two; without one, SIGINT keeps its default action.
/// SIGTERM, which CI runners, `timeout`, systemd and container stops send,
/// cancels the same way.
pub(crate) fn on_ctrl_c() -> CancellationToken {
    use tokio::signal::unix::{SignalKind, signal};
    let cancellation = CancellationToken::new();
    let (Ok(mut interrupt), Ok(mut terminate)) = (
        signal(SignalKind::interrupt()),
        signal(SignalKind::terminate()),
    ) else {
        return cancellation;
    };
    claim_graceful();
    let signal = cancellation.clone();
    tokio::spawn(async move {
        tokio::select! {
            () = signal.cancelled() => {}
            received = interrupt.recv() => if received.is_some() {
                signal.cancel();
            },
            received = terminate.recv() => if received.is_some() {
                signal.cancel();
            }
        }
    });
    cancellation
}

/// Ctrl-C or SIGTERM for the rest of the process, usable from synchronous code
/// before any runtime polls. The listener is never torn down, so a late signal
/// always has a reader and every caller shares one token.
pub(crate) fn interrupted() -> std::io::Result<CancellationToken> {
    static TOKEN: std::sync::Mutex<Option<CancellationToken>> = std::sync::Mutex::new(None);
    let mut token = TOKEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(token) = token.as_ref() {
        return Ok(token.clone());
    }
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
    ])?;
    let cancelled = CancellationToken::new();
    let cancel = cancelled.clone();
    std::thread::Builder::new()
        .name("ployz-interrupt".into())
        .spawn(move || {
            for _ in signals.forever() {
                cancel.cancel();
            }
        })?;
    claim_graceful();
    Ok(token.insert(cancelled).clone())
}

/// From here on a listener answers SIGINT, so it must no longer end the
/// process the default way, even after a prompt closes.
fn claim_graceful() {
    GRACEFUL.store(true, Ordering::SeqCst);
    if let Some(dies) = DIES.get() {
        dies.store(false, Ordering::SeqCst);
    }
}

/// Run a prompt with Ctrl-C reaching it as a keypress. The prompt's terminal
/// library raises SIGINT for ^C; while `ask` runs that raise returns, so the
/// prompt can hand back `Interrupted` and put the cursor back. Outside a
/// prompt SIGINT keeps its default action.
pub(crate) fn while_prompting<T>(ask: impl FnOnce() -> T) -> std::io::Result<T> {
    let dies = match DIES.get() {
        Some(dies) => dies,
        None => {
            let dies = Arc::new(AtomicBool::new(!GRACEFUL.load(Ordering::SeqCst)));
            signal_hook::flag::register_conditional_default(
                signal_hook::consts::SIGINT,
                Arc::clone(&dies),
            )?;
            DIES.get_or_init(|| dies)
        }
    };
    dies.store(false, Ordering::SeqCst);
    let answer = ask();
    dies.store(!GRACEFUL.load(Ordering::SeqCst), Ordering::SeqCst);
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    // Each test raises a real signal at its own process; nextest runs every
    // test in a process of its own, so the shared token never leaks between them.

    #[test]
    fn sigterm_cancels_like_ctrl_c() {
        let token = super::interrupted().unwrap();
        signal_hook::low_level::raise(signal_hook::consts::SIGTERM).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !token.is_cancelled() {
            assert!(
                std::time::Instant::now() < deadline,
                "SIGTERM did not cancel"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[tokio::test]
    async fn sigterm_cancels_an_async_command_like_ctrl_c() {
        let token = super::on_ctrl_c();
        signal_hook::low_level::raise(signal_hook::consts::SIGTERM).unwrap();
        tokio::time::timeout(Duration::from_secs(5), token.cancelled())
            .await
            .expect("SIGTERM did not cancel");
    }
}
