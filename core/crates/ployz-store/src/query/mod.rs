//! `read`'s queries. Each family lives in its own module and adds one [`Query`]
//! variant, one [`View`] variant, and one arm in [`run`].

mod environment;

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use environment::{EnvironmentQuery, EnvironmentView, SettingRow};

use crate::Actor;
use crate::storage::Tx;

/// One question about authored configuration, answered from one consistent state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "query", rename_all = "snake_case")]
#[ts(rename = "ConfigQuery")]
pub enum Query {
    Environment(EnvironmentQuery),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "view", rename_all = "snake_case")]
#[ts(rename = "ConfigView")]
pub enum View {
    Environment(EnvironmentView),
}

pub(crate) fn run(tx: &mut dyn Tx, who: &Actor, query: &Query) -> Result<View, RpcError> {
    match query {
        Query::Environment(query) => environment::run(tx, who, query).map(View::Environment),
    }
}
