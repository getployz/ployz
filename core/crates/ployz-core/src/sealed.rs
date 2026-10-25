use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::ValueError;
#[cfg(not(target_arch = "wasm32"))]
pub use sealer::*;

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

/// Seals a value to a Cluster's Sealing Key the way a Machine opens it: HPKE base
/// mode over X25519, HKDF-SHA256 and ChaCha20-Poly1305, bound by
/// [`SealedValue::associated_data`] to the variable, Service, Environment and
/// Organization it was authored as. The CLI and the Store seal with this; ployzd
/// opens.
#[cfg(not(target_arch = "wasm32"))]
mod sealer {
    use hpke::{
        Deserializable as _, OpModeS, Serializable as _, aead::ChaCha20Poly1305, kdf::HkdfSha256,
        kem::X25519HkdfSha256,
    };
    use rand_core::{OsRng, TryRngCore as _};
    use thiserror::Error;

    use super::{Ciphertext, SEALED_VALUE_INFO, SealedIdentity, SealedScope, SealedValue};
    use crate::SealingPublicKey;
    use crate::config::ClientSealedValue;

    type Kem = X25519HkdfSha256;

    /// Why a value could not be sealed.
    #[derive(Debug, Error)]
    pub enum SealError {
        #[error("the Sealing Key is not an X25519 public key")]
        Recipient,
        #[error("sealing failed")]
        Seal,
    }

    /// Seals `plaintext` as the variable `key` of `identity`'s Service, to `recipient`.
    ///
    /// # Errors
    /// [`SealError`] when `recipient` is not a usable public key or HPKE refuses.
    pub fn seal_value(
        recipient: &SealingPublicKey,
        identity: &SealedIdentity,
        key: &str,
        plaintext: &str,
    ) -> Result<ClientSealedValue, SealError> {
        let recipient = <Kem as hpke::Kem>::PublicKey::from_bytes(recipient.as_bytes())
            .map_err(|_| SealError::Recipient)?;
        let mut value = SealedValue {
            sealed: Ciphertext::from_bytes(Vec::new()),
            scope: SealedScope::Service(identity.service),
            key: key.to_owned(),
        };
        let aad = value.associated_data(identity);
        let (encapped, ciphertext) =
            hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem, _>(
                &OpModeS::Base,
                &recipient,
                SEALED_VALUE_INFO,
                plaintext.as_bytes(),
                &aad,
                &mut OsRng.unwrap_err(),
            )
            .map_err(|_| SealError::Seal)?;
        let mut sealed = encapped.to_bytes().to_vec();
        sealed.extend(ciphertext);
        value.sealed = Ciphertext::from_bytes(sealed);
        Ok(ClientSealedValue {
            sealed_for: identity.environment,
            value,
        })
    }

    /// A throwaway Sealing Key for tests that open what [`seal_value`] sealed, as a
    /// Machine does. Never a real Cluster's key.
    #[cfg(any(test, feature = "test-keypair"))]
    pub struct Keypair {
        private: <Kem as hpke::Kem>::PrivateKey,
        public: SealingPublicKey,
    }

    #[cfg(any(test, feature = "test-keypair"))]
    impl Keypair {
        const ENCAPPED_KEY_LEN: usize = 32;

        #[must_use]
        pub fn generate() -> Self {
            let (private, public) = <Kem as hpke::Kem>::gen_keypair(&mut OsRng.unwrap_err());
            Self {
                private,
                public: SealingPublicKey::from_bytes(public.to_bytes().into()),
            }
        }

        #[must_use]
        pub fn public(&self) -> SealingPublicKey {
            self.public
        }

        /// The plaintext of `sealed` as `identity` authored it, or `None` when it was
        /// sealed to another key or another identity.
        #[must_use]
        pub fn open(
            &self,
            identity: &SealedIdentity,
            sealed: &ClientSealedValue,
        ) -> Option<String> {
            let aad = sealed.value.associated_data(identity);
            let (encapped, ciphertext) = sealed
                .value
                .sealed
                .as_bytes()
                .split_at_checked(Self::ENCAPPED_KEY_LEN)?;
            let encapped = <Kem as hpke::Kem>::EncappedKey::from_bytes(encapped).ok()?;
            let plaintext = hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
                &hpke::OpModeR::Base,
                &self.private,
                &encapped,
                SEALED_VALUE_INFO,
                ciphertext,
                &aad,
            )
            .ok()?;
            String::from_utf8(plaintext).ok()
        }
    }

    #[cfg(test)]
    mod tests {
        use uuid::Uuid;

        use super::{Keypair, seal_value};
        use crate::{SealedIdentity, SealedScope};

        fn authored() -> SealedIdentity {
            SealedIdentity {
                organization: "org_1".into(),
                environment: Uuid::from_u128(2),
                service: Uuid::from_u128(3),
            }
        }

        #[test]
        fn a_sealed_value_opens_only_as_it_was_authored() {
            let keypair = Keypair::generate();
            let identity = authored();
            let sealed = seal_value(&keypair.public(), &identity, "TOKEN", "hunter2").unwrap();

            assert_eq!(sealed.sealed_for, identity.environment);
            assert_eq!(sealed.value.key, "TOKEN");
            assert_eq!(sealed.value.scope, SealedScope::Service(identity.service));
            assert_eq!(keypair.open(&identity, &sealed).as_deref(), Some("hunter2"));

            let other_organization = SealedIdentity {
                organization: "org_2".into(),
                ..identity.clone()
            };
            assert_eq!(keypair.open(&other_organization, &sealed), None);
            let other_environment = SealedIdentity {
                environment: Uuid::from_u128(4),
                ..identity
            };
            assert_eq!(keypair.open(&other_environment, &sealed), None);
            assert_eq!(Keypair::generate().open(&authored(), &sealed), None);
            let encoded = serde_json::to_string(&sealed).unwrap();
            assert!(!encoded.contains("hunter2"));
        }

        #[test]
        fn a_value_moved_to_another_variable_or_service_does_not_open() {
            let keypair = Keypair::generate();
            let sealed = seal_value(&keypair.public(), &authored(), "TOKEN", "hunter2").unwrap();

            let mut renamed = sealed.clone();
            renamed.value.key = "OTHER".into();
            assert_eq!(keypair.open(&authored(), &renamed), None);

            let mut moved = sealed;
            moved.value.scope = SealedScope::Service(Uuid::from_u128(4));
            assert_eq!(keypair.open(&authored(), &moved), None);
        }
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
