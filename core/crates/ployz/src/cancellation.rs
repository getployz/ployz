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

/// A synchronous signal subscription whose listener is stopped and joined on drop.
pub(crate) struct CtrlC {
    token: CancellationToken,
    signals: signal_hook::iterator::Handle,
    listener: Option<std::thread::JoinHandle<()>>,
}

impl CtrlC {
    /// Register before returning, including when the caller's Tokio runtime is idle.
    pub(crate) fn subscribe() -> std::io::Result<Self> {
        let mut signals = signal_hook::iterator::Signals::new([signal_hook::consts::SIGINT])?;
        let handle = signals.handle();
        let token = CancellationToken::new();
        let cancelled = token.clone();
        let listener = std::thread::Builder::new()
            .name("ployz-interrupt".into())
            .spawn(move || {
                if signals.forever().next().is_some() {
                    cancelled.cancel();
                }
            })?;
        Ok(Self {
            token,
            signals: handle,
            listener: Some(listener),
        })
    }

    pub(crate) fn token(&self) -> &CancellationToken {
        &self.token
    }
}

impl Drop for CtrlC {
    fn drop(&mut self) {
        self.signals.close();
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
    }
}
