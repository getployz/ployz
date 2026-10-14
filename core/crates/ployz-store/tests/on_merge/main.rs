#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! A PR Environment's On merge list through the Store's interface, on SQLite and on
//! Postgres (see `backend`): every change it made rides into its Destination, over
//! whatever production changed since, a removal too; Exclude keeps a row here, an
//! Edit gives a riding variable the value it lands with. The merge lands the list
//! in one Saved revision and deploys what it changed with the merge's push.

#[path = "../backend/mod.rs"]
mod backend;
mod merge;
mod node_removals;
mod review_findings;
mod support;
