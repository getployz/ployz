//! Process-lifetime signal ownership for commands spanning synchronous and async work.

use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use tokio_util::sync::CancellationToken;

/// Whether SIGINT still ends the process the default way; cleared while a
/// prompt reads keys, and for good once a command listens for Ctrl-C itself.
static DIES: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static GRACEFUL: AtomicBool = AtomicBool::new(false);
/// How many [`SigtermCancels`] guards are alive; SIGTERM and SIGHUP cancel only
/// while one is.
static SIGTERM_CANCELS: AtomicUsize = AtomicUsize::new(0);

/// Subscribe to Ctrl-C for async-only commands already running on Tokio.
/// The listener is registered before SIGINT stops ending the process, so no
/// Ctrl-C falls between the two; without one, SIGINT keeps its default action.
/// SIGTERM keeps its default action here: some of these commands stop reading
/// the token partway, and a supervisor's SIGTERM must still end them.
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
/// reader and every caller shares one token. It also hears SIGTERM and SIGHUP, but
/// only a [`SigtermCancels`] turns them into a cancel; otherwise they still end the
/// process, since a caller may stop reading the token once its wait is over.
pub(crate) fn interrupted() -> std::io::Result<CancellationToken> {
    static TOKEN: std::sync::Mutex<Option<CancellationToken>> = std::sync::Mutex::new(None);
    let mut token = TOKEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(token) = token.as_ref() {
        return Ok(token.clone());
    }
    let mut heard = vec![signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM];
    if !hangup_ignored() {
        heard.push(signal_hook::consts::SIGHUP);
    }
    let mut signals = signal_hook::iterator::Signals::new(heard)?;
    let cancelled = CancellationToken::new();
    let cancel = cancelled.clone();
    std::thread::Builder::new()
        .name("ployz-interrupt".into())
        .spawn(move || {
            for signal in signals.forever() {
                if signal != signal_hook::consts::SIGINT
                    && SIGTERM_CANCELS.load(Ordering::SeqCst) == 0
                {
                    let _ = signal_hook::low_level::emulate_default_handler(signal);
                }
                cancel.cancel();
            }
        })?;
    claim_graceful();
    Ok(token.insert(cancelled).clone())
}

/// Whether SIGHUP must keep the disposition the process started with, as
/// `nohup` asks: a listener would replace its ignore. Linux says which signals
/// are ignored; elsewhere `nohup` moves stderr off the terminal, and a hangup
/// without one ends the process as before.
fn hangup_ignored() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find_map(|line| line.strip_prefix("SigIgn:"))
                    .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
            })
            .is_some_and(|mask| mask & (1 << (signal_hook::consts::SIGHUP - 1)) != 0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        use std::io::IsTerminal as _;
        !std::io::stderr().is_terminal()
    }
}

/// [`interrupted`], with SIGTERM and SIGHUP cancelling it like Ctrl-C until the
/// guard drops. Hold it only while the token is read, as a deploy does while it
/// follows and settles its owned work: CI runners, `timeout`, systemd and
/// container stops send SIGTERM, a closed terminal or dropped SSH session sends
/// SIGHUP, and the work must settle, not be orphaned.
pub(crate) fn interrupted_or_terminated() -> std::io::Result<(CancellationToken, SigtermCancels)> {
    let token = interrupted()?;
    SIGTERM_CANCELS.fetch_add(1, Ordering::SeqCst);
    Ok((token, SigtermCancels(())))
}

/// While alive, SIGTERM and SIGHUP cancel the [`interrupted`] token instead of
/// ending the process.
#[must_use = "SIGTERM and SIGHUP end the process again once this drops"]
pub(crate) struct SigtermCancels(());

impl Drop for SigtermCancels {
    fn drop(&mut self) {
        SIGTERM_CANCELS.fetch_sub(1, Ordering::SeqCst);
    }
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

    // These raise real signals at the test process; nextest runs every test in
    // a process of its own, so the shared token never leaks into another test.

    #[test]
    fn sigterm_cancels_like_ctrl_c_while_a_deploy_holds_it() {
        let (token, _sigterm) = super::interrupted_or_terminated().unwrap();
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

    #[test]
    fn sigterm_still_ends_the_process_outside_a_deploy() {
        use std::os::unix::process::ExitStatusExt as _;
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "cancellation::tests::listener_without_a_guard_raises_sigterm",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(signal_hook::consts::SIGTERM));
    }

    #[test]
    #[ignore = "child of sigterm_still_ends_the_process_outside_a_deploy"]
    fn listener_without_a_guard_raises_sigterm() {
        // A deploy took the guard and dropped it, so its token is no longer read.
        let (_token, sigterm) = super::interrupted_or_terminated().unwrap();
        drop(sigterm);
        signal_hook::low_level::raise(signal_hook::consts::SIGTERM).unwrap();
        std::thread::sleep(Duration::from_secs(5));
    }

    // A closed terminal or a dropped SSH session sends SIGHUP.
    #[test]
    fn sighup_cancels_like_ctrl_c_while_a_deploy_holds_it() {
        let (token, _sigterm) = super::interrupted_or_terminated().unwrap();
        signal_hook::low_level::raise(signal_hook::consts::SIGHUP).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !token.is_cancelled() {
            assert!(
                std::time::Instant::now() < deadline,
                "SIGHUP did not cancel"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn sighup_still_ends_the_process_outside_a_deploy() {
        use std::os::unix::process::ExitStatusExt as _;
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "cancellation::tests::listener_without_a_guard_raises_sighup",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(signal_hook::consts::SIGHUP));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_deploy_under_nohup_keeps_running_through_a_hangup() {
        let status = std::process::Command::new("nohup")
            .arg(std::env::current_exe().unwrap())
            .args([
                "cancellation::tests::a_hangup_under_nohup_cancels_nothing",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .current_dir(std::env::temp_dir())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "{status:?}");
    }

    #[test]
    #[ignore = "child of a_deploy_under_nohup_keeps_running_through_a_hangup"]
    fn a_hangup_under_nohup_cancels_nothing() {
        let (token, _sigterm) = super::interrupted_or_terminated().unwrap();
        signal_hook::low_level::raise(signal_hook::consts::SIGHUP).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(!token.is_cancelled(), "nohup asked to ignore SIGHUP");
    }

    #[test]
    #[ignore = "child of sighup_still_ends_the_process_outside_a_deploy"]
    fn listener_without_a_guard_raises_sighup() {
        let (_token, sigterm) = super::interrupted_or_terminated().unwrap();
        drop(sigterm);
        signal_hook::low_level::raise(signal_hook::consts::SIGHUP).unwrap();
        std::thread::sleep(Duration::from_secs(5));
    }
}
