//! `read`'s queries. Each family lives in its own module and adds one [`Query`]
//! variant, one [`View`] variant, one arm in [`run`], and a typed method on
//! [`ConfigStore`](crate::ConfigStore) that calls the same function.

mod diff;
mod environment;

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};

pub use diff::DiffQuery;
pub(crate) use diff::diff;
pub(crate) use environment::environment;
pub use environment::{EnvironmentQuery, EnvironmentView, SettingRow};

use crate::Actor;
use crate::storage::Tx;

/// One question about authored configuration, answered from one consistent state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "query", rename_all = "snake_case")]
pub enum Query {
    /// An Environment's Settings.
    Environment(EnvironmentQuery),
    /// What Publish would save and Deploy would apply.
    Diff(DiffQuery),
}

/// A [`Query`]'s answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "view", rename_all = "snake_case")]
pub enum View {
    /// An Environment's Settings.
    Environment(EnvironmentView),
    /// An Environment's changes.
    Diff(crate::DiffView),
}

pub(crate) fn run(tx: &mut dyn Tx, who: &Actor, query: &Query) -> Result<View, RpcError> {
    match query {
        Query::Environment(query) => environment(tx, who, query).map(View::Environment),
        Query::Diff(query) => diff(tx, who, query).map(View::Diff),
    }
}
