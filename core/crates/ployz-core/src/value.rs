use std::{
    fmt,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    str::{self, FromStr},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ipnet::{IpNet, Ipv4Net};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;

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

/// Whether `value` is exactly `len` lowercase hexadecimal characters.
#[must_use]
pub fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn is_dns_label(value: &str) -> bool {
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

        /// Branded on the TypeScript side: an identity is never interchangeable
        /// with a selector or another identity, even though both are strings.
        /// Hand-written because `#[ts(type = "...")]` takes only a literal, and
        /// the brand carries the type name.
        impl TS for $name {
            type WithoutGenerics = Self;
            type OptionInnerType = Self;

            fn name(_: &ts_rs::Config) -> String {
                stringify!($name).to_owned()
            }

            fn inline(_: &ts_rs::Config) -> String {
                concat!("string & { readonly __brand: \"", stringify!($name), "\" }").to_owned()
            }

            fn decl(cfg: &ts_rs::Config) -> String {
                format!("type {} = {};", Self::name(cfg), Self::inline(cfg))
            }

            // `Some` marks a type with its own declaration; the path itself is
            // never written. Primitives return `None`.
            fn output_path() -> Option<std::path::PathBuf> {
                Some(std::path::PathBuf::from(concat!(stringify!($name), ".ts")))
            }
        }
    };
}

macro_rules! validated_string_newtype {
    ($(#[$attribute:meta])* $name:ident, $label:literal, $expected:expr, |$value:ident| $valid:expr) => {
        $(#[$attribute])*
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
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

validated_string_newtype!(
    /// An image reference pinned to a SHA-256 digest, preserving its raw repository path.
    ImageDigestReference, "image digest reference", "an image repository pinned to a SHA-256 digest",
    |value| value.parse::<oci_spec::distribution::Reference>().is_ok_and(|reference| {
        reference.digest().and_then(|digest| digest.strip_prefix("sha256:")).is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    })
);

validated_string_newtype!(
    /// A manifest digest as the Machine stored it: `sha256:` and 64 lowercase hex.
    ImageDigest, "image digest", "`sha256:` followed by 64 lowercase hexadecimal characters",
    |value| value.strip_prefix("sha256:").is_some_and(|hex| is_lower_hex(hex, 64))
);

impl ImageDigest {
    /// The digest without its `sha256:` algorithm prefix.
    #[must_use]
    pub fn hex(&self) -> &str {
        &self.as_str()["sha256:".len()..]
    }
}

validated_string_newtype!(
    /// The one repository a Build Grant may push into, in Docker's short form:
    /// lowercase path components and no registry or tag (`ployz-build/web`).
    BuildGrantRepository, "Build Grant repository", "a Docker repository path without a registry or tag",
    |value| (1..=255).contains(&value.len())
        && value.split('/').all(|component| {
            component.bytes().next().is_some_and(|byte| byte.is_ascii_alphanumeric())
                && component.bytes().all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
        })
);

pub(crate) fn is_swarm_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"_-.".contains(&byte)
}

pub(crate) fn is_swarm_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b":-_ \t\r\n\x0c.*()?+[]\\^$|/".contains(&byte)
}

validated_string_newtype!(
    /// A nonempty ASCII Machine Label key selectable by a placement constraint.
    MachineLabelKey, "Machine Label key", "nonempty ASCII letters, digits, '_', '.', or '-'",
    |value| !value.is_empty() && value.bytes().all(is_swarm_key_byte)
);
validated_string_newtype!(
    /// A nonempty printable Swarm value with no leading or trailing whitespace.
    MachineLabelValue, "Machine Label value", "a nonempty printable ASCII Swarm value without leading or trailing whitespace",
    |value| !value.is_empty()
        && value.trim() == value
        && value.bytes().all(|byte| !byte.is_ascii_control() && is_swarm_value_byte(byte))
);

impl std::borrow::Borrow<str> for MachineLabelKey {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

hex_id_newtype!(
    MachineId,
    "Machine ID",
    32,
    "32 lowercase hexadecimal characters"
);
hex_id_newtype!(
    /// Identity of one bounded Machine upgrade attempt.
    MachineUpgradeAttemptId,
    "Machine upgrade attempt ID",
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
    /// Public handle of one Build Grant: its key's public half. Not secret.
    BuildGrantId,
    "Build Grant ID",
    64,
    "64 lowercase hexadecimal characters"
);
hex_id_newtype!(
    ContainerId,
    "Container ID",
    64,
    "64 lowercase hexadecimal characters"
);

impl MachineId {
    /// Random 32-character lowercase hexadecimal Machine ID.
    #[must_use]
    pub fn random() -> Self {
        let mut hex = [0_u8; 32];
        uuid::Uuid::new_v4().simple().encode_lower(&mut hex);
        Self(hex)
    }
}

impl MachineUpgradeAttemptId {
    /// Random 32-character lowercase hexadecimal attempt ID.
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
        /// Known spellings plus the observed value of any spelling this reader
        /// does not know, carried verbatim.
        #[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
        pub enum $name {
            $(#[serde(rename = $wire)] $variant,)+
            #[serde(untagged)]
            $fallback(String),
        }

        impl $name {
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $wire,)+
                    Self::$fallback(value) => value,
                }
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                match value {
                    $($wire => Self::$variant,)+
                    other => Self::$fallback(other.to_owned()),
                }
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
    /// Names one Management Client slot on a Machine, such as `cloud`.
    ManagementClientLabel,
    "Management Client label",
    "a lowercase letter then up to 31 lowercase letters, digits or hyphens",
    |value| {
        let mut bytes = value.bytes();
        bytes.next().is_some_and(|first| first.is_ascii_lowercase())
            && value.len() <= 32
            && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    }
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
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
pub struct DockerVolumeId {
    pub machine_id: MachineId,
    pub name: DockerVolumeName,
}

impl fmt::Display for DockerVolumeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} on Machine {}",
            self.name.as_str().escape_debug(),
            self.machine_id
        )
    }
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

/// Docker label written on resources Ployz manages.
pub const MANAGED_LABEL: &str = "ployz.managed";
/// Docker label recording the owning Project.
pub const PROJECT_NAME_LABEL: &str = "ployz.project.name";

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

    /// Physical Docker Volume name for a declared volume owned by this Project.
    #[must_use]
    pub fn volume_name(&self, logical: &DockerVolumeName) -> DockerVolumeName {
        DockerVolumeName::parse(format!("{self}_{logical}"))
            .expect("a Project Name and Docker Volume name are each non-empty")
    }
}

