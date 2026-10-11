//! The control plane: the only process that judges observations and writes to Postgres.

pub mod detect;
pub(crate) mod engine;
pub mod feed;
pub mod incidents;
pub mod pg;
pub mod plane;
pub mod region_names;
pub mod results;
pub(crate) mod schedule;
#[cfg(test)]
pub(crate) mod test_pg;
