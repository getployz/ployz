//! Paths into an Environment: a node by name, and what a path addresses in it.

use std::fmt;

use ployz_core::config::{SavedConfigIntent, SavedEnvironmentIntent, SavedVolumeIntent};
use ployz_core::{ConfigFileName, ConfigName, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use super::ServiceSetting;
use crate::error;
use crate::id::{ConfigRef, VolumeName};
use crate::variables::VariableKey;

/// A node of an Environment: `SERVICE` for a Service, `volumes.VOLUME` for a
/// Volume, `configs.CONFIG` for a Config by name or `configs.@UUID` by identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
pub enum NodeName {
    Service(ServiceName),
    Volume(VolumeName),
    Config(ConfigRef),
}

impl NodeName {
    /// Parse `SERVICE`, `volumes.VOLUME`, `configs.CONFIG` or `configs.@UUID`.
    ///
    /// # Errors
    /// Returns `invalid_argument` for anything else, never echoing it.
    pub fn parse(name: &str) -> Result<Self, RpcError> {
        if let Some(volume) = name.strip_prefix("volumes.") {
            return Ok(Self::Volume(VolumeName::parse(volume)?));
        }
        if let Some(config) = name.strip_prefix("configs.") {
            return Ok(Self::Config(ConfigRef::parse(config)?));
        }
        match name {
            "volumes" => Err(name_a_volume()),
            "configs" => Err(name_a_config()),
            _ => ServiceName::parse(name).map(Self::Service).map_err(|_| {
                error::invalid(
                    "Expected a node name: SERVICE, volumes.VOLUME for a Volume, or configs.CONFIG for a Config",
                    json!({ "example": "web" }),
                )
            }),
        }
    }
}

impl fmt::Display for NodeName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Service(service) => write!(formatter, "{service}"),
            Self::Volume(volume) => write!(formatter, "volumes.{volume}"),
            Self::Config(config) => write!(formatter, "configs.{config}"),
        }
    }
}

fn name_a_volume() -> RpcError {
    error::invalid(
        "Name a Volume: volumes.VOLUME",
        json!({ "example": "volumes.data" }),
    )
}

fn name_a_config() -> RpcError {
    error::invalid(
        "Name a Config: configs.CONFIG",
        json!({ "example": "configs.sentry" }),
    )
}

/// A Config name in a path, refused without echoing it.
pub(crate) fn config_name(name: &str) -> Result<ConfigName, RpcError> {
    ConfigName::parse(name).map_err(|_| {
        error::invalid(
            "Expected a Config name: up to 63 lowercase letters, digits and -",
            json!({ "example": "sentry" }),
        )
    })
}

impl TryFrom<String> for NodeName {
    type Error = RpcError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<NodeName> for String {
    fn from(value: NodeName) -> Self {
        value.to_string()
    }
}

/// What a request addresses in an Environment: `SERVICE` for a whole Service,
/// `SERVICE.SETTING` for one of its Settings, `SERVICE.env.KEY` for one of its
/// variables, `SERVICE.env.KEY.exported` for whether other Services see it,
/// `SERVICE.mounts.VOLUME` for where it mounts a Volume, `SERVICE.configs.CONFIG`
/// for where it mounts a Config, `volumes.VOLUME` for a whole Volume,
/// `volumes.VOLUME.name` / `volumes.VOLUME.storage` for its name or storage, which
/// only discard addresses, `configs.CONFIG` for a whole Config, or
/// `configs.CONFIG.name` / `configs.CONFIG.files.FILE` for its name or one file, which
/// only discard addresses.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
pub struct SettingPath(Addressed);

#[derive(Clone, Debug, Eq, PartialEq)]
enum Addressed {
    Service(ServiceName, Option<Target>),
    Volume(VolumeName, Option<VolumeField>),
    Config(ConfigRef, Option<ConfigField>),
}

/// A Volume's or Config's field a change row names: what discard restores alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NodeField<'a> {
    Volume(VolumeField),
    Config(&'a ConfigField),
}

impl NodeField<'_> {
    /// This field of node `id` in `intent`, as JSON; none when `intent` lacks the node.
    pub(crate) fn of(self, intent: &SavedEnvironmentIntent, id: &str) -> Option<Value> {
        match self {
            Self::Volume(field) => intent
                .volumes
                .iter()
                .find(|volume| volume.resource_id == id)
                .map(|volume| field.of(volume)),
            Self::Config(field) => intent
                .configs
                .iter()
                .find(|config| config.resource_id == id)
                .map(|config| field.of(config)),
        }
    }

