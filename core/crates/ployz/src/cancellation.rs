//! Process-lifetime signal ownership for commands spanning synchronous and async work.

use std::{
    io::{self, Read},
    os::unix::{
        io::{AsRawFd, RawFd},
        net::UnixStream,
    },
};

use signal_hook::iterator::{
    backend::{OwningSignalIterator, PollResult, SignalDelivery},
    exfiltrator::SignalOnly,
};
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

#[derive(Debug)]
struct SignalWriter {
    writer: UnixStream,
    _receiver: UnixStream,
}

impl AsRawFd for SignalWriter {
    fn as_raw_fd(&self) -> RawFd {
        self.writer.as_raw_fd()
    }
}

impl CtrlC {
    /// Register before returning, including when the caller's Tokio runtime is idle.
    pub(crate) fn subscribe() -> std::io::Result<Self> {
        let (read, writer) = UnixStream::pair()?;
        let writer = SignalWriter {
            writer,
            _receiver: read.try_clone()?,
        };
        let signals =
            SignalDelivery::with_pipe(read, writer, SignalOnly, [signal_hook::consts::SIGINT])?;
        let handle = signals.handle();
        let token = CancellationToken::new();
        let cancelled = token.clone();
        let listener = std::thread::Builder::new()
            .name("ployz-interrupt".into())
            .spawn(move || listen(OwningSignalIterator::new(signals), &cancelled))?;
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

fn listen(
    mut signals: OwningSignalIterator<UnixStream, SignalOnly>,
    cancelled: &CancellationToken,
) {
    loop {
        match signals.poll_signal(&mut wait_for_byte) {
            PollResult::Signal(_) => cancelled.cancel(),
            PollResult::Closed => return,
            PollResult::Pending => {}
            PollResult::Err(error) => panic!("Unexpected error: {error}"),
        }
    }
}

fn wait_for_byte(read: &mut UnixStream) -> io::Result<bool> {
    loop {
        match read.read(&mut [0; 1]) {
            Ok(count) => return Ok(count != 0),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(all(test, unix))]
#[path = "cancellation/lifetime_tests.rs"]
mod lifetime_tests;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, Write},
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    #[test]
    fn scoped_listener_joins_with_zero_one_or_repeated_signals_and_tokio_subscribers() {
        for case in [
            "none",
            "early_drop",
            "single",
            "repeated",
            "tokio_active",
            "tokio_prior",
        ] {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "cancellation::tests::signal_listener_child",
                    "--nocapture",
                ])
                .env("PLOYZ_SIGNAL_TEST", case)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            let output = std::io::BufReader::new(child.stdout.take().unwrap());
            let (ready, readiness) = std::sync::mpsc::channel();
            let reader = std::thread::spawn(move || {
                for line in output.lines() {
                    if line.unwrap().trim() == "READY" {
                        let _ = ready.send(());
                    }
                }
            });
            if let Err(error) = readiness.recv_timeout(Duration::from_secs(5)) {
                let _ = child.kill();
                child.wait().unwrap();
                reader.join().unwrap();
                panic!("signal listener child did not become ready: {case}: {error}");
            }
            if !matches!(case, "none" | "early_drop") {
                for _ in 0..if case == "repeated" { 2 } else { 1 } {
                    assert!(
                        Command::new("kill")
                            .args(["-INT", &child.id().to_string()])
                            .status()
                            .unwrap()
                            .success()
                    );
                    std::thread::sleep(Duration::from_millis(30));
                }
            }
            child.stdin.take().unwrap().write_all(b"finish\n").unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while child.try_wait().unwrap().is_none() {
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    reader.join().unwrap();
                    panic!("listener did not join: {case}");
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let status = child.wait().unwrap();
            reader.join().unwrap();
            assert!(status.success(), "signal listener child {case}: {status}");
        }
    }

    #[test]
    fn signal_listener_child() {
        let Ok(case) = std::env::var("PLOYZ_SIGNAL_TEST") else {
            return;
        };
        sigpipe::reset();
        if case == "early_drop" {
            for _ in 0..16 {
                drop(CtrlC::subscribe().unwrap());
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let entered = runtime.enter();
        let mut tokio_signal = case.starts_with("tokio_").then(|| {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap()
        });
        if case == "tokio_prior" {
            tokio_signal.take();
        }
        drop(entered);
        let signal = CtrlC::subscribe().unwrap();
        writeln!(std::io::stdout(), "READY").unwrap();
        std::io::stdout().flush().unwrap();
        let mut done = String::new();
        std::io::stdin().read_line(&mut done).unwrap();
        if !matches!(case.as_str(), "none" | "early_drop") {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !signal.token().is_cancelled() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(signal.token().is_cancelled());
        }
        drop(signal);
        drop(tokio_signal);
        drop(runtime);
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
