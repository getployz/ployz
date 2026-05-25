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
    MachineRowQuery, membership_schema_statements, membership_startup_schema_sql,
    upsert_machine_statement, verify_membership_schema,
};

#[cfg(test)]
mod tests;