/// Logical Service identity: Project Name plus Service Name, written `project/name`.
///
/// A Service ID is a separate opaque deployment identity that survives updates.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
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

    /// Infrastructure Ingress Proxy in the reserved Project.
    #[must_use]
    pub fn system_ingress() -> Self {
        Self::new(
            ProjectName::system(),
            ServiceName::parse("ingress").expect("ingress is a DNS-label Service Name"),
        )
    }
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

impl From<&QualifiedService> for ServiceSelector {
    fn from(value: &QualifiedService) -> Self {
        Self(value.to_string())
    }
}
validated_string_newtype!(
    /// A validated HTTP ingress hostname. It is not a Machine Name.
    IngressHost,
    "Ingress Hostname",
    "a 1-253 character lowercase DNS hostname",
    |value| is_hostname(value)
);

validated_string_newtype!(
    /// Name that Certificate Material is held under: an explicit Ingress Hostname
    /// or a single-level wildcard `*.x`.
    CertificateHost,
    "certificate hostname",
    "a lowercase DNS hostname or a single-level wildcard such as *.example.com",
    |value| is_hostname(value) || value.strip_prefix("*.").is_some_and(is_hostname)
);

impl CertificateHost {
    /// Whether this is a single-level wildcard `*.x`.
    #[must_use]
    pub fn is_wildcard(&self) -> bool {
        self.0.starts_with("*.")
    }

