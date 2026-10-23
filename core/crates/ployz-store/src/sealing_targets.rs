//! What a CLI that seals its own secrets needs to seal them: the Organization and
//! Environment a value binds to, and the authored Service it lands in, by ID. The
//! Store reads them; Cloud adds the Sealing Key.

use ployz_core::config::RowId;
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::branch::RowRef;
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::storage::Tx;
use crate::{Actor, branch};

/// Where the secrets of one write land, asked before the CLI seals them.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "target", rename_all = "snake_case", deny_unknown_fields)]
pub enum SealingTargetsQuery {
    /// An Edit's: the Environment's Services.
    Edit {
        #[serde(default)]
        environment: EnvironmentRef,
    },
    /// A Sync's: the receiver's Service each row lands in.
    Sync {
        #[serde(default)]
        from: EnvironmentRef,
        /// Where they land, in the same Project; omitted, the sender's Parent.
        #[serde(default)]
        #[ts(optional = nullable)]
        into: Option<EnvironmentRef>,
        /// Refused with `conflict` unless the Sync view is still at this version.
        version: String,
        /// The rows given values, each by RowId or by name in either side.
        rows: Vec<RowRef>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum SealingTargetsView {
    Edit {
        organization: String,
        environment: EnvironmentSummary,
        /// Its authored Services, each with the ID a sealed value binds to.
        services: Vec<SealingService>,
    },
    Sync {
        organization: String,
        from: EnvironmentSummary,
        into: EnvironmentSummary,
        version: String,
        /// Each row asked for, resolved as the Sync resolves it.
        values: Vec<SyncSealingTarget>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SealingService {
    pub name: ServiceName,
    /// The Service's own ID in this Environment, not its lineage.
    #[ts(type = "string")]
    pub service: Uuid,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SyncSealingTarget {
    /// The row as the caller named it.
    pub asked: RowRef,
    pub row: RowId,
    /// The receiver's Service the row lands in, by its own ID.
    #[ts(type = "string")]
    pub service: Uuid,
    /// The variable's name.
    pub key: String,
}

pub(crate) fn targets(
    tx: &mut dyn Tx,
    who: &Actor,
    q: &SealingTargetsQuery,
) -> Result<SealingTargetsView, RpcError> {
    let organization = who.organization.as_str().to_owned();
    match q {
        SealingTargetsQuery::Edit { environment } => {
            let environment = scope::environment(tx, who, environment)?;
            let services = environment
                .working
                .services
                .iter()
                .filter_map(|service| {
                    Some(SealingService {
                        name: ServiceName::parse(service.slug.as_str()).ok()?,
                        service: Uuid::parse_str(&service.id).ok()?,
                    })
                })
                .collect();
            Ok(SealingTargetsView::Edit {
                organization,
                environment: environment.summary,
                services,
            })
        }
        SealingTargetsQuery::Sync {
            from,
            into,
            version,
            rows,
        } => {
            let (from, into, values) =
                branch::sealing_targets(tx, who, (from, into.as_ref()), version, rows)?;
            Ok(SealingTargetsView::Sync {
                organization,
                from,
                into,
                version: version.clone(),
                values,
            })
        }
    }
}