    /// Give node `id` in `intent` this field as `from` has it.
    ///
    /// # Errors
    /// Why not, when either lacks the node.
    pub(crate) fn restore(
        self,
        intent: &mut SavedEnvironmentIntent,
        from: &SavedEnvironmentIntent,
        id: &str,
    ) -> Result<(), String> {
        match self {
            Self::Volume(field) => {
                let gone = || "discard the whole Volume instead".to_owned();
                let from = from
                    .volumes
                    .iter()
                    .find(|volume| volume.resource_id == id)
                    .ok_or_else(gone)?;
                let volume = intent
                    .volumes
                    .iter_mut()
                    .find(|volume| volume.resource_id == id)
                    .ok_or_else(gone)?;
                field.restore(volume, from);
            }
            Self::Config(field) => {
                let gone = || "discard the whole Config instead".to_owned();
                let from = from
                    .configs
                    .iter()
                    .find(|config| config.resource_id == id)
                    .ok_or_else(gone)?;
                let config = intent
                    .configs
                    .iter_mut()
                    .find(|config| config.resource_id == id)
                    .ok_or_else(gone)?;
                field.restore(config, from);
            }
        }
        Ok(())
    }
}

/// A Config's field a change row names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ConfigField {
    Name,
    File(ConfigFileName),
}

impl ConfigField {
    fn of(&self, config: &SavedConfigIntent) -> Value {
        match self {
            Self::Name => json!(config.name),
            Self::File(file) => json!(config.files.get(file)),
        }
    }

    fn restore(&self, config: &mut SavedConfigIntent, from: &SavedConfigIntent) {
        match self {
            Self::Name => config.name.clone_from(&from.name),
            Self::File(file) => match from.files.get(file) {
                Some(kept) => {
                    config.files.insert(file.clone(), kept.clone());
                }
                None => {
                    config.files.remove(file);
                }
            },
        }
    }
}

/// A Volume's field a change row names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VolumeField {
    Name,
    Storage,
    SharedWrites,
}

impl VolumeField {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Storage => "storage",
            Self::SharedWrites => "sharedWrites",
        }
    }

    /// This field of `volume`, as JSON.
    fn of(self, volume: &SavedVolumeIntent) -> Value {
        match self {
            Self::Name => json!(volume.name),
            Self::Storage => json!(volume.storage),
            Self::SharedWrites => json!(volume.shared_writes),
        }
    }

    /// Give `volume` this field as `from` has it.
    fn restore(self, volume: &mut SavedVolumeIntent, from: &SavedVolumeIntent) {
        match self {
            Self::Name => volume.name.clone_from(&from.name),
            Self::Storage => volume.storage = from.storage,
            Self::SharedWrites => volume.shared_writes = from.shared_writes,
        }
    }
}

/// What a path addresses inside its Service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Target {
    Setting(ServiceSetting),
    /// Where it runs from, whole: an image, a repository or nothing. One change row, so
    /// only discard and Never sync name it; `image` and `repository` set it.
    Source,
    /// A variable's value.
    Variable(VariableKey),
    /// Whether a variable is exported.
    Exported(VariableKey),
    /// Template/import metadata; only reads and Discard address it.
    Description(VariableKey),
    /// Where the Service mounts a Volume.
    Mount(VolumeName),
    /// The directory the Service mounts a Config at.
    ConfigMount(ConfigRef),
}

