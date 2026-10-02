//! Branch Sync's engine. It compares two authored configurations over the base they
//! share, one row per lineage and place, and lands the picked rows. There is one exact
//! comparison, one policy table ([`Policy::verdict`]) and one writer ([`put`]), the
//! inverse of the projection a row's cells come from.

mod plan;
mod put;
mod row;

pub use plan::*;
pub use put::*;
pub use row::*;
