//! Who builds a Git Service: the Organization's Build Order, and each Service's
//! Preferred Builder, addressed as `SERVICE.preferredBuilder`. Both apply at once:
//! the next build reads them as it starts. A build walks its Builders in turn: the
//! Preferred Builder, then the Build Order without it.

use ployz_core::{MachineId, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::error;
use crate::id::EnvironmentId;
use crate::policy::Preferred;
use crate::storage::{Tx, name_of};

/// Which Builders a Git build tries, in turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum BuildOrder {
    ServersOnly,
    GithubThenServers,
    ServersThenGithub,
    GithubOnly,
}

impl BuildOrder {
    /// An Organization that never chose one: GitHub first. GitHub is skipped at
    /// once for a repository without the build workflow, so until GitHub is set up
    /// this builds on the servers only.
    pub const AUTO: Self = Self::GithubThenServers;

    /// Every Build Order that is set by name; an Organization that never chose one
    /// has [`BuildOrder::AUTO`].
    pub const ALL: [Self; 4] = [
        Self::ServersOnly,
        Self::GithubThenServers,
        Self::ServersThenGithub,
        Self::GithubOnly,
    ];

    /// The name it is typed and stored by.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ServersOnly => "servers-only",
            Self::GithubThenServers => "github-then-servers",
            Self::ServersThenGithub => "servers-then-github",
            Self::GithubOnly => "github-only",
        }
    }

    /// The Build Order `name` names.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|order| order.as_str() == name)
    }

    const fn builders(self) -> &'static [Builder] {
        match self {
            Self::ServersOnly => &[Builder::Servers],
            Self::GithubThenServers => &[Builder::Github, Builder::Servers],
            Self::ServersThenGithub => &[Builder::Servers, Builder::Github],
            Self::GithubOnly => &[Builder::Github],
        }
    }
}

/// One Builder a Git build may run on.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Builder {
    /// GitHub Actions, in the repository's `ployz-build.yml` workflow.
    Github,
    /// The Organization's Servers.
    Servers,
}

/// Set the Organization's Build Order; none returns it to Auto.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SetBuildOrder {
    #[serde(default)]
    pub build_order: Option<BuildOrder>,
}

/// Ask for the Build Order of the caller's Organization, and the Builders it tries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct BuildOrderQuery {}

/// The Organization's Build Order, and the Builders it tries in turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct BuildOrderView {
    /// None while it is Auto.
    pub build_order: Option<BuildOrder>,
    pub builders: Vec<Builder>,
}

pub(crate) fn build_order(tx: &mut dyn Tx, who: &Actor) -> Result<BuildOrderView, RpcError> {
    let build_order = chosen(tx, who.organization.as_str())?;
    Ok(BuildOrderView {
        build_order,
        builders: build_order.unwrap_or(BuildOrder::AUTO).builders().to_vec(),
    })
}

pub(crate) fn set_build_order(
    tx: &mut dyn Tx,
    who: &Actor,
    set: &SetBuildOrder,
) -> Result<BuildOrderView, RpcError> {
    let organization = who.organization.as_str();
    match set.build_order {
        Some(order) => tx.execute(
            "INSERT INTO config_build_order (organization_id, build_order) VALUES (?1, ?2) \
             ON CONFLICT (organization_id) DO UPDATE SET build_order = excluded.build_order",
            &[organization.into(), name_of(order).as_str().into()],
        )?,
        None => tx.execute(
            "DELETE FROM config_build_order WHERE organization_id = ?1",
            &[organization.into()],
        )?,
    };
    build_order(tx, who)
}

/// The Builders the Organization's Build Order tries, in turn.
pub(crate) fn order(tx: &mut dyn Tx, organization: &str) -> Result<Vec<Builder>, RpcError> {
    Ok(chosen(tx, organization)?
        .unwrap_or(BuildOrder::AUTO)
        .builders()
        .to_vec())
}

fn chosen(tx: &mut dyn Tx, organization: &str) -> Result<Option<BuildOrder>, RpcError> {
    tx.query(
        "SELECT build_order FROM config_build_order WHERE organization_id = ?1",
        &[organization.into()],
    )?
    .first()
    .map(|row| row.variant(0, "Build Order"))
    .transpose()
}

/// The Builders a Git build of `service_id` tries, in turn, and the Server the
/// servers try first, if one is preferred.
pub(crate) fn walk(
    tx: &mut dyn Tx,
    organization: &str,
    environment: &EnvironmentId,
    service_id: &str,
) -> Result<(Vec<Builder>, Option<MachineId>), RpcError> {
    let order = chosen(tx, organization)?.unwrap_or(BuildOrder::AUTO);
    let preferred = crate::policy::load(tx, environment, service_id)?
        .preferred_builder
        .map(|text| Preferred::parse(&json!(text)).map_err(|_| error::corrupt("Preferred Builder")))
        .transpose()?;
    let (first, machine) = match preferred {
        None => (None, None),
        Some(Preferred::Github) => (Some(Builder::Github), None),
        Some(Preferred::Server(machine)) => (Some(Builder::Servers), Some(machine)),
    };
    let rest = order
        .builders()
        .iter()
        .copied()
        .filter(|builder| Some(*builder) != first);
    Ok((first.into_iter().chain(rest).collect(), machine))
}

#[cfg(test)]
mod tests {
    use super::BuildOrder;

    #[test]
    fn a_build_order_is_named_as_it_is_stored() {
        for order in BuildOrder::ALL {
            assert_eq!(
                serde_json::to_value(order).unwrap(),
                serde_json::json!(order.as_str())
            );
            assert_eq!(BuildOrder::named(order.as_str()), Some(order));
        }
        assert_eq!(BuildOrder::named("auto"), None);
        assert_eq!(BuildOrder::named("Github-Only"), None);
    }
}
