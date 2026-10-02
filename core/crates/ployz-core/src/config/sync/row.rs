//! A row's address, what it holds, and each Environment's cells by row.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};

use crate::config::service_changes::{at, default_value};
use crate::config::{
    ConfigError, EncryptedSecretValue, SavedEnvironmentIntent as Intent, SavedServiceIntent,
    SavedVariableIntent, SavedVariableValue, SavedVolumeIntent, ServiceImageCredentials,
    ServiceSource,
};

/// One row's address: a lineage and a place in its node. The only row address anywhere;
/// it reads and writes as `<lineage>:<at>`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowId {
    pub(super) lineage: String,
    pub(super) at: At,
}

/// Where in its node a row is. Declaration order is row order, which is also landing
/// order within a node.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum At {
    /// The node itself.
    Node,
    /// A Volume's data, which never moves.
    Data,
    /// A Volume's name.
    Name,
    /// A Volume's storage.
    Storage,
    /// A Service setting.
    Setting(Setting),
    /// A mount on the Volume of this lineage.
    Mount(String),
    /// The variable of this key.
    Variable(String),
}

/// A Service setting a row can address. Declaration order is landing order: what a
/// source is comes before what it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[expect(missing_docs, reason = "each is the setting its path names")]
pub enum Setting {
    Repository,
    Image,
    RootDir,
    Branch,
    Credentials,
    PrivateDns,
    ManagedHostnames,
    Routes,
    PreDeployCommand,
    StartCommand,
    Healthcheck,
    RestartPolicy,
    MaxRetries,
    Replicas,
    CpuLimit,
    MemLimit,
    BuildMethod,
    DockerfilePath,
    BuildCommand,
}

impl Setting {
    pub(super) const ALL: [Self; 19] = [
        Self::Repository,
        Self::Image,
        Self::RootDir,
        Self::Branch,
        Self::Credentials,
        Self::PrivateDns,
        Self::ManagedHostnames,
        Self::Routes,
        Self::PreDeployCommand,
        Self::StartCommand,
        Self::Healthcheck,
        Self::RestartPolicy,
        Self::MaxRetries,
        Self::Replicas,
        Self::CpuLimit,
        Self::MemLimit,
        Self::BuildMethod,
        Self::DockerfilePath,
        Self::BuildCommand,
    ];

    /// Where it is in a Service's configuration, as the review names it.
    #[must_use]
    pub const fn path(self) -> &'static str {
        match self {
            Self::Repository => "source.repository",
            Self::Image => "source.image",
            Self::RootDir => "source.rootDir",
            Self::Branch => "source.branch",
            Self::Credentials => "source.credentials",
            Self::PrivateDns => "privateDns",
            Self::ManagedHostnames => "managedHostnames",
            Self::Routes => "routes",
            Self::PreDeployCommand => "preDeployCommand",
            Self::StartCommand => "startCommand",
            Self::Healthcheck => "healthcheck",
            Self::RestartPolicy => "restartPolicy",
            Self::MaxRetries => "maxRetries",
            Self::Replicas => "replicas",
            Self::CpuLimit => "cpuLimit",
            Self::MemLimit => "memLimit",
            Self::BuildMethod => "build.buildMethod",
            Self::DockerfilePath => "build.dockerfilePath",
            Self::BuildCommand => "build.command",
        }
    }

    fn of_path(path: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|setting| setting.path() == path)
    }

    /// Whether a new node arrives with it on its node row rather than as a row.
    pub(super) fn carried(self) -> bool {
        match self {
            Self::Repository
            | Self::Image
            | Self::RootDir
            | Self::Branch
            | Self::Credentials
            | Self::PrivateDns
            | Self::ManagedHostnames
            | Self::Routes => true,
            Self::PreDeployCommand
            | Self::StartCommand
            | Self::Healthcheck
            | Self::RestartPolicy
            | Self::MaxRetries
            | Self::Replicas
            | Self::CpuLimit
            | Self::MemLimit
            | Self::BuildMethod
            | Self::DockerfilePath
            | Self::BuildCommand => false,
        }
    }
}

impl At {
    /// Settings a new node arrives with on its node row rather than as rows of their own.
    pub(super) fn carried(&self) -> bool {
        match self {
            Self::Name | Self::Storage => true,
            Self::Setting(setting) => setting.carried(),
            Self::Node | Self::Data | Self::Mount(_) | Self::Variable(_) => false,
        }
    }

