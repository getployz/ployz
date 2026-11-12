//! The decisions a model check calls instead of re-stating.
//!
//! A decision a model checks is a free function or a small enum machine over plain facts.
//! Time arrives as a `SystemTime` or `Duration` parameter. There is no `tokio`, `std::fs`,
//! `Instant::now`, Docker or SQLite type here, and `clippy.toml` refuses the clock and
//! filesystem calls. Callers map their I/O types to the facts, so the daemon, the CLI and
//! the Store call the same function `ployz-verify` explores.
//!
//! The pure cores `ployz-core` already holds (`issuance_gate`, `plan`, `shrink_floor`,
//! `RestartPolicy`) stay there.

pub mod caddy_load;
pub mod issuance;
pub mod volume_records;
