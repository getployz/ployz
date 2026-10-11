//! The control plane: the only process that judges observations and writes to Postgres.

pub mod detect;
pub mod feed;
pub mod incidents;
pub mod pg;
pub mod results;
#[cfg(test)]
mod test_pg;