    /// Whether material held under this name serves `hostname`: the same name,
    /// or exactly one label under a wildcard's parent.
    #[must_use]
    pub fn covers(&self, hostname: &IngressHost) -> bool {
        match self.0.strip_prefix("*.") {
            Some(parent) => hostname
                .as_str()
                .split_once('.')
                .is_some_and(|(_, rest)| rest == parent),
            None => self.0 == hostname.as_str(),
        }
    }
}

impl From<IngressHost> for CertificateHost {
    fn from(hostname: IngressHost) -> Self {
        Self(hostname.0)
    }
}

impl std::borrow::Borrow<str> for CertificateHost {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

/// One Machine's optimistic container subnet candidate.
///
/// A Machine Subnet is always an IPv4 `/24`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, TS)]
#[serde(transparent)]
pub struct ContainerAddress(pub Ipv4Addr);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, TS)]
#[serde(transparent)]
pub struct AdvertisedEndpoint(pub SocketAddr);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, TS)]
#[serde(transparent)]
pub struct SelectedEndpoint(pub SocketAddr);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, TS)]
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

/// Cloud's bearer that authorizes founding and joining of one Cluster.
///
/// It is not a Pairing Credential.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CloudEnrollToken(String);

impl CloudEnrollToken {
    /// Parse a non-empty Cloud Enroll Token.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when `value` is empty.
    pub fn parse(value: impl Into<String>) -> Result<Self, ValueError> {
        let value = value.into();
        if value.is_empty() {
            Err(ValueError::new(
                "Cloud Enroll Token",
                value,
                "a non-empty bearer",
            ))
        } else {
            Ok(Self(value))
        }
    }

    /// Cloud Enroll Token bearer. Do not log this value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CloudEnrollToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CloudEnrollToken(..)")
    }
}

impl TryFrom<String> for CloudEnrollToken {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<CloudEnrollToken> for String {
    fn from(value: CloudEnrollToken) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_build_grant_repository_is_a_short_docker_repository_path() {
        assert!(super::BuildGrantRepository::parse("ployz-build/web").is_ok());
        for refused in ["", "registry.example:5000/web", "web:tag", "Web", "a//b"] {
            assert!(
                super::BuildGrantRepository::parse(refused).is_err(),
                "{refused}"
            );
        }
    }

    #[test]
    fn an_image_digest_is_lowercase_sha256() {
        let digest = super::ImageDigest::parse(format!("sha256:{}", "a".repeat(64))).unwrap();
        assert_eq!(digest.hex(), "a".repeat(64));
        for refused in [
            "a".repeat(64),
            format!("sha256:{}", "A".repeat(64)),
            "sha256:abc".into(),
        ] {
            assert!(
                super::ImageDigest::parse(refused.clone()).is_err(),
                "{refused}"
            );
        }
    }

    use super::*;

    #[test]
    fn labels_are_a_lowercase_letter_then_up_to_31_label_characters() {
        let longest = format!("a{}", "-9z".repeat(10) + "b");
        assert_eq!(longest.len(), 32);
        for valid in ["cloud", "a", "cli-2", longest.as_str()] {
            assert_eq!(ManagementClientLabel::parse(valid).unwrap().as_str(), valid);
        }
        let too_long = format!("{longest}c");
        for invalid in [
            "",
            "2cloud",
            "-cloud",
            "Cloud",
            "cl_oud",
            "cl oud",
            "clöud",
            too_long.as_str(),
        ] {
            assert!(ManagementClientLabel::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn cloud_enroll_token_rejects_empty_and_redacts_debug() {
        assert!(CloudEnrollToken::parse("").is_err());
        let token = CloudEnrollToken::parse("pmet_test").unwrap();
        assert_eq!(token.as_str(), "pmet_test");
        assert_eq!(format!("{token:?}"), "CloudEnrollToken(..)");
        assert!(!format!("{token:?}").contains("pmet_test"));
    }
}

validated_string_newtype!(
    /// Caller-assigned correlation for Containers created by one deployment attempt.
    DeploymentLogId, "deployment log ID", "1..128 ASCII letters, digits, hyphens or underscores", |value|
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
);
