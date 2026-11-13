//! The scrubs that keep no plain copy of a sealed value anywhere the Store can still
//! read one: in what a Branch was made with, in a pair's base and its arrivals, in
//! every Branch below a sealer, and in earlier saved versions. A write that seals a
//! value runs them; [`ledger`] keeps the record of what was ever sealed, which
//! they read.
//!
//! Every walk reads and writes a Branch's row, then its arrivals, then its bases,
//! never a base before an arrival: Discard and Sync lock the two tables in that
//! order, so a walk that took them the other way could deadlock against either.

pub(crate) mod backfill;
pub(crate) mod empty;
pub(crate) mod history;
pub(crate) mod inherit;
pub(crate) mod ledger;
pub(crate) mod walk;
pub(crate) mod write;

#[cfg(test)]
mod walk_tests;
