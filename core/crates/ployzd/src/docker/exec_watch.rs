//! An exec's command, followed from its start until evidence that it ended, for the work a
//! management key started ([`crate::machine::Bound`]).
//!
//! Docker answers an exec's attach before it marks the exec running, marks it running before
//! its process exists, and forgets every exec when it restarts with live-restore while the
//! command runs on. So the watch takes hold of the command's process, as a pidfd checked to
//! be in the exec's Container, as soon as Docker reports it, and from then on asks only the
//! kernel. The evidence an exec ended is an exit code Docker reported, the process seen
//! gone, or its Container confirmed stopped or removed. Not running with no exit code, a
//! 404, and a failed look are none.

use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use bollard::models::ExecInspectResponse;
use ployz_core::ContainerId;
use rustix::fd::OwnedFd;

use crate::machine::{Bound, Seen};

/// How long the command of an exec being stopped has to exit after SIGTERM before SIGKILL,
/// as `docker stop` gives a container.
const TERM_GRACE: Duration = Duration::from_secs(10);
/// How long a command has to be gone after SIGKILL before its stop is reported unconfirmed.
const KILL_GRACE: Duration = Duration::from_secs(5);
/// How long one Docker call of a look may take.
const DOCKER_ANSWER: Duration = Duration::from_secs(5);
/// How long a stop waits for Docker to start a command it was asked to, so it can stop it.
const START_WITHIN: Duration = Duration::from_secs(5);
/// How often a stop looks again for a command Docker has yet to start.
const START_POLL: Duration = Duration::from_millis(100);
/// How long an attached exec's client waits, once the command's process is gone, for
/// Docker to report its exit code.
const EXIT_CODE_WITHIN: Duration = Duration::from_secs(1);

/// One exec's command, which every clone follows together.
#[derive(Clone)]
pub(super) struct ExecWatch {
    docker: bollard::Docker,
    exec: String,
    container: ContainerId,
    /// The command's process once a look saw it in the exec's Container.
    process: Arc<OnceLock<OwnedFd>>,
}

/// What one look at the command found.
enum Looked {
    /// Its process, now held.
    Process,
    /// Evidence it ended, with its exit code when Docker reported one.
    Ended(Seen, Option<i64>),
    /// No evidence either way: what the look saw, and why.
    Unseen(Seen, ExecStopError),
}

/// How the command was seen to end.
#[derive(Clone, Copy, Debug)]
pub(super) struct Ended {
    pub(super) seen: Seen,
    pub(super) exit_code: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ExecStopError {
    #[error("Docker didn't say whether exec {exec}'s command still runs")]
    Unread {
        exec: String,
        #[source]
        cause: Unanswered,
    },
    #[error("Could not read exec {exec}'s process")]
    Process {
        exec: String,
        #[source]
        cause: std::io::Error,
    },
    #[error("Docker reports exec {exec}'s process at PID {pid}, which is not in its Container")]
    Elsewhere { exec: String, pid: i64 },
    #[error("Could not signal exec {exec}'s command")]
    Signal {
        exec: String,
        #[source]
        cause: std::io::Error,
    },
    #[error(
        "Exec {exec}'s command was still running {} s after SIGKILL",
        KILL_GRACE.as_secs()
    )]
    Unkilled { exec: String },
    #[error("Docker has not started exec {exec}'s command yet")]
    Starting { exec: String },
    #[error(
        "Docker no longer knows exec {exec}, as after it restarted, and its process was never seen"
    )]
    Forgotten { exec: String },
}

#[derive(Debug, thiserror::Error)]
pub(super) enum Unanswered {
    #[error("Docker did not answer within {} s", DOCKER_ANSWER.as_secs())]
    Late,
    #[error(transparent)]
    Docker(#[from] bollard::errors::Error),
}

impl ExecWatch {
    pub(super) fn new(docker: bollard::Docker, exec: String, container: ContainerId) -> Self {
        Self {
            docker,
            exec,
            container,
            process: Arc::default(),
        }
    }

    pub(super) fn id(&self) -> &str {
        &self.exec
    }

