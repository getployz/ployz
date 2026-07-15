use std::fs::File;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

pub const RELOAD_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// What happened when the server was asked to reload its config. Always
/// carries the command evidence; failures are retained per operation rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsReloadOutcome {
    Reloaded(NatsReloadEvidence),
    Failed(NatsReloadEvidence),
}

/// How the reload step failed. Each case carries only the evidence that
/// actually exists: command output, a panic message, or the timeout bound.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NatsReloadFailure {
    /// The reload command ran and reported failure.
    #[error("nats-server reload failed: {} -> {}", evidence.command, evidence.output)]
    CommandFailed { evidence: NatsReloadEvidence },
    /// The reload runner panicked before producing an outcome.
    #[error("nats-server reload runner panicked: {message}")]
    RunnerPanicked { message: String },
    /// The reload runner did not finish within the bounded window.
    #[error("nats-server reload did not finish within {}s", limit.as_secs())]
    TimedOut { limit: Duration },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsReloadEvidence {
    pub command: String,
    pub output: String,
}

/// The reload seam: the real implementation signals the supervised
/// `nats-server`; tests record calls.
pub trait NatsReloadRunner: Send + Sync + 'static {
    fn reload(&self) -> NatsReloadOutcome;
}

/// Production reload through the host's active service supervisor.
#[derive(Debug, Clone)]
pub struct HostNatsReloadRunner {
    openrc_runtime_dir: PathBuf,
}

impl Default for HostNatsReloadRunner {
    fn default() -> Self {
        Self {
            openrc_runtime_dir: PathBuf::from("/run/openrc"),
        }
    }
}

impl NatsReloadRunner for HostNatsReloadRunner {
    fn reload(&self) -> NatsReloadOutcome {
        let (program, args) = reload_command(detect_supervisor(&self.openrc_runtime_dir));
        run_reload_command(program, args)
    }
}

/// Reload by signalling a known server pid directly. Used where systemd is
/// absent (fixtures, containers); command evidence is still recorded.
#[derive(Debug, Clone)]
#[cfg(test)]
pub struct SignalNatsReloadRunner {
    pid: u32,
}

#[cfg(test)]
impl SignalNatsReloadRunner {
    #[must_use]
    pub const fn new(pid: u32) -> Self {
        Self { pid }
    }
}

#[cfg(test)]
impl NatsReloadRunner for SignalNatsReloadRunner {
    fn reload(&self) -> NatsReloadOutcome {
        let pid = self.pid.to_string();
        run_reload_command("kill", &["-HUP", &pid])
    }
}

fn run_reload_command(program: &str, args: &[&str]) -> NatsReloadOutcome {
    let command = std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    let mut capture = match tempfile::tempfile() {
        Ok(capture) => capture,
        Err(error) => {
            return NatsReloadOutcome::Failed(NatsReloadEvidence {
                command,
                output: format!("capture setup failed: {error}"),
            });
        }
    };
    let stdout = match capture.try_clone() {
        Ok(stdout) => stdout,
        Err(error) => {
            return NatsReloadOutcome::Failed(NatsReloadEvidence {
                command,
                output: format!("capture setup failed: {error}"),
            });
        }
    };
    let stderr = match capture.try_clone() {
        Ok(stderr) => stderr,
        Err(error) => {
            return NatsReloadOutcome::Failed(NatsReloadEvidence {
                command,
                output: format!("capture setup failed: {error}"),
            });
        }
    };
    match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
    {
        Ok(mut child) => match child.wait_timeout(RELOAD_COMMAND_TIMEOUT) {
            Ok(Some(status)) => {
                let evidence = NatsReloadEvidence {
                    command,
                    output: format!("status={status}: {}", captured_output(&mut capture)),
                };
                if status.success() {
                    NatsReloadOutcome::Reloaded(evidence)
                } else {
                    NatsReloadOutcome::Failed(evidence)
                }
            }
            Ok(None) => {
                let kill = child.kill();
                let reap = child.wait();
                NatsReloadOutcome::Failed(NatsReloadEvidence {
                    command,
                    output: format!(
                        "timed out after {}s; kill={kill:?}; reap={reap:?}; output={}",
                        RELOAD_COMMAND_TIMEOUT.as_secs(),
                        captured_output(&mut capture)
                    ),
                })
            }
            Err(error) => NatsReloadOutcome::Failed(NatsReloadEvidence {
                command,
                output: format!("wait failed: {error}"),
            }),
        },
        Err(error) => NatsReloadOutcome::Failed(NatsReloadEvidence {
            command,
            output: format!("spawn failed: {error}"),
        }),
    }
}

fn captured_output(capture: &mut File) -> String {
    if let Err(error) = capture.rewind() {
        return format!("capture read failed: {error}");
    }
    let mut bytes = Vec::new();
    if let Err(error) = capture.take(4096).read_to_end(&mut bytes) {
        return format!("capture read failed: {error}");
    }
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostSupervisor {
    Systemd,
    OpenRc,
}

fn detect_supervisor(openrc_runtime_dir: &Path) -> HostSupervisor {
    if openrc_runtime_dir.join("softlevel").is_file() {
        HostSupervisor::OpenRc
    } else {
        HostSupervisor::Systemd
    }
}

fn reload_command(supervisor: HostSupervisor) -> (&'static str, &'static [&'static str]) {
    match supervisor {
        HostSupervisor::Systemd => ("systemctl", &["reload", "nats-server"]),
        HostSupervisor::OpenRc => ("rc-service", &["nats-server", "reload"]),
    }
}

#[cfg(test)]
mod tests {
    use super::{HostSupervisor, NatsReloadOutcome, detect_supervisor, reload_command};

    #[test]
    fn active_openrc_runtime_selects_rc_service_reload() {
        let runtime = tempfile::tempdir().expect("temporary OpenRC runtime");
        std::fs::write(runtime.path().join("softlevel"), "default\n")
            .expect("OpenRC runtime marker");

        let supervisor = detect_supervisor(runtime.path());

        assert_eq!(supervisor, HostSupervisor::OpenRc);
        assert_eq!(
            reload_command(supervisor),
            ("rc-service", &["nats-server", "reload"][..])
        );
    }

    #[test]
    fn absent_openrc_runtime_selects_systemd_reload() {
        let runtime = tempfile::tempdir().expect("temporary OpenRC directory");

        let supervisor = detect_supervisor(runtime.path());

        assert_eq!(supervisor, HostSupervisor::Systemd);
        assert_eq!(
            reload_command(supervisor),
            ("systemctl", &["reload", "nats-server"][..])
        );
    }

    #[test]
    fn failed_reload_retains_bounded_command_output() {
        let outcome = super::run_reload_command("sh", &["-c", "printf reload-failed >&2; exit 7"]);

        let NatsReloadOutcome::Failed(evidence) = outcome else {
            panic!("reload command should fail");
        };
        assert!(evidence.output.contains("status=exit status: 7"));
        assert!(evidence.output.contains("reload-failed"));
    }
}
