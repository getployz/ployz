//! Structured logging wiring for daemon role processes.
//!
//! Role processes run under a supervisor that captures stderr into the
//! journal, so log lines go to stderr: JSON when stderr is not a terminal,
//! a human-readable format when it is. The `PLOYZ_LOG` environment variable
//! carries `tracing_subscriber::EnvFilter` directives; the default level is
//! `info`. Log lines are evidence for diagnosing the daemon itself —
//! operation status and events remain the product-facing record, and
//! nothing may read cluster truth back out of a log line.

use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

/// Environment variable holding the `EnvFilter` directive string.
pub const LOG_FILTER_ENV: &str = "PLOYZ_LOG";

const DEFAULT_FILTER: &str = "info";

/// Installs the process-global subscriber for a daemon role process.
///
/// A caller that already installed a subscriber (an embedding test harness)
/// keeps its subscriber; this function never panics over registration.
pub fn init_daemon_logging() {
    let filter =
        EnvFilter::try_from_env(LOG_FILTER_ENV).unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    let registration = if std::io::stderr().is_terminal() {
        builder.try_init()
    } else {
        builder.json().try_init()
    };
    drop(registration);
}