    /// How a key change names this exec, as `docker ps` shortens an ID.
    pub(super) fn name(&self) -> String {
        format!("exec {}", self.exec.get(..12).unwrap_or(&self.exec))
    }

    /// Completes once the command ended: through its process once held, before that by
    /// Docker's exit code or its Container gone, looking every `every` while Docker has yet
    /// to start it or doesn't answer. This watch outlives an `exec` call, bounded by the
    /// command its caller started, and acts only to stop it.
    ///
    /// # Errors
    /// Docker forgot the exec before its process was seen, so nothing can follow it.
    pub(super) async fn exited(&self, every: Duration) -> Result<Ended, ExecStopError> {
        loop {
            if let Some(process) = self.process.get() {
                return match exited_within(process, None).await {
                    Ok(_) => Ok(Ended {
                        seen: Seen::Reaped,
                        exit_code: None,
                    }),
                    Err(cause) => Err(self.unread_process(cause)),
                };
            }
            match self.look().await {
                Looked::Process => {}
                Looked::Ended(seen, exit_code) => return Ok(Ended { seen, exit_code }),
                Looked::Unseen(Seen::Forgotten, error) => return Err(error),
                // ponytail: one inspect per `every` until the process shows; use a Docker
                // exec wait API if one appears.
                Looked::Unseen(_, _) => tokio::time::sleep(every).await,
            }
        }
    }

    /// The exit code Docker reports once it handled the end of the command's process, or
    /// `None` when it doesn't within [`EXIT_CODE_WITHIN`].
    pub(super) async fn exit_code(&self) -> Option<i64> {
        tokio::time::timeout(EXIT_CODE_WITHIN, async {
            loop {
                match self.inspect().await {
                    Ok(Some(inspected)) if inspected.running != Some(true) => {
                        if let Some(code) = inspected.exit_code {
                            return Some(code);
                        }
                    }
                    Ok(None) => return None,
                    Ok(Some(_)) | Err(_) => {}
                }
                tokio::time::sleep(START_POLL).await;
            }
        })
        .await
        .ok()
        .flatten()
    }

    /// Stop the command: SIGTERM, then SIGKILL after [`TERM_GRACE`], and confirm through
    /// its process that it exited. A command Docker has yet to start is waited for up to
    /// [`START_WITHIN`], so this never returns while Docker may still start it unreported.
    /// Bounded by those, [`DOCKER_ANSWER`] per Docker call, and both graces. The pidfd keeps
    /// the signals on the command's process even once it exits and its PID is reused, and
    /// reads readable once it exited.
    ///
    /// # Errors
    /// Why the stop found no evidence the command ended.
    pub(super) async fn stop(&self) -> Result<Seen, ExecStopError> {
        let started = tokio::time::Instant::now() + START_WITHIN;
        let process = loop {
            if let Some(process) = self.process.get() {
                break process;
            }
            match self.look().await {
                Looked::Process => {}
                Looked::Ended(seen, _) => return Ok(seen),
                Looked::Unseen(Seen::Pending, error) => {
                    if tokio::time::Instant::now() >= started {
                        return Err(error);
                    }
                    tokio::time::sleep(START_POLL).await;
                }
                Looked::Unseen(_, error) => return Err(error),
            }
        };
        let failed = |cause: std::io::Error| ExecStopError::Signal {
            exec: self.exec.clone(),
            cause,
        };
        for (signal, grace) in [
            (rustix::process::Signal::TERM, TERM_GRACE),
            (rustix::process::Signal::KILL, KILL_GRACE),
        ] {
            if !send(process, signal).map_err(failed)? {
                return Ok(Seen::Reaped);
            }
            if exited_within(process, Some(grace)).await.map_err(failed)? {
                return Ok(Seen::Reaped);
            }
        }
        Err(ExecStopError::Unkilled {
            exec: self.exec.clone(),
        })
    }