    /// Whether a new node can't arrive without the row here: it needs what it is
    /// carried with, but arrives without custom domains anyway.
    pub(super) fn needed(&self) -> bool {
        self.carried() && *self != Self::Setting(Setting::Routes)
    }
}

impl fmt::Display for At {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Node => f.write_str("node"),
            Self::Data => f.write_str("data"),
            Self::Name => f.write_str("name"),
            Self::Storage => f.write_str("storage"),
            Self::Setting(setting) => f.write_str(setting.path()),
            Self::Mount(volume) => write!(f, "mounts.{volume}"),
            Self::Variable(key) => write!(f, "variables.{key}"),
        }
    }
}

impl RowId {
    /// The lineage of the node the row is in.
    #[must_use]
    pub fn lineage(&self) -> &str {
        &self.lineage
    }

    /// Where in the node.
    #[must_use]
    pub fn at(&self) -> &At {
        &self.at
    }

    /// The row of the node itself.
    #[must_use]
    pub fn node(lineage: &str) -> Self {
        Self::of(lineage, At::Node)
    }

    /// The row at `at` in node `lineage`.
    #[must_use]
    pub fn of(lineage: &str, at: At) -> Self {
        Self {
            lineage: lineage.to_owned(),
            at,
        }
    }
}

impl fmt::Display for RowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.lineage, self.at)
    }
}

impl FromStr for RowId {
    type Err = ConfigError;

    fn from_str(row: &str) -> Result<Self, ConfigError> {
        let unknown = || ConfigError::at("row", "Unknown change");
        let (lineage, at) = row.split_once(':').ok_or_else(unknown)?;
        let at = match at {
            "node" => At::Node,
            "data" => At::Data,
            "name" => At::Name,
            "storage" => At::Storage,
            at => match (at.strip_prefix("mounts."), at.strip_prefix("variables.")) {
                (Some(volume), _) if !volume.is_empty() => At::Mount(volume.to_owned()),
                (_, Some(key)) if !key.is_empty() => At::Variable(key.to_owned()),
                _ => At::Setting(Setting::of_path(at).ok_or_else(unknown)?),
            },
        };
        if lineage.is_empty() {
            return Err(unknown());
        }
        Ok(Self {
            lineage: lineage.to_owned(),
            at,
        })
    }
}

impl Serialize for RowId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// Branded on the TypeScript side: a row is addressed only by the id a read gave.
impl ts_rs::TS for RowId {
    type WithoutGenerics = Self;
    type OptionInnerType = Self;

    fn name(_: &ts_rs::Config) -> String {
        "RowId".to_owned()
    }

    fn inline(_: &ts_rs::Config) -> String {
        "string & { readonly __brand: \"RowId\" }".to_owned()
    }

    fn decl(cfg: &ts_rs::Config) -> String {
        format!("type RowId = {};", Self::inline(cfg))
    }

    fn output_path() -> Option<std::path::PathBuf> {
        Some(std::path::PathBuf::from("RowId.ts"))
    }
}

impl<'de> Deserialize<'de> for RowId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// What one side holds at a row. A setting at its default is `Absent`, so unsetting is
/// a removal. A secret is there by fingerprint only: its value travels as a
/// [`SealedCell`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cell {
    /// Nothing there, or a setting at its default.
    Absent,
    /// A plain value.
    Value(Value),
    /// A secret with a value.
    Secret {
        /// Tells two values apart without showing either.
        fingerprint: String,
    },
    /// A secret still waiting for its value.
    SecretWithoutValue,
}

impl Cell {
    /// Whether it is a secret, with a value or without one.
    #[must_use]
    pub fn is_secret(&self) -> bool {
        matches!(self, Self::Secret { .. } | Self::SecretWithoutValue)
    }
}

/// A secret's value, sealed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SealedSecret {
    /// Tells it apart from another value without showing either.
    pub fingerprint: String,
    /// The value, encrypted.
    pub value: EncryptedSecretValue,
}

/// What a row holds with a secret's value: what lands, and what
/// [`unapply`](super::unapply) puts back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SealedCell {
    /// Anything but a secret with its value; a secret there by fingerprint only
    /// where the configuration holds no value for it.
    Cell(Cell),
    /// A secret with its value.
    Secret(SealedSecret),
}

impl SealedCell {
    /// What a plan compares it as.
    #[must_use]
    pub fn to_redacted(&self) -> Cell {
        match self {
            Self::Cell(cell) => cell.clone(),
            Self::Secret(secret) => Cell::Secret {
                fingerprint: secret.fingerprint.clone(),
            },
        }
    }
}

