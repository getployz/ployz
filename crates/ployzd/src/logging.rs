//! Structured logging wiring for daemon role processes.
//!
//! Role processes run under a supervisor that captures stderr into the
//! journal, so log lines go to stderr: JSON when stderr is not a terminal,
//! a human-readable format when it is. The `PLOYZ_LOG` environment variable
//! carries `tracing_subscriber::EnvFilter` directives; the default level is
//! `info`. Log lines are evidence for diagnosing the daemon itself —
//! coarse operation rows remain the product-facing record, and
//! nothing may read cluster truth back out of a log line.

use std::io::IsTerminal;

use tracing_subscriber::filter::{FilterExt, filter_fn};
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, Layer};

/// Environment variable holding the `EnvFilter` directive string.
pub const LOG_FILTER_ENV: &str = "PLOYZ_LOG";

const DEFAULT_FILTER: &str = "info";

fn configured_filter() -> EnvFilter {
    EnvFilter::try_from_env(LOG_FILTER_ENV).unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER))
}

fn secret_bearing_dependency_enabled(target: &str) -> bool {
    // Duroxide may include secret-bearing work items at any log level.
    target != "duroxide" && !target.starts_with("duroxide::")
}

/// Installs the process-global subscriber for a daemon role process.
///
/// A caller that already installed a subscriber (an embedding test harness)
/// keeps its subscriber; this function never panics over registration.
pub fn init_daemon_logging() {
    let registration = if std::io::stderr().is_terminal() {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_filter(configured_filter().and(filter_fn(|metadata| {
                        secret_bearing_dependency_enabled(metadata.target())
                    }))),
            )
            .try_init()
    } else {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(std::io::stderr)
                    .with_filter(configured_filter().and(filter_fn(|metadata| {
                        secret_bearing_dependency_enabled(metadata.target())
                    }))),
            )
            .try_init()
    };
    drop(registration);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duroxide_payload_logs_cannot_be_enabled() {
        assert!(!secret_bearing_dependency_enabled("duroxide"));
        assert!(!secret_bearing_dependency_enabled("duroxide::runtime"));
        assert!(secret_bearing_dependency_enabled("ployzd::roles::api"));
    }
}