    /// Look at the command once, taking hold of its process when Docker reports one in the
    /// exec's Container.
    async fn look(&self) -> Looked {
        if self.process.get().is_some() {
            return Looked::Process;
        }
        let inspected = match self.inspect().await {
            Ok(Some(inspected)) => inspected,
            // Docker forgets every exec when it restarts, and the command may run on.
            Ok(None) => return self.unless_container_gone(Seen::Forgotten).await,
            Err(error) => return Looked::Unseen(Seen::Unchecked, error),
        };
        match (inspected.running, inspected.exit_code, inspected.pid) {
            (Some(true), _, Some(pid)) if pid > 0 => self.hold(pid).await,
            (running, Some(code), _) if running != Some(true) => {
                Looked::Ended(Seen::ExitCode, Some(code))
            }
            // Docker answers an attach before it marks the exec running, and marks it
            // running before its process exists.
            _ => Looked::Unseen(
                Seen::Pending,
                ExecStopError::Starting {
                    exec: self.exec.clone(),
                },
            ),
        }
    }

    /// Hold the process Docker reports at `pid`, once checked to be in the exec's Container.
    /// Docker reports the PID until it handles the exit, and by then the PID may name
    /// another process. Either way, a process gone from that PID means the command's is.
    async fn hold(&self, pid: i64) -> Looked {
        let Some(raw) = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        else {
            return Looked::Unseen(Seen::Unchecked, self.elsewhere(pid));
        };
        let process = match rustix::process::pidfd_open(raw, rustix::process::PidfdFlags::empty()) {
            Ok(process) => process,
            Err(rustix::io::Errno::SRCH) => return Looked::Ended(Seen::Reaped, None),
            Err(error) => {
                return Looked::Unseen(Seen::Unchecked, self.unread_process(error.into()));
            }
        };
        let container = match tokio::time::timeout(
            DOCKER_ANSWER,
            super::inspect::inspect(&self.docker, self.container.as_str()),
        )
        .await
        {
            Ok(Ok(Some(container))) if container.has_no_process() => {
                return Looked::Ended(Seen::ContainerGone, None);
            }
            Ok(Ok(Some(container))) => container.pid(),
            Ok(Ok(None)) => return Looked::Ended(Seen::ContainerGone, None),
            Ok(Err(error)) => return Looked::Unseen(Seen::Unchecked, self.unread(error.into())),
            Err(_) => return Looked::Unseen(Seen::Unchecked, self.unread(Unanswered::Late)),
        };
        let Some(init) = container else {
            return Looked::Unseen(Seen::Unchecked, self.elsewhere(pid));
        };
        let cgroup = |pid: i64| std::fs::read_to_string(format!("/proc/{pid}/cgroup"));
        match (cgroup(pid), cgroup(init)) {
            (Ok(process), Ok(container)) if process == container => {}
            (Ok(_), Ok(_)) => return Looked::Unseen(Seen::Unchecked, self.elsewhere(pid)),
            (Err(cause), _) | (_, Err(cause)) => {
                return Looked::Unseen(Seen::Unchecked, self.unread_process(cause));
            }
        }
        match self.inspect().await {
            Ok(Some(again)) if again.running == Some(true) && again.pid == Some(pid) => {
                let _ = self.process.set(process);
                Looked::Process
            }
            Ok(Some(again)) if again.running != Some(true) && again.exit_code.is_some() => {
                Looked::Ended(Seen::ExitCode, again.exit_code)
            }
            Ok(Some(_)) => Looked::Unseen(Seen::Unchecked, self.elsewhere(pid)),
            Ok(None) => Looked::Unseen(
                Seen::Forgotten,
                ExecStopError::Forgotten {
                    exec: self.exec.clone(),
                },
            ),
            Err(error) => Looked::Unseen(Seen::Unchecked, error),
        }
    }

    /// `seen`, unless Docker confirms the exec's Container stopped or removed, which takes
    /// every process in it.
    async fn unless_container_gone(&self, seen: Seen) -> Looked {
        match tokio::time::timeout(
            DOCKER_ANSWER,
            super::inspect::inspect(&self.docker, self.container.as_str()),
        )
        .await
        {
            Ok(Ok(None)) => Looked::Ended(Seen::ContainerGone, None),
            Ok(Ok(Some(container))) if container.has_no_process() => {
                Looked::Ended(Seen::ContainerGone, None)
            }
            Ok(Ok(Some(_))) => Looked::Unseen(
                seen,
                ExecStopError::Forgotten {
                    exec: self.exec.clone(),
                },
            ),
            Ok(Err(error)) => Looked::Unseen(Seen::Unchecked, self.unread(error.into())),
            Err(_) => Looked::Unseen(Seen::Unchecked, self.unread(Unanswered::Late)),
        }
    }

