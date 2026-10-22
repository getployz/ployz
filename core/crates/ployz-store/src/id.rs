//! Identities minted by callers, and the names Projects and Environments are addressed by.

use std::fmt;

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error;

// Not core's `validated_string_newtype!`: that one derives `TS` and returns a
// `ValueError` that quotes the rejected value, and the Store never echoes input.
macro_rules! store_string {
    ($(#[$doc:meta])* $name:ident, $what:literal, $valid:expr $(, normalize = $normalize:expr)?) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
        #[serde(try_from = "String", into = "String")]
        #[ts(as = "String")]
        pub struct $name(String);

        impl $name {
            /// Accept `value` if it is well formed.
            ///
            /// # Errors
            /// Returns `invalid_argument` naming what was expected, never the value.
            pub fn parse(value: impl Into<String>) -> Result<Self, RpcError> {
                let value = value.into();
                $(let value = $normalize(value);)?
                if $valid(value.as_str()) {
                    Ok(Self(value))
                } else {
                    Err(error::invalid(
                        concat!("Expected ", $what),
                        serde_json::Value::Null,
                    ))
                }
            }

            /// The value as `parse` kept it, after any normalizing.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl TryFrom<String> for $name {
            type Error = RpcError;

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

/// Caller-minted IDs are UUIDs, as authored documents require.
fn is_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok()
}

/// Organization IDs come from Cloud's accounts: 1–64 ASCII letters, digits, `-` or `_`.
fn is_id(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Who acts, as Cloud names them: 1–255 characters, none a control character.
fn is_principal(value: &str) -> bool {
    (1..=255).contains(&value.chars().count()) && !value.chars().any(char::is_control)
}

/// A public hostname: a lowercase DNS name of at least two labels.
fn is_hostname(value: &str) -> bool {
    value.contains('.') && ployz_core::IngressHost::parse(value).is_ok()
}

fn is_name(value: &str) -> bool {
    ployz_core::ServiceName::parse(value).is_ok()
}

store_string!(
    /// GitHub's `updated_at`, like `2026-09-29T10:00:00Z`: fixed-width UTC, so it
    /// orders as text and an older result never replaces a newer one.
    GithubTimestamp, "GitHub's updated_at, like 2026-09-29T10:00:00Z", is_github_timestamp
);

/// GitHub's fixed-width UTC timestamps.
fn is_github_timestamp(value: &str) -> bool {
    value.len() == 20
        && value.bytes().enumerate().all(|(index, byte)| match index {
            4 | 7 => byte == b'-',
            10 => byte == b'T',
            13 | 16 => byte == b':',
            19 => byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
}

store_string!(
    /// The Organization that owns a Config Store's rows; every read and write is scoped to one.
    OrganizationId, "an Organization ID", is_id
);
store_string!(
    /// A Project's durable identity, minted by the caller that creates it.
    ProjectId, "a Project ID (a UUID)", is_uuid
);
store_string!(
    /// An Environment's durable identity, minted by the caller that creates it.
    EnvironmentId, "an Environment ID (a UUID)", is_uuid
);
store_string!(
    /// A Service's durable identity, minted by the caller that creates it. It is
    /// also the lineage of the Service it creates.
    ServiceLineageId, "a Service ID (a UUID)", is_uuid
);
store_string!(
    /// A Volume's durable identity, minted by the caller that creates it. It is also
    /// the lineage of the Volume it creates.
    VolumeId, "a Volume ID (a UUID)", is_uuid
);
store_string!(
    /// A Config's durable identity, minted by the caller that creates it. It is also
    /// the lineage of the Config it creates.
    ConfigId, "a Config ID (a UUID)", is_uuid
);
store_string!(
    /// A Volume's name, unique in its Environment: a lowercase DNS label.
    VolumeName, "a Volume name: up to 63 lowercase letters, digits and -, starting and ending with a letter or digit", is_name
);
store_string!(
    /// A Sync's identity, minted by the Store when it lands: what Undo names.
    SyncId, "a Sync ID (a UUID)", is_uuid
);
store_string!(
    /// A removal's identity, minted by the Store when it writes the mark: what
    /// reads where the removal ended once its target is gone.
    RemovalId, "a removal ID (a UUID)", is_uuid
);
store_string!(
    /// A proposal's identity: one source included in a destination's draft, minted by
    /// the Store when it is first included.
    ProposalId, "a proposal ID (a UUID)", is_uuid
);
store_string!(
    /// A Deployment's durable identity, minted by the caller that admits it.
    DeploymentId, "a Deployment ID (a UUID)", is_uuid
);
store_string!(
    /// The durable identity of the runner that claims a Deployment and records what it did.
    RunnerId, "a runner ID of 1-64 letters, digits, - or _", is_id
);
store_string!(
    /// A Project's name, unique in its Organization: a lowercase DNS label.
    ProjectName, "a Project name: up to 63 lowercase letters, digits and -, starting and ending with a letter or digit", is_name
);
store_string!(
    /// An Environment's name, unique in its Project: a lowercase DNS label.
    EnvironmentName, "an Environment name: up to 63 lowercase letters, digits and -, starting and ending with a letter or digit", is_name
);

store_string!(
    /// Who acts, as Cloud authenticated them: a member's name or an access token's.
    /// Deployments record who admitted them by it.
    Principal, "a principal of 1-255 characters", is_principal
);

store_string!(
    /// A public hostname, like `app.example.com`: at least two labels, stored
    /// trimmed, lowercase and in punycode whatever form the member typed.
    Hostname, "a hostname like app.example.com", is_hostname, normalize = normalize_hostname
);

impl Hostname {
    /// Parse a newly added custom domain, keeping stored hostname readers compatible.
    ///
    /// # Errors
    /// Returns `invalid_argument` explaining the invalid part, without echoing input.
    pub fn parse_custom(value: &str) -> Result<Self, RpcError> {
        let invalid = |message| error::invalid(message, serde_json::Value::Null);
        let value = value.trim();
        if value.contains("://") {
            return Err(invalid("Drop the URL scheme. Enter only the hostname."));
        }
        if value.contains('*') {
            return Err(invalid(
                "Wildcard domains are not supported. Add each hostname.",
            ));
        }
        let address = value.strip_suffix('.').unwrap_or(value);
        let address = address
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(address);
        if address.parse::<std::net::IpAddr>().is_ok() {
            return Err(invalid(
                "An IP address cannot be a custom domain. Enter a hostname you own.",
            ));
        }
        if value.contains(['/', '\\']) {
            return Err(invalid("Drop the URL path. Enter only the hostname."));
        }
        if value.contains(':') {
            return Err(invalid(
                "Drop the port. Set the target port with the domain's port option.",
            ));
        }
        if value.contains('_') {
            return Err(invalid(
                "A hostname cannot contain an underscore. Use letters, digits or inner hyphens.",
            ));
        }
        if value.contains(['?', '#']) {
            return Err(invalid(
                "Drop the URL query or fragment. Enter only the hostname.",
            ));
        }
        if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(invalid(
                "A hostname cannot contain whitespace or control characters.",
            ));
        }
        let value = idna::domain_to_ascii(value).map_err(|_| {
            if value
                .split('.')
                .any(|label| label.to_ascii_lowercase().starts_with("xn--"))
            {
                invalid("The hostname contains an invalid punycode A-label.")
            } else {
                invalid("The hostname contains invalid internationalized domain characters.")
            }
        })?;
        let value = value.strip_suffix('.').unwrap_or(&value);
        if value.len() > 253 {
            return Err(invalid(
                "The hostname is too long. Use at most 253 ASCII characters after IDNA conversion.",
            ));
        }
        if value.split('.').any(|label| label.len() > 63) {
            return Err(invalid(
                "A hostname label is too long. Use at most 63 ASCII characters per label after IDNA conversion.",
            ));
        }
        if value.parse::<std::net::IpAddr>().is_ok() {
            return Err(invalid(
                "An IP address cannot be a custom domain. Enter a hostname you own.",
            ));
        }
        let hostname = Self::parse(value).map_err(|_| invalid("A hostname needs at least two nonempty DNS labels with letters, digits or inner hyphens."))?;
        if value == "ployz.app" || value.ends_with(".ployz.app") {
            return Err(invalid(
                "The ployz.app zone is reserved for Ployz-generated domains. Use a domain you own.",
            ));
        }
        Ok(hostname)
    }
}

/// DNS ignores case and carries Unicode names as punycode, so `Acme.COM ` names
/// `acme.com` and `пример.рф` names `xn--e1afmkfd.xn--p1ai`. A name IDNA refuses
/// keeps the ASCII-lowercase form it had before IDNA: saved routes and prefixes
/// like `xn--zz` stay addressable, and `is_hostname` still refuses anything
/// that is not a DNS name.
pub(crate) fn normalize_hostname(value: String) -> String {
    let value = value.trim();
    idna::domain_to_ascii(value).unwrap_or_else(|_| value.to_ascii_lowercase())
}

/// A full lowercase Git commit.
fn is_commit(value: &str) -> bool {
    ployz_core::is_lower_hex(value, 40)
}

/// A GitHub repository as `owner/name`.
fn is_repository(name: &str) -> bool {
    let Some((owner, repository)) = name.split_once('/') else {
        return false;
    };
    let fits = |part: &str, extra: &[u8]| {
        !part.is_empty()
            && part
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || extra.contains(&c))
    };
    name.len() <= ployz_core::config::REPOSITORY_MAX
        && fits(owner, b"")
        && fits(repository, b"_.")
        && !matches!(repository, "." | "..")
}

/// A Git branch name, as Git allows one.
fn is_branch(branch: &str) -> bool {
    !branch.is_empty()
        && branch.chars().count() <= ployz_core::config::BRANCH_MAX
        && !branch.contains("..")
        && !branch.starts_with(['-', '/'])
        && !branch.ends_with(['/', '.'])
        && branch
            .chars()
            .all(|c| !c.is_whitespace() && !c.is_control() && !"~^:?*[\\".contains(c))
}

store_string!(
    /// A full lowercase Git commit, as GitHub names one.
    CommitSha, "a full lowercase Git commit", is_commit
);
store_string!(
    /// A GitHub repository, as `owner/name`.
    RepositoryName, "a GitHub repository as owner/name", is_repository
);
store_string!(
    /// A Git branch name: at most `BRANCH_MAX` characters, with no whitespace, control
    /// characters, `..` or any of `~^:?*[\`, and no leading `-` or `/` or trailing `/` or `.`.
    BranchName, "a Git branch name", is_branch
);
store_string!(
    /// `version:digest` binding an Approval to one review: the version pins the
    /// revisions reviewed, the digest the destructive set a human saw.
    ApprovalDigest, "an approval as version:digest", is_approval
);

fn is_approval(value: &str) -> bool {
    value.rsplit_once(':').is_some_and(|(version, digest)| {
        !version.is_empty() && digest.len() == 16 && digest.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

// A GitHub number: what JavaScript holds exactly, so 1 to 2^53 - 1.
macro_rules! github_number {
    ($(#[$doc:meta])* $name:ident, $what:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
        #[serde(try_from = "u64", into = "u64")]
        #[ts(type = "number")]
        pub struct $name(u64);

        impl $name {
            /// Accept `value` if GitHub could have minted it.
            ///
            /// # Errors
            /// Returns `invalid_argument` naming what was expected.
            pub fn parse(value: u64) -> Result<Self, RpcError> {
                if (1..=9_007_199_254_740_991).contains(&value) {
                    Ok(Self(value))
                } else {
                    Err(error::invalid(concat!("Expected ", $what), serde_json::Value::Null))
                }
            }

            /// The number, from 1 to 2^53 - 1.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl TryFrom<u64> for $name {
            type Error = RpcError;

            fn try_from(value: u64) -> Result<Self, Self::Error> {
                Self::parse(value)
            }
        }

        impl From<$name> for u64 {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl From<$name> for crate::storage::Param<'_> {
            fn from(value: $name) -> Self {
                // At most 2^53 - 1, so it fits.
                Self::Int(i64::try_from(value.0).unwrap_or(i64::MAX))
            }
        }
    };
}

github_number!(
    /// GitHub's ID for a repository.
    RepositoryId, "a GitHub repository ID"
);
github_number!(
    /// A pull request's number in its repository.
    PullRequestNumber, "a pull request number"
);
github_number!(
    /// GitHub's ID for a user account, which a rename keeps.
    GithubUserId, "a GitHub user ID"
);

/// Working State's revision: it advances by one with every write that changes it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(transparent)]
pub struct Revision(pub u64);

impl From<u64> for Revision {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl Revision {
    /// The revision a change to Working State at this one produces.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for Revision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// One Config by name, or by exact identity as `@UUID`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
pub enum ConfigRef {
    /// The Working Config with this name, or its deleted Head predecessor.
    Name(ployz_core::ConfigName),
    /// One exact Config identity, including a deleted Head Config.
    Id(ConfigId),
}

impl ConfigRef {
    /// Parse a Config name or `@UUID`.
    ///
    /// # Errors
    /// Returns `invalid_argument` for malformed selectors, never echoing them.
    pub fn parse(value: &str) -> Result<Self, RpcError> {
        match value.strip_prefix('@') {
            Some(id) => ConfigId::parse(id).map(Self::Id),
            None => crate::settings::config_name(value).map(Self::Name),
        }
    }

    /// Resolve one identity across the states a restore addresses.
    pub(crate) fn resolve<'a>(
        &self,
        intents: impl IntoIterator<Item = &'a ployz_core::config::SavedEnvironmentIntent>,
    ) -> Result<&'a ployz_core::config::SavedConfigIntent, RpcError> {
        let matching: Vec<_> = intents
            .into_iter()
            .flat_map(|intent| &intent.configs)
            .filter(|config| self.matches(config))
            .collect();
        let first = matching.first().copied().ok_or_else(|| {
            crate::error::not_found(
                "No Config matches this selector.",
                serde_json::json!({ "config": self }),
            )
        })?;
        if matching
            .iter()
            .any(|config| config.resource_id != first.resource_id)
        {
            let valid: std::collections::BTreeSet<_> = matching
                .iter()
                .map(|config| format!("configs.@{}", config.resource_id))
                .collect();
            return Err(crate::error::ambiguous(
                "More than one Config has this name.",
                serde_json::json!({ "config": self, "valid_children": valid }),
            ));
        }
        Ok(first)
    }

    pub(crate) fn matches(&self, config: &ployz_core::config::SavedConfigIntent) -> bool {
        match self {
            Self::Name(name) => config.name == *name,
            Self::Id(id) => config.resource_id == id.as_str(),
        }
    }
}

impl fmt::Display for ConfigRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name) => name.fmt(formatter),
            Self::Id(id) => write!(formatter, "@{id}"),
        }
    }
}

impl TryFrom<String> for ConfigRef {
    type Error = RpcError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<ConfigRef> for String {
    fn from(value: ConfigRef) -> Self {
        value.to_string()
    }
}

impl From<ployz_core::ConfigName> for ConfigRef {
    fn from(name: ployz_core::ConfigName) -> Self {
        Self::Name(name)
    }
}

impl From<ConfigId> for ConfigRef {
    fn from(id: ConfigId) -> Self {
        Self::Id(id)
    }
}

#[cfg(test)]
mod config_ref_tests {
    use super::{ConfigId, ConfigRef};
    use ployz_core::ConfigName;

    #[test]
    fn exact_config_identity_cannot_be_confused_with_a_uuid_name() {
        let uuid = "00000000-0000-4000-8000-000000000009";
        assert_eq!(
            ConfigRef::parse(uuid).unwrap(),
            ConfigRef::Name(ConfigName::parse(uuid).unwrap())
        );
        let exact = ConfigRef::parse(&format!("@{uuid}")).unwrap();
        assert_eq!(exact, ConfigRef::Id(ConfigId::parse(uuid).unwrap()));
        assert_eq!(
            serde_json::from_value::<ConfigRef>(serde_json::to_value(&exact).unwrap()).unwrap(),
            exact
        );
        assert!(ConfigRef::parse("@sentry").is_err());
        assert!(ConfigName::parse(format!("@{uuid}")).is_err());
    }
}

#[cfg(test)]
mod hostname_tests {
    use super::Hostname;

    #[test]
    fn a_hostname_in_any_case_is_stored_lowercase() {
        assert_eq!(Hostname::parse(" Acme.COM ").unwrap().as_str(), "acme.com");
        let sent: Hostname = serde_json::from_value(serde_json::json!("App.Example.com")).unwrap();
        assert_eq!(sent.as_str(), "app.example.com");
        assert!(Hostname::parse("acme..com").is_err());
        assert!(Hostname::parse("ACME").is_err());
    }
}
