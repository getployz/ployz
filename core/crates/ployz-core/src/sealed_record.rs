use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use ts_rs::TS;
use zeroize::Zeroizing;

use crate::{EnvValue, RequestedServiceSpec, SealedIdentity, scrub::SealedValues};

/// One Service's Sealed values, still sealed, with who they were sealed for.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SealedRecord {
    identity: SealedIdentity,
    values: BTreeMap<String, EnvValue>,
}

/// A Service reads a Sealed value but its Deploy names no Environment to open it in.
#[derive(Debug, Error)]
#[error("a sealed value needs the Deploy's Environment")]
pub struct MissingSealedIdentity;

impl SealedRecord {
    #[must_use]
    pub const fn new(identity: SealedIdentity, values: BTreeMap<String, EnvValue>) -> Self {
        Self { identity, values }
    }

    /// The Sealed values `service` reads, or `None` when it reads none.
    ///
    /// # Errors
    /// Fails when `service` reads one without a Sealed identity.
    pub fn of(service: &RequestedServiceSpec) -> Result<Option<Self>, MissingSealedIdentity> {
        let values: BTreeMap<String, EnvValue> = service
            .container
            .environment
            .iter()
            .filter(|(_, value)| value.is_sealed())
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        if values.is_empty() {
            return Ok(None);
        }
        let identity = service
            .container
            .sealed_identity
            .clone()
            .ok_or(MissingSealedIdentity)?;
        Ok(Some(Self { identity, values }))
    }

    #[must_use]
    pub const fn identity(&self) -> &SealedIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn values(&self) -> &BTreeMap<String, EnvValue> {
        &self.values
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }
}

/// A record's values opened for one Build: each whole by name, and each Sealed part a
/// Template joined, which a scrub hides too. Its Debug prints names only, and it
/// serializes only as the body a Build Grant's holder reads.
pub struct OpenedSealed {
    values: BTreeMap<String, Zeroizing<String>>,
    parts: Vec<(String, Zeroizing<String>)>,
}

/// The holder's Build reads a Sealed value its grant doesn't open.
#[derive(Debug, Error)]
#[error("the Build Grant does not open {0}")]
pub struct MissingSealed(pub String);

impl OpenedSealed {
    #[must_use]
    pub fn new(values: BTreeMap<String, String>, parts: Vec<(String, String)>) -> Self {
        Self {
            values: values
                .into_iter()
                .map(|(name, value)| (name, Zeroizing::new(value)))
                .collect(),
            parts: parts
                .into_iter()
                .map(|(name, value)| (name, Zeroizing::new(value)))
                .collect(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(|value| value.as_str())
    }

    pub fn values(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    /// Every form a scrub hides: each value whole and each Sealed part.
    #[must_use]
    pub fn secrets(&self) -> SealedValues {
        self.values
            .iter()
            .chain(self.parts.iter().map(|(name, value)| (name, value)))
            .map(|(name, value)| (name.clone(), value.as_str().to_owned()))
            .collect()
    }

    /// Refuse a set that lacks one of `names`, the values a Build reads.
    ///
    /// # Errors
    /// Names the first value missing.
    pub fn covering<'name>(
        self,
        mut names: impl Iterator<Item = &'name str>,
    ) -> Result<Self, MissingSealed> {
        match names.find(|name| !self.values.contains_key(*name)) {
            Some(name) => Err(MissingSealed(name.to_owned())),
            None => Ok(self),
        }
    }
}

impl fmt::Debug for OpenedSealed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.values.keys()).finish()
    }
}

#[derive(Serialize)]
struct WireRef<'a> {
    values: BTreeMap<&'a str, &'a str>,
    parts: Vec<(&'a str, &'a str)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    values: BTreeMap<String, String>,
    parts: Vec<(String, String)>,
}

impl Serialize for OpenedSealed {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        WireRef {
            values: self.values().collect(),
            parts: self
                .parts
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for OpenedSealed {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let Wire { values, parts } = Wire::deserialize(deserializer)?;
        Ok(Self::new(values, parts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened() -> OpenedSealed {
        OpenedSealed::new(
            BTreeMap::from([
                ("NPM_TOKEN".into(), "marker-token".into()),
                ("URL".into(), "https://u:marker-part@host".into()),
            ]),
            vec![("PASSWORD".into(), "marker-part".into())],
        )
    }

    #[test]
    fn debug_names_values_and_shows_none() {
        let debug = format!("{:?}", opened());
        assert_eq!(debug, r#"["NPM_TOKEN", "URL"]"#);
        assert!(!debug.contains("marker"));
    }

    #[test]
    fn the_wire_body_round_trips_every_form() {
        let body = serde_json::to_string(&opened()).unwrap();
        let back: OpenedSealed = serde_json::from_str(&body).unwrap();
        assert_eq!(back.get("NPM_TOKEN"), Some("marker-token"));
        assert_eq!(
            format!("{:?}", back.secrets()),
            format!("{:?}", opened().secrets())
        );
        assert_eq!(serde_json::to_string(&back).unwrap(), body);
    }

    #[test]
    fn a_set_covers_only_the_names_it_holds() {
        assert!(opened().covering(["NPM_TOKEN"].into_iter()).is_ok());
        let missing = opened()
            .covering(["NPM_TOKEN", "OTHER"].into_iter())
            .unwrap_err();
        assert_eq!(missing.to_string(), "the Build Grant does not open OTHER");
    }
}
