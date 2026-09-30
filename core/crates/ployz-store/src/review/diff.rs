//! `diff`: an Environment's Change Set and the version to publish or discard it by.

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::Actor;
use crate::review::{self, DiffView};
use crate::scope::{self, EnvironmentRef};
use crate::storage::Tx;

/// Review an Environment's changes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DiffQuery {
    /// The Environment to review.
    #[serde(default)]
    pub environment: EnvironmentRef,
}

pub(crate) fn diff(tx: &mut dyn Tx, who: &Actor, query: &DiffQuery) -> Result<DiffView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let mut view = review::review(tx, &environment)?.view;
    view.hints = crate::conditional_save::hints(tx, &environment.summary.id)?;
    Ok(view)
}