impl SettingPath {
    /// Parse `SERVICE`, `SERVICE.SETTING`, `SERVICE.env.KEY`,
    /// `SERVICE.env.KEY.exported`, `SERVICE.mounts.VOLUME`, `SERVICE.configs.CONFIG`,
    /// `volumes.VOLUME`, `volumes.VOLUME.name`, `volumes.VOLUME.storage`,
    /// `configs.CONFIG`, `configs.CONFIG.name` or `configs.CONFIG.files.FILE`.
    /// Use `@UUID` in place of `CONFIG` to select an exact Config identity.
    ///
    /// # Errors
    /// Returns `invalid_argument` for a malformed path or an unknown Setting, never
    /// echoing the path.
    pub fn parse(path: &str) -> Result<Self, RpcError> {
        if path == "volumes" {
            return Err(name_a_volume());
        }
        if path == "configs" {
            return Err(name_a_config());
        }
        if let Some(config) = path.strip_prefix("configs.") {
            let (config, field) = match config.split_once('.') {
                None => (config, None),
                Some((config, "name")) => (config, Some(ConfigField::Name)),
                Some((config, field)) if field.starts_with("files.") => {
                    let file = field.strip_prefix("files.").unwrap_or_default();
                    let file = ConfigFileName::parse(file).map_err(|_| {
                        error::invalid(
                            "Expected a Config file name, like nginx.conf or conf.d/site.conf",
                            json!({ "example": "configs.sentry.files.config.yml" }),
                        )
                    })?;
                    (config, Some(ConfigField::File(file)))
                }
                Some(_) => {
                    return Err(error::invalid(
                        "A Config has no Settings: address it as configs.CONFIG",
                        json!({ "example": "configs.sentry" }),
                    ));
                }
            };
            return Ok(Self(Addressed::Config(ConfigRef::parse(config)?, field)));
        }
        if let Some(volume) = path.strip_prefix("volumes.") {
            let (volume, field) = match volume.split_once('.') {
                None => (volume, None),
                Some((volume, "name")) => (volume, Some(VolumeField::Name)),
                Some((volume, "storage")) => (volume, Some(VolumeField::Storage)),
                Some((volume, "sharedWrites")) => (volume, Some(VolumeField::SharedWrites)),
                Some(_) => {
                    return Err(error::invalid(
                        "A Volume has no Settings: address it as volumes.VOLUME",
                        json!({ "example": "volumes.data" }),
                    ));
                }
            };
            return Ok(Self(Addressed::Volume(VolumeName::parse(volume)?, field)));
        }
        let (service, rest) = path.split_once('.').unzip();
        // Service names are lowercase, so `REDIS.replicas` can only mean `redis`.
        let service =
            ServiceName::parse(service.unwrap_or(path).to_ascii_lowercase()).map_err(|_| {
                error::invalid(
                    "Expected a path like SERVICE.SETTING",
                    json!({ "example": "web.replicas" }),
                )
            })?;
        let target = match rest {
            None => None,
            Some("env") => {
                return Err(error::invalid(
                    "Name a variable: SERVICE.env.KEY",
                    json!({ "example": format!("{service}.env.DATABASE_URL") }),
                ));
            }
            Some("mounts") => {
                return Err(error::invalid(
                    "Name a Volume: SERVICE.mounts.VOLUME",
                    json!({ "example": format!("{service}.mounts.data") }),
                ));
            }
            Some("configs") => {
                return Err(error::invalid(
                    "Name a Config: SERVICE.configs.CONFIG",
                    json!({ "example": format!("{service}.configs.sentry") }),
                ));
            }
            Some(rest) if rest.starts_with("mounts.") => {
                let volume = rest.strip_prefix("mounts.").unwrap_or_default();
                Some(Target::Mount(VolumeName::parse(volume)?))
            }
            Some(rest) if rest.starts_with("configs.") => {
                let config = rest.strip_prefix("configs.").unwrap_or_default();
                Some(Target::ConfigMount(ConfigRef::parse(config)?))
            }
            Some("source") => Some(Target::Source),
            Some(rest) => Some(match rest.strip_prefix("env.") {
                None => Target::Setting(ServiceSetting::parse(rest)?),
                Some(variable) => match variable.split_once('.') {
                    None => Target::Variable(VariableKey::parse(variable)?),
                    Some((key, "exported")) => Target::Exported(VariableKey::parse(key)?),
                    Some((key, "description")) => Target::Description(VariableKey::parse(key)?),
                    Some(_) => {
                        return Err(error::invalid(
                            "Unknown variable field",
                            json!({ "valid_children": ["exported", "description"] }),
                        ));
                    }
                },
            }),
        };
        Ok(Self(Addressed::Service(service, target)))
    }

    /// The node it is in.
    #[must_use]
    pub fn node(&self) -> NodeName {
        match &self.0 {
            Addressed::Service(service, _) => NodeName::Service(service.clone()),
            Addressed::Volume(volume, _) => NodeName::Volume(volume.clone()),
            Addressed::Config(config, _) => NodeName::Config(config.clone()),
        }
    }

