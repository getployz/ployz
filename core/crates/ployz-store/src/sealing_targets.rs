//! What a CLI that seals its own secrets needs to seal them: the Organization and
//! Environment a value binds to, and the authored Service it lands in, by ID. The
//! Store reads them; Cloud adds the Sealing Key.

use ployz_core::config::RowId;
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;
use uuid::Uuid;

use crate::branch::RowRef;
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::storage::Tx;
use crate::variables::VariableKey;
use crate::{Actor, branch, error};

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
    /// The Service's own ID in this Environment, not its lineage; `None` when it has
    /// none a sealed value binds to.
    #[ts(type = "string | null")]
    pub service: Option<Uuid>,
}

impl SealingService {
    /// The ID that a new secret `key` of Service `name` binds to, among the
    /// `services` of `environment`.
    ///
    /// # Errors
    /// `not_found` when no Service has that name, and `invalid_argument` when it has
    /// no ID, as the Store refuses them.
    pub fn bind(
        services: &[Self],
        environment: &EnvironmentSummary,
        name: &ServiceName,
        key: &str,
    ) -> Result<Uuid, RpcError> {
        let Some(service) = services.iter().find(|service| &service.name == name) else {
            let names = services.iter().map(|service| service.name.as_str());
            return Err(scope::no_service_among(name, &environment.name, names));
        };
        service.service.ok_or_else(|| {
            error::invalid(
                format!("{name}.env.{key}: {UNBOUND}"),
                json!({ "variable": key }),
            )
        })
    }
}

/// Why a Service can't take a sealed value: it has no ID one binds to.
pub(crate) const UNBOUND: &str = "this Service has no ID a sealed value binds to";

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
    /// The row as the Sync names it in a refusal.
    pub label: String,
}

impl SyncSealingTarget {
    /// `value`, once the Sync would seal it for this row.
    ///
    /// # Errors
    /// What the Sync refuses in a secret's plain text, as it refuses it.
    pub fn admit<'value>(&self, value: &'value str) -> Result<&'value str, RpcError> {
        let key = VariableKey::parse(&self.key)?;
        branch::admit_secret_text(&self.row, &self.label, &key, value)?;
        Ok(value)
    }
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
                .map(|service| {
                    Ok(SealingService {
                        name: ServiceName::parse(service.slug.as_str())
                            .map_err(|_| error::corrupt("Service name"))?,
                        service: Uuid::parse_str(&service.id).ok(),
                    })
                })
                .collect::<Result<_, RpcError>>()?;
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
