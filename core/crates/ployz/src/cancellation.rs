//! Process-lifetime signal ownership for commands spanning synchronous and async work.

use tokio_util::sync::CancellationToken;

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

/// Ctrl-C for the rest of the process, usable from synchronous code before any
/// runtime polls. The listener is never torn down, so a late signal always has a
/// reader and every caller shares one token.
pub(crate) fn interrupted() -> std::io::Result<CancellationToken> {
    static TOKEN: std::sync::Mutex<Option<CancellationToken>> = std::sync::Mutex::new(None);
    let mut token = TOKEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(token) = token.as_ref() {
        return Ok(token.clone());
    }
    let mut signals = signal_hook::iterator::Signals::new([signal_hook::consts::SIGINT])?;
    let cancelled = CancellationToken::new();
    let cancel = cancelled.clone();
    std::thread::Builder::new()
        .name("ployz-interrupt".into())
        .spawn(move || {
            for _ in signals.forever() {
                cancel.cancel();
            }
        })?;
    Ok(token.insert(cancelled).clone())
}
