//! Keeps systemd's start timeout from killing a daemon start that is slow but alive.

use std::time::Duration;

use ployz_core::MACHINE_START_WAIT;
use sd_notify::NotifyState;
use tokio::{task::JoinHandle, time::Instant};

const SYSTEMD_START_TIMEOUT_EXTENSION: Duration = Duration::from_secs(30);
/// systemd counts each extension from when it reads it, so a renewal sent as the last one runs
/// out loses to a late wakeup under the IO load that makes a start slow.
const SYSTEMD_START_TIMEOUT_RENEWAL: Duration = Duration::from_secs(15);
const _: () = assert!(
    SYSTEMD_START_TIMEOUT_RENEWAL.as_secs() * 2 <= SYSTEMD_START_TIMEOUT_EXTENSION.as_secs()
);

#[must_use]
pub(crate) struct SystemdStartTimeoutExtend {
    task: JoinHandle<()>,
}

impl Drop for SystemdStartTimeoutExtend {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Keeps extending the start timeout while `what` runs, up to MACHINE_START_WAIT.
pub(crate) fn extend_systemd_start_timeout(what: &'static str) -> SystemdStartTimeoutExtend {
    let usec = u32::try_from(SYSTEMD_START_TIMEOUT_EXTENSION.as_micros())
        .expect("30s start-timeout extension fits u32 microseconds");
    let deadline = Instant::now() + MACHINE_START_WAIT;
    SystemdStartTimeoutExtend {
        task: tokio::spawn(async move {
            loop {
                if Instant::now() >= deadline {
                    eprintln!(
                        "stopped extending the systemd start timeout: {what} is taking too long"
                    );
                    return;
                }
                if let Err(error) = sd_notify::notify(&[NotifyState::ExtendTimeoutUsec(usec)]) {
                    eprintln!(
                        "failed to extend the systemd start timeout: {error}",
                        error = ployz_core::error_chain::inline(&error),
                    );
                    return;
                }
                tokio::time::sleep(SYSTEMD_START_TIMEOUT_RENEWAL).await;
            }
        }),
    }
}
