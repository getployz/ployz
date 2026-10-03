//! Paths into an Environment: a node by name, and what a path addresses in it.

use std::fmt;

use ployz_core::config::SavedVolumeIntent;
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use super::ServiceSetting;
use crate::error;
use crate::id::VolumeName;
use crate::variables::VariableKey;

/// A node of an Environment by name: `SERVICE` for a Service, `volumes.VOLUME` for a
/// Volume.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
pub enum NodeName {
    Service(ServiceName),
    Volume(VolumeName),
}

impl NodeName {
    /// Parse `SERVICE` or `volumes.VOLUME`.
    ///
    /// # Errors
    /// Returns `invalid_argument` for anything else, never echoing it.
    pub fn parse(name: &str) -> Result<Self, RpcError> {
        match name.strip_prefix("volumes.") {
            Some(volume) => Ok(Self::Volume(VolumeName::parse(volume)?)),
            // `volumes` addresses Volumes, never a Service, as in a Setting path.
            None if name == "volumes" => Err(error::invalid(
                "Name a Volume: volumes.VOLUME",
                json!({ "example": "volumes.data" }),
            )),
            None => ServiceName::parse(name).map(Self::Service).map_err(|_| {
                error::invalid(
                    "Expected a node name: SERVICE, or volumes.VOLUME for a Volume",
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
        }
    }
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
/// `SERVICE.mounts.VOLUME` for where it mounts a Volume, `volumes.VOLUME` for a
/// whole Volume, or `volumes.VOLUME.name` / `volumes.VOLUME.storage` for its name
/// or storage, which only discard addresses.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
pub struct SettingPath(Addressed);

#[derive(Clone, Debug, Eq, PartialEq)]
enum Addressed {
    Service(ServiceName, Option<Target>),
    Volume(VolumeName, Option<VolumeField>),
}

/// A Volume's field a change row names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VolumeField {
    Name,
    Storage,
}

impl VolumeField {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Storage => "storage",
        }
    }

    /// This field of `volume`, as JSON.
    pub(crate) fn of(self, volume: &SavedVolumeIntent) -> Value {
        match self {
            Self::Name => json!(volume.name),
            Self::Storage => json!(volume.storage),
        }
    }

    /// Give `volume` this field as `from` has it.
    pub(crate) fn restore(self, volume: &mut SavedVolumeIntent, from: &SavedVolumeIntent) {
        match self {
            Self::Name => volume.name.clone_from(&from.name),
            Self::Storage => volume.storage = from.storage,
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
    /// Where the Service mounts a Volume.
    Mount(VolumeName),
}

impl SettingPath {
    /// Parse `SERVICE`, `SERVICE.SETTING`, `SERVICE.env.KEY`,
    /// `SERVICE.env.KEY.exported`, `SERVICE.mounts.VOLUME` or `volumes.VOLUME`.
    ///
    /// # Errors
    /// Returns `invalid_argument` for a malformed path or an unknown Setting, never
    /// echoing the path.
    pub fn parse(path: &str) -> Result<Self, RpcError> {
        if path == "volumes" {
            return Err(error::invalid(
                "Name a Volume: volumes.VOLUME",
                json!({ "example": "volumes.data" }),
            ));
        }
        if let Some(volume) = path.strip_prefix("volumes.") {
            let (volume, field) = match volume.split_once('.') {
                None => (volume, None),
                Some((volume, "name")) => (volume, Some(VolumeField::Name)),
                Some((volume, "storage")) => (volume, Some(VolumeField::Storage)),
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
            Some(rest) if rest.starts_with("mounts.") => {
                let volume = rest.strip_prefix("mounts.").unwrap_or_default();
                Some(Target::Mount(VolumeName::parse(volume)?))
            }
            Some("source") => Some(Target::Source),
            Some(rest) => Some(match rest.strip_prefix("env.") {
                None => Target::Setting(ServiceSetting::parse(rest)?),
                Some(variable) => match variable.split_once('.') {
                    None => Target::Variable(VariableKey::parse(variable)?),
                    Some((key, "exported")) => Target::Exported(VariableKey::parse(key)?),
                    Some(_) => {
                        return Err(error::invalid(
                            "Unknown variable field",
                            json!({ "valid_children": ["exported"] }),
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
        }
    }

    /// The Volume field it names, if any.
    pub(crate) const fn volume_field(&self) -> Option<VolumeField> {
        match &self.0 {
            Addressed::Volume(_, field) => *field,
            Addressed::Service(..) => None,
        }
    }

    /// Core's change-row `field` of Service `service` as a path: `SERVICE.SETTING`,
    /// `SERVICE.env.KEY` or `SERVICE.mounts.VOLUME`, naming a Volume by
    /// `volume(id)`. Text, not a parsed path: a row may name a field no path does.
    pub(crate) fn from_core(service: &str, field: &str, volume: impl Fn(&str) -> String) -> String {
        let key = field
            .strip_prefix("env.")
            .or_else(|| field.strip_prefix("variables."));
        let field = match (key, field.strip_prefix("mounts.")) {
            (Some(key), _) => format!("env.{key}"),
            (_, Some(id)) => format!("mounts.{}", volume(id)),
            _ => field.to_owned(),
        };
        format!("{service}.{field}")
    }

    /// The Service it is in; none for a Volume.
    #[must_use]
    pub const fn service(&self) -> Option<&ServiceName> {
        match &self.0 {
            Addressed::Service(service, _) => Some(service),
            Addressed::Volume(..) => None,
        }
    }

    /// The Service whose Settings it addresses, or why a Volume has none.
    pub(crate) fn settings_of(&self) -> Result<&ServiceName, RpcError> {
        self.service().ok_or_else(|| {
            error::invalid(
                "A Volume has no Settings: name a Service, SERVICE.SETTING",
                json!({ "example": "web.replicas" }),
            )
        })
    }

    /// What it addresses inside its Service; none for a whole Service or a Volume.
    pub(crate) const fn target(&self) -> Option<&Target> {
        match &self.0 {
            Addressed::Service(_, target) => target.as_ref(),
            Addressed::Volume(..) => None,
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
        };
        match target {
            Some(Target::Setting(setting)) => write!(formatter, "{service}.{}", setting.name()),
            Some(Target::Source) => write!(formatter, "{service}.source"),
            Some(Target::Variable(key)) => write!(formatter, "{service}.env.{key}"),
            Some(Target::Exported(key)) => write!(formatter, "{service}.env.{key}.exported"),
            Some(Target::Mount(volume)) => write!(formatter, "{service}.mounts.{volume}"),
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