    /// The Volume or Config field it names, if any.
    pub(crate) fn node_field(&self) -> Option<NodeField<'_>> {
        match &self.0 {
            Addressed::Volume(_, field) => field.map(NodeField::Volume),
            Addressed::Config(_, field) => field.as_ref().map(NodeField::Config),
            Addressed::Service(..) => None,
        }
    }

    /// Core's change-row `field` of Service `service` as a path: `SERVICE.SETTING`,
    /// `SERVICE.env.KEY`, `SERVICE.mounts.VOLUME` or `SERVICE.configs.CONFIG`, naming
    /// a Volume or Config by `named(family, id)`. Text, not a parsed path: a row may
    /// name a field no path does.
    pub(crate) fn from_core(
        service: &str,
        field: &str,
        named: impl Fn(&str, &str) -> String,
    ) -> String {
        let key = field
            .strip_prefix("env.")
            .or_else(|| field.strip_prefix("variables."));
        let field = match (key, field.split_once('.')) {
            (Some(key), _) => format!("env.{key}"),
            (_, Some(("configs", id))) => format!("configs.@{id}"),
            (_, Some((family @ "mounts", id))) => {
                format!("{family}.{}", named(family, id))
            }
            _ => field.to_owned(),
        };
        format!("{service}.{field}")
    }

    /// The Service it is in; none for a Volume or a Config.
    #[must_use]
    pub const fn service(&self) -> Option<&ServiceName> {
        match &self.0 {
            Addressed::Service(service, _) => Some(service),
            Addressed::Volume(..) | Addressed::Config(..) => None,
        }
    }

    /// The Service whose Settings it addresses, or why a Volume or Config has none.
    pub(crate) fn settings_of(&self) -> Result<&ServiceName, RpcError> {
        self.service().ok_or_else(|| {
            let what = match &self.0 {
                Addressed::Config(..) => "A Config",
                Addressed::Volume(..) | Addressed::Service(..) => "A Volume",
            };
            error::invalid(
                format!("{what} has no Settings: name a Service, SERVICE.SETTING"),
                json!({ "example": "web.replicas" }),
            )
        })
    }

    /// What it addresses inside its Service; none for a whole Service, a Volume or
    /// a Config.
    pub(crate) const fn target(&self) -> Option<&Target> {
        match &self.0 {
            Addressed::Service(_, target) => target.as_ref(),
            Addressed::Volume(..) | Addressed::Config(..) => None,
        }
    }

    /// The path of `service` as a whole.
    pub(crate) fn whole(service: &ServiceName) -> Self {
        Self(Addressed::Service(service.clone(), None))
    }

    /// The path of Volume `volume` as a whole.
    pub(crate) fn volume(volume: &VolumeName) -> Self {
        Self(Addressed::Volume(volume.clone(), None))
    }

    /// The path of Config `config` as a whole.
    pub(crate) fn config(config: &ConfigName) -> Self {
        Self(Addressed::Config(config.clone().into(), None))
    }

    /// The path of one Setting of `service`.
    pub(crate) fn of(service: &ServiceName, setting: ServiceSetting) -> Self {
        Self::at(service, Target::Setting(setting))
    }

    pub(crate) fn at(service: &ServiceName, target: Target) -> Self {
        Self(Addressed::Service(service.clone(), Some(target)))
    }
}

impl fmt::Display for SettingPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (service, target) = match &self.0 {
            Addressed::Service(service, target) => (service, target),
            Addressed::Volume(volume, Some(field)) => {
                return write!(formatter, "volumes.{volume}.{}", field.name());
            }
            Addressed::Volume(volume, None) => return write!(formatter, "volumes.{volume}"),
            Addressed::Config(config, Some(ConfigField::Name)) => {
                return write!(formatter, "configs.{config}.name");
            }
            Addressed::Config(config, Some(ConfigField::File(file))) => {
                return write!(formatter, "configs.{config}.files.{file}");
            }
            Addressed::Config(config, None) => return write!(formatter, "configs.{config}"),
        };
        match target {
            Some(Target::Setting(setting)) => write!(formatter, "{service}.{}", setting.name()),
            Some(Target::Source) => write!(formatter, "{service}.source"),
            Some(Target::Variable(key)) => write!(formatter, "{service}.env.{key}"),
            Some(Target::Exported(key)) => write!(formatter, "{service}.env.{key}.exported"),
            Some(Target::Description(key)) => write!(formatter, "{service}.env.{key}.description"),
            Some(Target::Mount(volume)) => write!(formatter, "{service}.mounts.{volume}"),
            Some(Target::ConfigMount(config)) => {
                write!(formatter, "{service}.configs.{config}")
            }
            None => write!(formatter, "{service}"),
        }
    }
}

impl TryFrom<String> for SettingPath {
    type Error = RpcError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<SettingPath> for String {
    fn from(value: SettingPath) -> Self {
        value.to_string()
    }
}

/// A path that stops at its node: name one of its Settings.
pub(crate) fn name_a_setting(node: impl std::fmt::Display) -> RpcError {
    error::invalid(
        "Name a Setting: SERVICE.SETTING",
        json!({
            "valid_children": ServiceSetting::ALL.map(ServiceSetting::name),
            "example": format!("{node}.replicas"),
        }),
    )
}