/// A node, as [`name_of`] finds it.
#[derive(Clone, Copy, Debug)]
pub enum NodeRef<'intent> {
    /// A Service.
    Service(&'intent SavedServiceIntent),
    /// A Volume.
    Volume(&'intent SavedVolumeIntent),
}

/// The node at `row` and where in it, with a mount named by its Volume; display only.
#[must_use]
pub fn name_of<'intent>(
    intent: &'intent Intent,
    row: &RowId,
) -> Option<(NodeRef<'intent>, String)> {
    let node = *nodes(intent).get(row.lineage.as_str())?;
    let at = match &row.at {
        At::Mount(lineage) => format!("mounts.{}", volume(intent, lineage)?.name),
        at @ (At::Node | At::Data | At::Name | At::Storage | At::Setting(_) | At::Variable(_)) => {
            at.to_string()
        }
    };
    Some((node, at))
}

pub(super) fn nodes(env: &Intent) -> BTreeMap<&str, NodeRef<'_>> {
    env.services
        .iter()
        .map(|s| (s.lineage_id.as_str(), NodeRef::Service(s)))
        .chain(
            env.volumes
                .iter()
                .map(|v| (v.resource_lineage_id.as_str(), NodeRef::Volume(v))),
        )
        .collect()
}

pub(super) fn service<'intent>(
    env: &'intent Intent,
    lineage: &str,
) -> Option<&'intent SavedServiceIntent> {
    env.services.iter().find(|s| s.lineage_id == lineage)
}

pub(super) fn service_mut<'intent>(
    env: &'intent mut Intent,
    lineage: &str,
) -> Option<&'intent mut SavedServiceIntent> {
    env.services.iter_mut().find(|s| s.lineage_id == lineage)
}

pub(super) fn volume<'intent>(
    env: &'intent Intent,
    lineage: &str,
) -> Option<&'intent SavedVolumeIntent> {
    env.volumes
        .iter()
        .find(|v| v.resource_lineage_id == lineage)
}

/// Every row an Environment holds something at, with what it holds there, redacted,
/// as a plan reads it: projected once per Environment, then read by row.
pub struct Cells(BTreeMap<RowId, Cell>);

impl Cells {
    /// Project `intent`, whose generated addresses end in `suffix`.
    #[must_use]
    pub fn of(intent: &Intent, suffix: &str) -> Self {
        let mut rows = BTreeMap::new();
        for (lineage, node) in nodes(intent) {
            let cells = cells(intent, node, suffix);
            rows.insert(RowId::node(lineage), node_cell(node, &cells));
            rows.extend(cells.into_iter().map(|(at, cell)| {
                let lineage = lineage.to_owned();
                (RowId { lineage, at }, cell)
            }));
        }
        Self(rows)
    }

    /// What it holds at `row`; a Volume's data reads as its node.
    #[must_use]
    pub fn at(&self, row: &RowId) -> &Cell {
        let found = match row.at {
            At::Data => self.0.get(&RowId::node(&row.lineage)),
            At::Node | At::Name | At::Storage | At::Setting(_) | At::Mount(_) | At::Variable(_) => {
                self.0.get(row)
            }
        };
        found.unwrap_or(&Cell::Absent)
    }

    /// Every row it holds something at.
    pub fn rows(&self) -> impl Iterator<Item = &RowId> {
        self.0.keys()
    }

    /// The places in node `lineage`, without the node itself.
    pub(super) fn places<'cells>(
        &'cells self,
        lineage: &str,
    ) -> impl Iterator<Item = (&'cells RowId, &'cells Cell)> {
        self.0
            .range(RowId::node(lineage)..)
            .take_while(move |(id, _)| id.lineage == lineage)
            .filter(|(id, _)| id.at != At::Node)
    }
}

/// A node's cell is what it arrives with as a new node: its name and the settings it
/// carries (no custom domains), each by where it is.
fn node_cell(node: NodeRef, cells: &BTreeMap<At, Cell>) -> Cell {
    let mut cell = serde_json::Map::new();
    if let NodeRef::Service(service) = node {
        cell.insert("name".to_owned(), json!(service.slug));
    }
    for (at, value) in cells {
        if let (true, Cell::Value(value)) = (at.needed(), value) {
            cell.insert(at.to_string(), value.clone());
        }
    }
    Cell::Value(Value::Object(cell))
}

