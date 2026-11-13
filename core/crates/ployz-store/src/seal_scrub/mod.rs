//! The scrubs that keep no plain copy of a sealed value anywhere the Store can still
//! read one: in what a Branch was made with, in a pair's base and its arrivals, in
//! every Branch below a sealer, and in earlier saved versions. A write that seals a
//! value runs them; `review::sealed` keeps the ledger of what was ever sealed, which
//! they read.

pub(crate) mod backfill;
pub(crate) mod history;
pub(crate) mod inherit;
pub(crate) mod walk;