    /// Docker's inspect of the exec, `None` when it no longer knows it.
    async fn inspect(&self) -> Result<Option<ExecInspectResponse>, ExecStopError> {
        match tokio::time::timeout(DOCKER_ANSWER, self.docker.inspect_exec(&self.exec)).await {
            Ok(Ok(inspected)) => Ok(Some(inspected)),
            Ok(Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            })) => Ok(None),
            Ok(Err(error)) => Err(self.unread(error.into())),
            Err(_) => Err(self.unread(Unanswered::Late)),
        }
    }

    fn unread(&self, cause: Unanswered) -> ExecStopError {
        ExecStopError::Unread {
            exec: self.exec.clone(),
            cause,
        }
    }

    fn unread_process(&self, cause: std::io::Error) -> ExecStopError {
        ExecStopError::Process {
            exec: self.exec.clone(),
            cause,
        }
    }

    fn elsewhere(&self, pid: i64) -> ExecStopError {
        ExecStopError::Elsewhere {
            exec: self.exec.clone(),
            pid,
        }
    }
}

/// Hand `bound` how the command ended, or why that's unknown, with a stop a later key
/// change tries again.
pub(super) fn settle(bound: Bound, watch: &ExecWatch, stopped: Result<Seen, ExecStopError>) {
    match stopped {
        Ok(seen) => bound.stopped(seen),
        Err(error) => {
            let watch = watch.clone();
            bound.unconfirmed(
                error,
                Some(Arc::new(move || {
                    let watch = watch.clone();
                    Box::pin(async move { watch.stop().await.map_err(Into::into) })
                })),
            );
        }
    }
}

/// Send `signal` to `process`: `false` when it had already exited.
fn send(process: &OwnedFd, signal: rustix::process::Signal) -> std::io::Result<bool> {
    match rustix::process::pidfd_send_signal(process, signal) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::SRCH) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Whether `process` exited within `grace`, or ever when `None`, read from its pidfd. Each
/// wait reads its own copy, since epoll registers a descriptor once: an attached exec's
/// watch still waits while a stop does.
async fn exited_within(process: &OwnedFd, grace: Option<Duration>) -> std::io::Result<bool> {
    let exited = tokio::io::unix::AsyncFd::with_interest(
        process.try_clone()?,
        tokio::io::Interest::READABLE,
    )?;
    match grace {
        None => exited.readable().await.map(|_| true),
        Some(grace) => Ok(tokio::time::timeout(grace, exited.readable()).await.is_ok()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An attached exec's watch waits on its command while the client goes away, and the
    /// exec then stops the command: the stop confirms the command ended.
    #[tokio::test]
    async fn a_stop_while_the_watch_waits_confirms_the_command_ended() {
        let mut command = std::process::Command::new("sleep")
            .arg("300")
            .spawn()
            .unwrap();
        let pid = rustix::process::Pid::from_child(&command);
        // Docker is never asked: the watch already holds the command's process.
        let docker = bollard::Docker::connect_with_http(
            "http://127.0.0.1:9",
            5,
            bollard::API_DEFAULT_VERSION,
        )
        .unwrap();
        let watch = ExecWatch::new(
            docker,
            "slow".into(),
            ContainerId::parse("a".repeat(64)).unwrap(),
        );
        let _ = watch
            .process
            .set(rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()).unwrap());
        let waiting = watch.exited(Duration::from_secs(1));
        tokio::pin!(waiting);
        assert!(futures_util::FutureExt::now_or_never(&mut waiting).is_none());

        let stopped = watch.stop().await;
        let _ = command.kill();
        let _ = command.wait();
        assert!(matches!(stopped, Ok(Seen::Reaped)), "{stopped:?}");
    }
}