/// `env`'s cell at `row` as `cells` projects it, a secret with its value where `env`
/// holds one.
pub(super) fn sealed_cell(env: &Intent, cells: &Cells, row: &RowId) -> SealedCell {
    let cell = cells.at(row).clone();
    let (Cell::Secret { fingerprint }, At::Variable(key)) = (&cell, &row.at) else {
        return SealedCell::Cell(cell);
    };
    let value = service(env, &row.lineage)
        .and_then(|service| service.variables.iter().find(|v| v.key == *key))
        .and_then(|variable| match &variable.value {
            SavedVariableValue::Secret { encrypted_value } => encrypted_value.clone(),
            SavedVariableValue::Literal { .. }
            | SavedVariableValue::Template { .. }
            | SavedVariableValue::SecretWithoutValue => None,
        });
    match value {
        Some(value) => SealedCell::Secret(SealedSecret {
            fingerprint: fingerprint.clone(),
            value,
        }),
        None => SealedCell::Cell(cell),
    }
}

/// Every place in `node` that holds something, normalized so copies compare with their
/// originals: generated addresses without `suffix`, credentials and custom domains by
/// presence and hostname, mounts by Volume lineage, secrets by fingerprint.
fn cells(env: &Intent, node: NodeRef, suffix: &str) -> BTreeMap<At, Cell> {
    let service = match node {
        NodeRef::Volume(volume) => {
            return BTreeMap::from([
                (At::Name, Cell::Value(json!(volume.name))),
                (At::Storage, Cell::Value(json!(volume.storage))),
            ]);
        }
        NodeRef::Service(service) => service,
    };
    let config = json!(service.config);
    let mut cells = BTreeMap::new();
    for setting in Setting::ALL {
        let value = match setting {
            // Repository authority (id and access) moves with the repository it names.
            Setting::Repository => match &service.config.source {
                ServiceSource::Git {
                    repository,
                    repository_id,
                    access,
                    ..
                } => json!({"access": access, "repository": repository, "repositoryId": repository_id}),
                ServiceSource::Empty { .. } | ServiceSource::Image { .. } => Value::Null,
            },
            Setting::Credentials => match &service.config.source {
                ServiceSource::Image {
                    credentials: ServiceImageCredentials::Configured { .. },
                    ..
                } => json!(true),
                ServiceSource::Image { .. } | ServiceSource::Empty { .. } | ServiceSource::Git { .. } => {
                    Value::Null
                }
            },
            Setting::Routes => {
                let mut domains: Vec<_> = service.config.routes.iter().map(|r| &r.hostname).collect();
                domains.sort();
                json!(domains)
            }
            Setting::ManagedHostnames => json!(
                service
                    .config
                    .managed_hostnames
                    .iter()
                    .map(|h| json!({"prefix": h.prefix.strip_suffix(suffix).unwrap_or(&h.prefix), "targetPort": h.target_port}))
                    .collect::<Vec<_>>()
            ),
            Setting::Image
            | Setting::RootDir
            | Setting::Branch
            | Setting::PrivateDns
            | Setting::PreDeployCommand
            | Setting::StartCommand
            | Setting::Healthcheck
            | Setting::RestartPolicy
            | Setting::MaxRetries
            | Setting::Replicas
            | Setting::CpuLimit
            | Setting::MemLimit
            | Setting::BuildMethod
            | Setting::DockerfilePath
            | Setting::BuildCommand => at(&config, setting.path()).clone(),
        };
        if !(value.is_null() || value == default_value(setting.path()) || value == json!([])) {
            cells.insert(At::Setting(setting), Cell::Value(value));
        }
    }
    for attachment in &service.volume_attachments {
        if let Some(volume) = env
            .volumes
            .iter()
            .find(|v| v.resource_id == attachment.volume_resource_id)
        {
            cells.insert(
                At::Mount(volume.resource_lineage_id.clone()),
                Cell::Value(json!(attachment.mount_path)),
            );
        }
    }
    for variable in &service.variables {
        cells.insert(At::Variable(variable.key.clone()), variable_cell(variable));
    }
    cells
}

fn variable_cell(variable: &SavedVariableIntent) -> Cell {
    match &variable.value {
        SavedVariableValue::Literal { .. } | SavedVariableValue::Template { .. } => {
            let mut value = json!(variable.value);
            if let Some(object) = value.as_object_mut() {
                object.insert("fingerprint".to_owned(), json!(variable.value_fingerprint));
            }
            Cell::Value(value)
        }
        SavedVariableValue::Secret { .. } => Cell::Secret {
            fingerprint: variable.value_fingerprint.clone(),
        },
        SavedVariableValue::SecretWithoutValue => Cell::SecretWithoutValue,
    }
}
