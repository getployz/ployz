//! Who builds a Git Service: the Organization's Build Order, and each Service's
//! Preferred Builder, addressed as `SERVICE.preferredBuilder`. Both apply at once:
//! the next build reads them as it starts. A build walks its Builders in turn: the
//! Preferred Builder, then the Build Order without it.

use ployz_core::{MachineId, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::Actor;
use crate::error;
use crate::id::EnvironmentId;
use crate::settings::ServiceSetting;
use crate::storage::Tx;

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
    const ALL: [Self; 4] = [
        Self::ServersOnly,
        Self::GithubThenServers,
        Self::ServersThenGithub,
        Self::GithubOnly,
    ];

    /// An Organization that never chose one: GitHub first. GitHub is skipped at
    /// once for a repository without the build workflow, so until GitHub is set up
    /// this builds on the servers only.
    pub const AUTO: Self = Self::GithubThenServers;

    const fn as_str(self) -> &'static str {
        match self {
            Self::ServersOnly => "servers-only",
            Self::GithubThenServers => "github-then-servers",
            Self::ServersThenGithub => "servers-then-github",
            Self::GithubOnly => "github-only",
        }
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

/// The Organization's Build Order.
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
            &[organization.into(), order.as_str().into()],
        )?,
        None => tx.execute(
            "DELETE FROM config_build_order WHERE organization_id = ?1",
            &[organization.into()],
        )?,
    };
    build_order(tx, who)
}

fn chosen(tx: &mut dyn Tx, organization: &str) -> Result<Option<BuildOrder>, RpcError> {
    tx.query(
        "SELECT build_order FROM config_build_order WHERE organization_id = ?1",
        &[organization.into()],
    )?
    .first()
    .map(|row| {
        let text = row.text(0)?;
        BuildOrder::ALL
            .into_iter()
            .find(|order| order.as_str() == text)
            .ok_or_else(|| error::corrupt("Build Order"))
    })
    .transpose()
}

/// A Service's Preferred Builder: GitHub, or one Server. None is Auto.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Preferred {
    Github,
    Server(MachineId),
}

const SETTING: ServiceSetting = ServiceSetting::PreferredBuilder;

impl Preferred {
    fn parse(value: &Value) -> Result<Self, RpcError> {
        match value.as_str() {
            Some("github") => Ok(Self::Github),
            Some(text) => MachineId::parse(text.trim().to_owned())
                .map(Self::Server)
                .map_err(|_| SETTING.invalid("expected \"github\" or a Server's Machine ID")),
            None => Err(SETTING.invalid("expected \"github\" or a Server's Machine ID")),
        }
    }

    fn text(&self) -> String {
        match self {
            Self::Github => "github".to_owned(),
            Self::Server(machine) => machine.to_string(),
        }
    }
}

/// `service_id`'s Preferred Builder in `environment`, as `get` shows it.
pub(crate) fn preferred_value(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    service_id: &str,
) -> Result<Value, RpcError> {
    Ok(preferred(tx, environment, service_id)?
        .map_or(Value::Null, |preferred| json!(preferred.text())))
}

pub(crate) fn preferred(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    service_id: &str,
) -> Result<Option<Preferred>, RpcError> {
    tx.query(
        "SELECT builder FROM config_preferred_builder \
         WHERE environment_id = ?1 AND service_id = ?2",
        &[environment.as_str().into(), service_id.into()],
    )?
    .first()
    .map(|row| {
        Preferred::parse(&json!(row.text(0)?)).map_err(|_| error::corrupt("Preferred Builder"))
    })
    .transpose()
}

/// Set (`Some`) or unset `service_id`'s Preferred Builder; whether it changed.
pub(crate) fn set_preferred(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    service_id: &str,
    value: Option<&Value>,
) -> Result<bool, RpcError> {
    let new = value.map(Preferred::parse).transpose()?;
    if preferred(tx, environment, service_id)? == new {
        return Ok(false);
    }
    match new {
        Some(new) => tx.execute(
            "INSERT INTO config_preferred_builder \
             (environment_id, service_id, organization_id, builder) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (environment_id, service_id) DO UPDATE SET builder = excluded.builder",
            &[
                environment.as_str().into(),
                service_id.into(),
                who.organization.as_str().into(),
                new.text().as_str().into(),
            ],
        )?,
        None => tx.execute(
            "DELETE FROM config_preferred_builder WHERE environment_id = ?1 AND service_id = ?2",
            &[environment.as_str().into(), service_id.into()],
        )?,
    };
    Ok(true)
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
    let (first, machine) = match preferred(tx, environment, service_id)? {
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
