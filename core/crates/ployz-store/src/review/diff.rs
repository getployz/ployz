//! `diff`: an Environment's Change Set and the version to publish or discard it by.

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::review::{self, DiffView};
use crate::scope::{self, EnvironmentRef};
use crate::storage::Tx;
use crate::{Actor, Trusted};

/// Review an Environment's changes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DiffQuery {
    /// The Environment to review.
    #[serde(default)]
    pub environment: EnvironmentRef,
}

pub(crate) fn diff(
    tx: &mut dyn Tx,
    who: &Actor,
    trusted: &Trusted,
    query: &DiffQuery,
) -> Result<DiffView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let review = review::review(tx, &environment)?;
    let mut view = review.view;
    let rows = review::service_rows(&mut view.changes);
    crate::domain::name_generated(tx, &environment, &review.head, trusted, rows)?;
    view.hints = crate::conditional_sync::hints(tx, &environment)?;
    view.incoming = crate::branch::incoming(tx, &environment)?;
    view.follow_hints = crate::branch::hints(tx, &environment)?;
    Ok(view)
}
