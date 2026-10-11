//! The control plane: the only process that judges observations and writes to Postgres.

pub mod detect;
pub mod feed;
pub mod pg;
