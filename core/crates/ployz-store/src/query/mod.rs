//! `read`'s queries. Each family lives in its own module and adds one [`Query`]
//! variant, one [`View`] variant, and one arm in [`run`].

mod diff;
mod environment;

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};

pub use diff::DiffQuery;
pub use environment::{EnvironmentQuery, EnvironmentView, SettingRow};

use crate::Actor;
use crate::storage::Tx;

/// One question about authored configuration, answered from one consistent state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "query", rename_all = "snake_case")]
pub enum Query {
    Environment(EnvironmentQuery),
    Diff(DiffQuery),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "view", rename_all = "snake_case")]
pub enum View {
    Environment(EnvironmentView),
    Diff(crate::DiffView),
}

pub(crate) fn run(tx: &mut dyn Tx, who: &Actor, query: &Query) -> Result<View, RpcError> {
    match query {
        Query::Environment(query) => environment::run(tx, who, query).map(View::Environment),
        Query::Diff(query) => diff::run(tx, who, query).map(View::Diff),
    }
}
