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

    /// Only the values `read` keeps, or `None` when it keeps none.
    #[must_use]
    pub fn only(mut self, read: impl Fn(&str) -> bool) -> Option<Self> {
        self.values.retain(|name, _| read(name));
        (!self.values.is_empty()).then_some(self)
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

/// Build secrets on their way from a grant Machine to a rule build: HPKE in authenticated
/// mode (RFC 9180 §5.1.3) over X25519, HKDF-SHA256 and ChaCha20-Poly1305. The sender is the
/// Cluster's attest key, which only a Machine holding the Sealing Key can derive, and the
/// info binds one grant audience. So the builder opens only values a Cluster Machine sealed
/// for exactly this build.
#[cfg(not(target_arch = "wasm32"))]
mod build_secrets {
    use hpke::{
        Deserializable as _, Kem as _, OpModeR, OpModeS, Serializable as _, aead::ChaCha20Poly1305,
        kdf::HkdfSha256, kem::X25519HkdfSha256,
    };
    use rand_core::{OsRng, TryRngCore as _};
    use thiserror::Error;
    use zeroize::Zeroizing;

    use super::OpenedSealed;
    use crate::authority::{AttestKey, RecipientKey};

    type Kem = X25519HkdfSha256;
    const ENCAPPED_KEY_LEN: usize = 32;

    /// What every build-secrets seal is bound under, followed by the grant's audience.
    pub const BUILD_SECRETS_INFO: &[u8] = b"ployz build secrets v1";

    /// An X25519 private key: a build's recipient key, or a Machine's attest key.
    pub struct X25519Secret(Zeroizing<[u8; 32]>);

    impl X25519Secret {
        #[must_use]
        pub fn generate() -> Self {
            let (secret, _) = Kem::gen_keypair(&mut OsRng.unwrap_err());
            Self::from_bytes(
                secret
                    .to_bytes()
                    .as_slice()
                    .try_into()
                    .expect("an X25519 private key is 32 bytes"),
            )
        }

        #[must_use]
        pub fn from_bytes(bytes: [u8; 32]) -> Self {
            Self(Zeroizing::new(bytes))
        }

        fn private(&self) -> <Kem as hpke::Kem>::PrivateKey {
            <Kem as hpke::Kem>::PrivateKey::from_bytes(self.0.as_slice())
                .expect("an X25519 private key is any 32 bytes")
        }

        fn public(&self) -> [u8; 32] {
            Kem::sk_to_pk(&self.private())
                .to_bytes()
                .as_slice()
                .try_into()
                .expect("an X25519 public key is 32 bytes")
        }

        /// This key's public half, as a build names its recipient.
        #[must_use]
        pub fn recipient(&self) -> RecipientKey {
            RecipientKey::from_bytes(self.public())
        }

        /// This key's public half, as a Cluster names its attest key.
        #[must_use]
        pub fn attest(&self) -> AttestKey {
            AttestKey::from_bytes(self.public())
        }
    }

    impl std::fmt::Debug for X25519Secret {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("X25519Secret(..)")
        }
    }

    /// Why build secrets could not be sealed or opened.
    #[derive(Debug, Error)]
    pub enum BuildSecretsError {
        #[error("the build secrets could not be sealed")]
        Seal,
        #[error("the build secrets are not sealed for this build by this Cluster")]
        Unopenable,
        #[error("the opened build secrets are not a set of values")]
        Malformed,
    }

    fn info(audience: &str) -> Vec<u8> {
        let mut info = BUILD_SECRETS_INFO.to_vec();
        info.extend_from_slice(audience.as_bytes());
        info
    }

    impl OpenedSealed {
        /// Seal these values from `sender` to `recipient`, bound to `audience`: the
        /// encapsulated key, then the ciphertext.
        ///
        /// # Errors
        /// Returns [`BuildSecretsError::Seal`] when HPKE refuses.
        pub fn seal_for_build(
            &self,
            sender: &X25519Secret,
            recipient: &RecipientKey,
            audience: &str,
        ) -> Result<Vec<u8>, BuildSecretsError> {
            let recipient = <Kem as hpke::Kem>::PublicKey::from_bytes(recipient.as_bytes())
                .map_err(|_| BuildSecretsError::Seal)?;
            let sender_private = sender.private();
            let sender_public = Kem::sk_to_pk(&sender_private);
            let plaintext =
                Zeroizing::new(serde_json::to_vec(self).map_err(|_| BuildSecretsError::Seal)?);
            let (encapped, ciphertext) =
                hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem, _>(
                    &OpModeS::Auth((sender_private, sender_public)),
                    &recipient,
                    &info(audience),
                    &plaintext,
                    &[],
                    &mut OsRng.unwrap_err(),
                )
                .map_err(|_| BuildSecretsError::Seal)?;
            let mut sealed = encapped.to_bytes().to_vec();
            sealed.extend(ciphertext);
            Ok(sealed)
        }

        /// Open values sealed by [`Self::seal_for_build`] for `recipient`, only if
        /// `sender` sealed them, in authenticated mode, for `audience`.
        ///
        /// # Errors
        /// Returns [`BuildSecretsError::Unopenable`] for values sealed by another key,
        /// in base mode, for another recipient or another audience, and
        /// [`BuildSecretsError::Malformed`] when what opens is not a set of values.
        pub fn open_build(
            sealed: &[u8],
            recipient: &X25519Secret,
            sender: &AttestKey,
            audience: &str,
        ) -> Result<Self, BuildSecretsError> {
            let (encapped, ciphertext) = sealed
                .split_at_checked(ENCAPPED_KEY_LEN)
                .ok_or(BuildSecretsError::Unopenable)?;
            let encapped = <Kem as hpke::Kem>::EncappedKey::from_bytes(encapped)
                .map_err(|_| BuildSecretsError::Unopenable)?;
            let sender = <Kem as hpke::Kem>::PublicKey::from_bytes(sender.as_bytes())
                .map_err(|_| BuildSecretsError::Unopenable)?;
            let opened = Zeroizing::new(
                hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
                    &OpModeR::Auth(sender),
                    &recipient.private(),
                    &encapped,
                    &info(audience),
                    ciphertext,
                    &[],
                )
                .map_err(|_| BuildSecretsError::Unopenable)?,
            );
            serde_json::from_slice(&opened).map_err(|_| BuildSecretsError::Malformed)
        }
    }

    #[cfg(test)]
    mod tests {
        use std::collections::BTreeMap;

        use super::*;

        const AUDIENCE: &str = "ployz-build:0123";
        const MARKER: &str = "marker-build-secret-42";

        fn values() -> OpenedSealed {
            OpenedSealed::new(
                BTreeMap::from([("NPM_TOKEN".into(), MARKER.into())]),
                Vec::new(),
            )
        }

        #[test]
        fn a_build_opens_values_its_clusters_attest_key_sealed_for_it() {
            let attest = X25519Secret::generate();
            let recipient = X25519Secret::generate();
            let sealed = values()
                .seal_for_build(&attest, &recipient.recipient(), AUDIENCE)
                .unwrap();
            assert!(
                !sealed
                    .windows(MARKER.len())
                    .any(|window| window == MARKER.as_bytes())
            );
            let opened =
                OpenedSealed::open_build(&sealed, &recipient, &attest.attest(), AUDIENCE).unwrap();
            assert_eq!(opened.get("NPM_TOKEN"), Some(MARKER));
        }

        #[test]
        fn a_build_refuses_values_sealed_by_another_key() {
            let attest = X25519Secret::generate();
            let recipient = X25519Secret::generate();
            let sealed = values()
                .seal_for_build(&X25519Secret::generate(), &recipient.recipient(), AUDIENCE)
                .unwrap();
            assert!(matches!(
                OpenedSealed::open_build(&sealed, &recipient, &attest.attest(), AUDIENCE),
                Err(BuildSecretsError::Unopenable)
            ));
        }

        #[test]
        fn a_build_refuses_values_sealed_for_another_audience() {
            let attest = X25519Secret::generate();
            let recipient = X25519Secret::generate();
            let sealed = values()
                .seal_for_build(&attest, &recipient.recipient(), "ployz-build:other")
                .unwrap();
            assert!(matches!(
                OpenedSealed::open_build(&sealed, &recipient, &attest.attest(), AUDIENCE),
                Err(BuildSecretsError::Unopenable)
            ));
        }

        #[test]
        fn a_build_refuses_values_sealed_in_base_mode() {
            let attest = X25519Secret::generate();
            let recipient = X25519Secret::generate();
            let public =
                <Kem as hpke::Kem>::PublicKey::from_bytes(recipient.recipient().as_bytes())
                    .unwrap();
            let plaintext = serde_json::to_vec(&values()).unwrap();
            let (encapped, ciphertext) =
                hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem, _>(
                    &OpModeS::Base,
                    &public,
                    &info(AUDIENCE),
                    &plaintext,
                    &[],
                    &mut OsRng.unwrap_err(),
                )
                .unwrap();
            let mut sealed = encapped.to_bytes().to_vec();
            sealed.extend(ciphertext);
            assert!(matches!(
                OpenedSealed::open_build(&sealed, &recipient, &attest.attest(), AUDIENCE),
                Err(BuildSecretsError::Unopenable)
            ));
        }

        #[test]
        fn a_build_refuses_values_sealed_to_another_recipient() {
            let attest = X25519Secret::generate();
            let sealed = values()
                .seal_for_build(&attest, &X25519Secret::generate().recipient(), AUDIENCE)
                .unwrap();
            assert!(matches!(
                OpenedSealed::open_build(
                    &sealed,
                    &X25519Secret::generate(),
                    &attest.attest(),
                    AUDIENCE
                ),
                Err(BuildSecretsError::Unopenable)
            ));
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use build_secrets::{BUILD_SECRETS_INFO, BuildSecretsError, X25519Secret};

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
