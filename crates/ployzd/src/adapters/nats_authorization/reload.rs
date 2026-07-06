use std::fmt;
use std::time::Duration;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsReloadFailure {
    /// The reload command ran and reported failure.
    CommandFailed { evidence: NatsReloadEvidence },
    /// The reload runner panicked before producing an outcome.
    RunnerPanicked { message: String },
    /// The reload runner did not finish within the bounded window.
    TimedOut { limit: Duration },
}

impl fmt::Display for NatsReloadFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CommandFailed { evidence } => write!(
                formatter,
                "nats-server reload failed: {} -> {}",
                evidence.command, evidence.output
            ),
            Self::RunnerPanicked { message } => {
                write!(formatter, "nats-server reload runner panicked: {message}")
            }
            Self::TimedOut { limit } => write!(
                formatter,
                "nats-server reload did not finish within {}s",
                limit.as_secs()
            ),
        }
    }
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

/// Production reload: `systemctl reload nats-server`.
#[derive(Debug, Clone, Copy)]
pub struct SystemctlNatsReloadRunner;

impl NatsReloadRunner for SystemctlNatsReloadRunner {
    fn reload(&self) -> NatsReloadOutcome {
        run_reload_command("systemctl", &["reload", "nats-server"])
    }
}

/// Reload by signalling a known server pid directly. Used where systemd is
/// absent (fixtures, containers); command evidence is still recorded.
#[derive(Debug, Clone)]
pub struct SignalNatsReloadRunner {
    pid: u32,
}

impl SignalNatsReloadRunner {
    #[must_use]
    pub const fn new(pid: u32) -> Self {
        Self { pid }
    }
}

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
    match std::process::Command::new(program).args(args).output() {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            let evidence = NatsReloadEvidence {
                command,
                output: format!("status={}: {}", output.status, text.trim()),
            };
            if output.status.success() {
                NatsReloadOutcome::Reloaded(evidence)
            } else {
                NatsReloadOutcome::Failed(evidence)
            }
        }
        Err(error) => NatsReloadOutcome::Failed(NatsReloadEvidence {
            command,
            output: format!("spawn failed: {error}"),
        }),
    }
}
