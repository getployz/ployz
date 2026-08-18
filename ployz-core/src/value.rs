use std::{
    fmt,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    str::{self, FromStr},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ipnet::{IpNet, Ipv4Net};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A rejected shared value. Validation is identical on both sides of the wire.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("invalid {kind} {value:?}: {expected}")]
pub struct ValueError {
    kind: &'static str,
    value: String,
    expected: &'static str,
}

impl ValueError {
    pub(crate) fn new(
        kind: &'static str,
        value: impl Into<String>,
        expected: &'static str,
    ) -> Self {
        Self {
            kind,
            value: value.into(),
            expected,
        }
    }
}

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_dns_label(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn is_service_selector(value: &str) -> bool {
    !value.is_empty()
        && match value.split_once('/') {
            None => true,
            Some((project, name)) => is_dns_label(project) && is_dns_label(name),
        }
}

fn is_hostname(value: &str) -> bool {
    (1..=253).contains(&value.len()) && value.split('.').all(is_dns_label)
}

macro_rules! hex_id_newtype {
    ($(#[$attribute:meta])* $name:ident, $label:literal, $len:expr, $expected:literal) => {
        $(#[$attribute])*
        #[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name([u8; $len]);

        impl $name {
            pub fn parse(value: impl AsRef<str>) -> Result<Self, ValueError> {
                let value = value.as_ref();
                if !is_lower_hex(value, $len) {
                    return Err(ValueError::new($label, value, $expected));
                }
                let mut bytes = [0_u8; $len];
                bytes.copy_from_slice(value.as_bytes());
                Ok(Self(bytes))
            }

            pub fn as_str(&self) -> &str {
                str::from_utf8(&self.0).expect("hex IDs are ASCII")
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.debug_tuple(stringify!($name)).field(&self.as_str()).finish()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = ValueError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = ValueError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.as_str().to_owned()
            }
        }
    };
}

macro_rules! validated_string_newtype {
    ($(#[$attribute:meta])* $name:ident, $label:literal, $expected:expr, |$value:ident| $valid:expr) => {
        $(#[$attribute])*
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self, ValueError> {
                let value = value.into();
                let $value = value.as_str();
                if $valid {
                    Ok(Self(value))
                } else {
                    Err(ValueError::new($label, value, $expected))
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = ValueError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = ValueError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

hex_id_newtype!(
    MachineId,
    "Machine ID",
    32,
    "32 lowercase hexadecimal characters"
);
hex_id_newtype!(
    ServiceId,
    "Service ID",
    32,
    "32 lowercase hexadecimal characters"
);
hex_id_newtype!(
    TunnelId,
    "Tunnel ID",
    32,
    "32 lowercase hexadecimal characters"
);
hex_id_newtype!(
    ContainerId,
    "Container ID",
    64,
    "64 lowercase hexadecimal characters"
);

impl MachineId {
    /// Generate the same 32-character lowercase hexadecimal identity shape as the baseline.
    #[must_use]
    pub fn random() -> Self {
        let mut hex = [0_u8; 32];
        uuid::Uuid::new_v4().simple().encode_lower(&mut hex);
        Self(hex)
    }
}

impl ServiceId {
    #[must_use]
    pub fn random() -> Self {
        let mut hex = [0_u8; 32];
        uuid::Uuid::new_v4().simple().encode_lower(&mut hex);
        Self(hex)
    }
}

impl TunnelId {
    /// Generate a 32-character lowercase hexadecimal Tunnel ID.
    #[must_use]
    pub fn random() -> Self {
        let mut hex = [0_u8; 32];
        uuid::Uuid::new_v4().simple().encode_lower(&mut hex);
        Self(hex)
    }
}

macro_rules! open_string_enum {
    ($name:ident, $fallback:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub enum $name {
            $($variant,)+
            $fallback(String),
        }

        impl $name {
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $wire,)+
                    Self::$fallback(value) => value,
                }
            }

            /// Known wire spellings, excluding the unknown fallback.
            #[must_use]
            pub const fn known_wires() -> &'static [&'static str] {
                &[$($wire,)+]
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Ok(match value.as_str() {
                    $($wire => Self::$variant,)+
                    _ => Self::$fallback(value),
                })
            }
        }
    };
}

pub(crate) use open_string_enum;

validated_string_newtype!(
    /// A DNS-label Machine selector. It is not a unique identity.
    MachineName,
    "Machine Name",
    "a 1-63 character lowercase DNS label",
    |value| is_dns_label(value)
);
validated_string_newtype!(
    DockerVolumeName,
    "Docker Volume name",
    "a non-empty string",
    |value| !value.is_empty()
);

validated_string_newtype!(
    ServiceVolumeReference,
    "Service Volume Reference",
    "a non-empty string",
    |value| !value.is_empty()
);
validated_string_newtype!(
    MachinePath,
    "Bind Mount Machine path",
    "an absolute Unix path",
    |value| value.starts_with('/')
);
validated_string_newtype!(
    ContainerPath,
    "container mount target",
    "an absolute Unix path",
    |value| value.starts_with('/')
);
validated_string_newtype!(
    /// Unresolved name-or-ID text that targets one Machine. It cannot be a wildcard.
    MachineTarget,
    "Machine Target",
    "a non-empty Machine identity that is not a wildcard",
    |value| !value.is_empty() && value != "*"
);
validated_string_newtype!(
    /// Unresolved name-or-ID text used to select a Service.
    ServiceSelector,
    "Service Selector",
    "a Service ID, Qualified Service (project/name), or Service Name",
    |value| is_service_selector(value)
);
validated_string_newtype!(
    /// Unresolved Container ID, display name, or ID prefix used to select one Container.
    ContainerSelector,
    "Container Selector",
    "a non-empty Container ID, display name, or ID prefix",
    |value| !value.is_empty()
);

impl From<&MachineId> for MachineTarget {
    fn from(value: &MachineId) -> Self {
        Self(value.to_string())
    }
}

impl From<&ServiceId> for ServiceSelector {
    fn from(value: &ServiceId) -> Self {
        Self(value.to_string())
    }
}

/// Fan-out selection of every visible Machine or one Machine Target.
///
/// `*` is the only wildcard spelling.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum FanoutSelector {
    All,
    One(MachineTarget),
}

impl FanoutSelector {
    /// Parse `*` as [`FanoutSelector::All`], or a Machine Target as [`FanoutSelector::One`].
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty.
    pub fn parse(value: impl Into<String>) -> Result<Self, ValueError> {
        let value = value.into();
        if value == "*" {
            Ok(Self::All)
        } else {
            MachineTarget::parse(value).map(Self::One)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::All => "*",
            Self::One(target) => target.as_str(),
        }
    }
}

impl fmt::Display for FanoutSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for FanoutSelector {
    type Err = ValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<String> for FanoutSelector {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<FanoutSelector> for String {
    fn from(value: FanoutSelector) -> Self {
        value.as_str().to_owned()
    }
}

/// A machine-local Docker Volume identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct DockerVolumeId {
    pub machine_id: MachineId,
    pub name: DockerVolumeName,
}

validated_string_newtype!(
    /// A DNS-label Service selector. It is not a unique identity.
    ServiceName,
    "Service Name",
    "a 1-63 character lowercase DNS label",
    |value| is_dns_label(value)
);
validated_string_newtype!(
    /// A DNS-label Project name. It is an ownership namespace, not a persisted identity.
    ProjectName,
    "Project Name",
    "a 1-63 character lowercase DNS label; underscores and uppercase are not accepted",
    |value| is_dns_label(value)
);

impl ProjectName {
    /// The reserved Project for Ployz infrastructure.
    pub const SYSTEM: &'static str = "ployz-system";

    /// The reserved Project name for Ployz infrastructure Containers.
    #[must_use]
    pub fn system() -> Self {
        Self::parse(Self::SYSTEM).expect("the reserved Project name is a valid DNS label")
    }

    /// Whether this name is reserved for Ployz infrastructure.
    #[must_use]
    pub fn is_reserved(&self) -> bool {
        self.as_str() == Self::SYSTEM
    }
}

/// Logical Service identity: Project Name plus Service Name, written `project/name`.
///
/// A Service ID is a separate opaque deployment identity that survives updates.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct QualifiedService {
    pub project: ProjectName,
    pub name: ServiceName,
}

impl QualifiedService {
    /// Combine an already-valid Project Name and Service Name.
    #[must_use]
    pub fn new(project: ProjectName, name: ServiceName) -> Self {
        Self { project, name }
    }

    /// Parse `project/name` where both sides are DNS labels.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when `value` is not exactly one `/` between two DNS labels.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ValueError> {
        let value = value.as_ref();
        let Some((project, name)) = value.split_once('/') else {
            return Err(qualified_service_error(value));
        };
        let project = ProjectName::parse(project).map_err(|_| qualified_service_error(value))?;
        let name = ServiceName::parse(name).map_err(|_| qualified_service_error(value))?;
        Ok(Self { project, name })
    }

    /// Internal DNS labels `{name}.{project}` under the `.internal` zone.
    #[must_use]
    pub fn dns_name(&self) -> String {
        format!("{}.{}", self.name, self.project)
    }

    /// Combined public ingress DNS label `{name}-{project}` under a Cluster Domain wildcard.
    ///
    /// # Errors
    ///
    /// Returns [`IngressLabelTooLong`] when the hyphenated label exceeds 63 characters.
    pub fn ingress_label(&self) -> Result<String, IngressLabelTooLong> {
        let label = format!("{}-{}", self.name, self.project);
        if label.len() > 63 {
            return Err(IngressLabelTooLong { label });
        }
        Ok(label)
    }

    /// Parse Internal DNS labels `{name}.{project}`.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when `value` is not exactly one `.` between two DNS labels.
    pub fn parse_dns_name(value: impl AsRef<str>) -> Result<Self, ValueError> {
        let value = value.as_ref();
        let Some((name, project)) = value.split_once('.') else {
            return Err(dns_name_error(value));
        };
        if project.contains('.') {
            return Err(dns_name_error(value));
        }
        let project = ProjectName::parse(project).map_err(|_| dns_name_error(value))?;
        let name = ServiceName::parse(name).map_err(|_| dns_name_error(value))?;
        Ok(Self { project, name })
    }

    /// Infrastructure Caddy in the reserved Project.
    #[must_use]
    pub fn system_caddy() -> Self {
        Self::new(
            ProjectName::system(),
            ServiceName::parse("caddy").expect("caddy is a DNS-label Service Name"),
        )
    }
}

/// Combined `{name}-{project}` label for a public Ingress Hostname.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error(
    "generated Ingress Hostname label \"{label}\" exceeds the 63-character DNS label limit; shorten the Service Name or Project Name, or supply a custom hostname"
)]
pub struct IngressLabelTooLong {
    pub label: String,
}

fn qualified_service_error(value: &str) -> ValueError {
    ValueError::new(
        "Qualified Service",
        value,
        "a Project Name, '/', and a Service Name",
    )
}

fn dns_name_error(value: &str) -> ValueError {
    ValueError::new(
        "Qualified Service DNS name",
        value,
        "a Service Name, '.', and a Project Name",
    )
}

impl fmt::Display for QualifiedService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.project, self.name)
    }
}

