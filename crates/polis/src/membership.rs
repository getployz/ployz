//! Substrate membership row primitives.
//!
//! These types describe durable rows and queries. They do not decide Ployz
//! product outcomes such as `Joined` or `AlreadyPresent`.

mod model;
mod schema;

pub use model::{
    IslandId, MachineRow, MembershipLifecycle, OverlayIp, RowEpoch, StoreMachineId,
    WireGuardPublicKey,
};
pub use schema::{
    MachineRowQuery, membership_replication_schema_sql, membership_schema_statements,
    upsert_machine_statement, verify_membership_replication_schema, verify_membership_schema,
};

#[cfg(test)]
mod tests;
