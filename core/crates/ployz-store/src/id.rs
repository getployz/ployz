//! Identities minted by callers, and the names Projects and Environments are addressed by.

use std::fmt;

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error;

// Not core's `validated_string_newtype!`: that one derives `TS` and returns a
// `ValueError` that quotes the rejected value, and the Store never echoes input.
macro_rules! store_string {
    ($(#[$doc:meta])* $name:ident, $what:literal, $valid:expr) => {
        store_string!($(#[$doc])* $name, $what, $valid, "String");
    };
    ($(#[$doc:meta])* $name:ident, $what:literal, $valid:expr, $ts:literal) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
        #[serde(try_from = "String", into = "String")]
        #[ts(as = $ts)]
        pub struct $name(String);

        impl $name {
            /// Accept `value` if it is well formed.
            ///
            /// # Errors
            /// Returns `invalid_argument` naming what was expected, never the value.
            pub fn parse(value: impl Into<String>) -> Result<Self, RpcError> {
                let value = value.into();
                if $valid(value.as_str()) {
                    Ok(Self(value))
                } else {
                    Err(error::invalid(
                        concat!("Expected ", $what),
                        serde_json::Value::Null,
                    ))
                }
            }

            /// The value as text.
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

/// A public hostname: a lowercase DNS name of at least two labels.
fn is_hostname(value: &str) -> bool {
    value.contains('.') && ployz_core::IngressHost::parse(value).is_ok()
}

fn is_name(value: &str) -> bool {
    ployz_core::ServiceName::parse(value).is_ok()
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
    ServiceId, "a Service ID (a UUID)", is_uuid, "ployz_core::ServiceId"
);
store_string!(
    /// A Volume's durable identity, minted by the caller that creates it. It is also
    /// the lineage of the Volume it creates.
    VolumeId, "a Volume ID (a UUID)", is_uuid
);
store_string!(
    /// A Volume's name, unique in its Environment: a lowercase DNS label.
    VolumeName, "a Volume name: lowercase letters, digits and -", is_name
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
    ProjectName, "a Project name: lowercase letters, digits and -", is_name
);
store_string!(
    /// An Environment's name, unique in its Project: a lowercase DNS label.
    EnvironmentName, "an Environment name: lowercase letters, digits and -", is_name
);

store_string!(
    /// A public hostname, like `app.example.com`: lowercase, at least two labels.
    Hostname, "a hostname like app.example.com", is_hostname
);

/// Working State's revision: it advances by one with every write that changes it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(transparent)]
pub struct Revision(pub u64);

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