impl FromStr for QualifiedService {
    type Err = ValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<String> for QualifiedService {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<QualifiedService> for String {
    fn from(value: QualifiedService) -> Self {
        value.to_string()
    }
}
validated_string_newtype!(
    /// A validated HTTP ingress hostname. It is not a Machine Name.
    IngressHost,
    "Ingress Hostname",
    "a 1-253 character lowercase DNS hostname",
    |value| is_hostname(value)
);

/// One Machine's optimistic container subnet candidate.
///
/// A Machine Subnet is always an IPv4 `/24`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MachineSubnet(Ipv4Net);

impl MachineSubnet {
    /// Parse an IPv4 `/24` CIDR as a Machine Subnet.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when the value is not an IPv4 `/24` CIDR.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ValueError> {
        let value = value.as_ref();
        let network = value
            .parse::<Ipv4Net>()
            .map_err(|_| ValueError::new("Machine Subnet", value, "an IPv4 /24 CIDR"))?;
        Self::from_net(network)
            .map_err(|_| ValueError::new("Machine Subnet", value, "an IPv4 /24 CIDR"))
    }

    fn from_net(network: Ipv4Net) -> Result<Self, ValueError> {
        if network.prefix_len() != 24 {
            return Err(ValueError::new(
                "Machine Subnet",
                network.to_string(),
                "an IPv4 /24 CIDR",
            ));
        }
        Ok(Self(network.trunc()))
    }

    /// The Machine-local gateway: the first usable address in this subnet.
    #[must_use]
    pub fn gateway(self) -> MachineGateway {
        MachineGateway(Ipv4Addr::from(u32::from(self.0.network()) + 1))
    }
}

impl fmt::Display for MachineSubnet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl FromStr for MachineSubnet {
    type Err = ValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<String> for MachineSubnet {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl TryFrom<Ipv4Net> for MachineSubnet {
    type Error = ValueError;

    fn try_from(network: Ipv4Net) -> Result<Self, Self::Error> {
        Self::from_net(network)
    }
}

impl From<MachineSubnet> for Ipv4Net {
    fn from(value: MachineSubnet) -> Self {
        value.0
    }
}

impl From<MachineSubnet> for IpNet {
    fn from(value: MachineSubnet) -> Self {
        Self::V4(value.0)
    }
}

impl From<MachineSubnet> for String {
    fn from(value: MachineSubnet) -> Self {
        value.to_string()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ManagementAddress(pub Ipv6Addr);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MachineGateway(pub Ipv4Addr);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContainerAddress(pub Ipv4Addr);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AdvertisedEndpoint(pub SocketAddr);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SelectedEndpoint(pub SocketAddr);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WireGuardPublicKey(pub [u8; 32]);

impl fmt::Display for WireGuardPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&STANDARD.encode(self.0))
    }
}

validated_string_newtype!(
    /// An open wire capability name using a stable namespace.
    CapabilityName,
    "capability name",
    "at least three dot-separated lowercase namespace segments",
    |value| {
        let valid_segment = |segment: &str| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        };
        value.split('.').count() >= 3 && value.split('.').all(valid_segment)
    }
);
