use serde::{Deserialize, Deserializer, Serialize, de};
use url::Url;

use crate::ValueError;

/// A syntactically usable HTTP(S) Cloud Relay base URL, not proof of availability.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RelayEndpoint(Url);

impl RelayEndpoint {
    /// Admit a Relay URL without contacting it. Authentication uses separate credentials.
    ///
    /// # Errors
    /// Returns [`ValueError`] for an invalid HTTP(S) URL, embedded credentials,
    /// query, fragment, or zero port.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ValueError> {
        let value = value.as_ref();
        let invalid = || {
            ValueError::new(
                "Relay endpoint",
                value,
                "an HTTP(S) URL with a host, nonzero port, and no credentials, query or fragment",
            )
        };
        let url = Url::parse(value).map_err(|_| invalid())?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.has_host()
            || url.port() == Some(0)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid());
        }
        Ok(Self(url))
    }

    #[must_use]
    pub fn as_url(&self) -> &Url {
        &self.0
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Display for RelayEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RelayEndpoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}
