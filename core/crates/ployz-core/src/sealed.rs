use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::ValueError;

/// HPKE `info` for env values sealed to a Cluster's Sealing Key.
pub const SEALED_VALUE_INFO: &[u8] = b"ployz-sealed-v1";

/// HPKE X25519 encapsulated key plus the ChaCha20-Poly1305 tag.
const MIN_CIPHERTEXT_LEN: usize = 32 + 16;

/// One container environment value as a Machine receives it.
///
/// Text stays a bare JSON string, so a Machine that predates Sealed values
/// rejects the object forms instead of starting a container without them.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum EnvValue {
    Text(String),
    Sealed(SealedValue),
    Template(Template),
}

/// A value assembled from literal text and Sealed parts, such as a URL with a
/// sealed password.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub template: Vec<EnvPart>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum EnvPart {
    Text(String),
    Sealed(SealedValue),
}

/// One variable's value sealed to the Cluster's Sealing Key, bound to the
/// variable it was authored as.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SealedValue {
    pub sealed: Ciphertext,
    pub scope: SealedScope,
    pub key: String,
}

/// Where the sealed variable was authored.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SealedScope {
    Shared,
    Service(#[ts(type = "string")] Uuid),
}

/// HPKE encapsulated key followed by the ciphertext.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct Ciphertext(Vec<u8>);

/// The authored Organization, Environment and Service a spec's Sealed values
/// were sealed for. The authored Service is not the runtime Service ID.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SealedIdentity {
    pub organization: String,
    #[ts(type = "string")]
    pub environment: Uuid,
    #[ts(type = "string")]
    pub service: Uuid,
}

impl EnvValue {
    /// The literal value, when it holds no Sealed part.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Sealed(_) | Self::Template(_) => None,
        }
    }

    /// Every Sealed part, in the order the value joins them.
    #[must_use]
    pub fn sealed_parts(&self) -> Vec<&SealedValue> {
        match self {
            Self::Text(_) => Vec::new(),
            Self::Sealed(value) => vec![value],
            Self::Template(template) => template
                .template
                .iter()
                .filter_map(|part| match part {
                    EnvPart::Text(_) => None,
                    EnvPart::Sealed(value) => Some(value),
                })
                .collect(),
        }
    }

    #[must_use]
    pub fn is_sealed(&self) -> bool {
        !matches!(self, Self::Text(_))
    }
}

impl From<String> for EnvValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for EnvValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl SealedValue {
    /// HPKE associated data binding this value to the variable it was authored
    /// as, in `identity`'s Organization and Environment.
    #[must_use]
    pub fn associated_data(&self, identity: &SealedIdentity) -> Vec<u8> {
        let mut aad = Vec::new();
        put(&mut aad, SEALED_VALUE_INFO);
        put(&mut aad, identity.organization.as_bytes());
        put(&mut aad, identity.environment.as_bytes());
        match self.scope {
            SealedScope::Shared => put(&mut aad, b"shared"),
            SealedScope::Service(service) => {
                put(&mut aad, b"service");
                put(&mut aad, service.as_bytes());
            }
        }
        put(&mut aad, self.key.as_bytes());
        aad
    }
}

fn put(aad: &mut Vec<u8>, field: &[u8]) {
    let len = u32::try_from(field.len()).expect("associated data field fits in u32");
    aad.extend_from_slice(&len.to_be_bytes());
    aad.extend_from_slice(field);
}

impl Ciphertext {
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Ciphertext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Ciphertext(..)")
    }
}

impl TryFrom<String> for Ciphertext {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        STANDARD
            .decode(&value)
            .ok()
            .filter(|bytes| bytes.len() >= MIN_CIPHERTEXT_LEN)
            .map(Self)
            .ok_or_else(|| ValueError::new("sealed value", value, "base64 of at least 48 bytes"))
    }
}

impl From<Ciphertext> for String {
    fn from(value: Ciphertext) -> Self {
        STANDARD.encode(value.0)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn identity() -> SealedIdentity {
        SealedIdentity {
            organization: "org_1".into(),
            environment: Uuid::from_u128(1),
            service: Uuid::from_u128(2),
        }
    }

    fn sealed(key: &str) -> SealedValue {
        SealedValue {
            sealed: Ciphertext::from_bytes(vec![7; MIN_CIPHERTEXT_LEN]),
            scope: SealedScope::Service(Uuid::from_u128(2)),
            key: key.into(),
        }
    }

    #[test]
    fn text_stays_a_bare_string_on_the_wire() {
        let env = BTreeMap::from([("A".to_owned(), EnvValue::from("b"))]);
        assert_eq!(serde_json::to_string(&env).unwrap(), r#"{"A":"b"}"#);
    }

    #[test]
    fn sealed_and_template_values_round_trip() {
        let env = BTreeMap::from([
            ("TOKEN".to_owned(), EnvValue::Sealed(sealed("TOKEN"))),
            (
                "URL".to_owned(),
                EnvValue::Template(Template {
                    template: vec![
                        EnvPart::Text("postgres://app:".into()),
                        EnvPart::Sealed(sealed("DB_PASSWORD")),
                        EnvPart::Text("@db".into()),
                    ],
                }),
            ),
        ]);
        let json = serde_json::to_string(&env).unwrap();
        let back: BTreeMap<String, EnvValue> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, env);
        let keys: Vec<_> = back
            .get("URL")
            .unwrap()
            .sealed_parts()
            .into_iter()
            .map(|part| &part.key)
            .collect();
        assert_eq!(keys, ["DB_PASSWORD"]);
    }

    #[test]
    fn a_string_map_reader_rejects_a_sealed_value() {
        let env = BTreeMap::from([("TOKEN".to_owned(), EnvValue::Sealed(sealed("TOKEN")))]);
        let json = serde_json::to_string(&env).unwrap();
        assert!(serde_json::from_str::<BTreeMap<String, String>>(&json).is_err());
    }

    #[test]
    fn short_or_unknown_sealed_shapes_are_refused() {
        let short = serde_json::json!({ "sealed": STANDARD.encode([1; 47]), "scope": "shared", "key": "A" });
        assert!(serde_json::from_value::<EnvValue>(short).is_err());
        let mut extra = serde_json::to_value(sealed("A")).unwrap();
        extra
            .as_object_mut()
            .unwrap()
            .insert("algorithm".into(), "x".into());
        assert!(serde_json::from_value::<EnvValue>(extra).is_err());
    }

    #[test]
    fn associated_data_separates_every_bound_field() {
        let value = sealed("TOKEN");
        let base = value.associated_data(&identity());
        let other_env = SealedIdentity {
            environment: Uuid::from_u128(3),
            ..identity()
        };
        let other_org = SealedIdentity {
            organization: "org_2".into(),
            ..identity()
        };
        let shared = SealedValue {
            scope: SealedScope::Shared,
            ..sealed("TOKEN")
        };
        for other in [
            value.associated_data(&other_env),
            value.associated_data(&other_org),
            sealed("TOKEM").associated_data(&identity()),
            shared.associated_data(&identity()),
        ] {
            assert_ne!(other, base);
        }
    }
}
