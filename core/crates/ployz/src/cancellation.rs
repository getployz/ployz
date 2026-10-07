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
pub(crate) fn on_ctrl_c() -> CancellationToken {
    let cancellation = CancellationToken::new();
    let Ok(mut interrupt) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
    else {
        return cancellation;
    };
    claim_graceful();
    let signal = cancellation.clone();
    tokio::spawn(async move {
        tokio::select! {
            () = signal.cancelled() => {}
            received = interrupt.recv() => if received.is_some() {
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
