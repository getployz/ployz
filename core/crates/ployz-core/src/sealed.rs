use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::value::base64_newtype;
#[cfg(not(target_arch = "wasm32"))]
pub use sealer::*;

/// HPKE `info` for env values sealed to a Cluster's Sealing Key.
pub const SEALED_VALUE_INFO: &[u8] = b"ployz-sealed-v1";

/// HPKE `info` for values Ployz itself seals to a Cluster's Sealing Key for one
/// [`PlatformPurpose`], apart from any Service's env.
pub const PLATFORM_SEALED_INFO: &[u8] = b"ployz-platform-sealed-v1";

/// What a value Ployz seals to a Cluster is for. A value sealed for one purpose
/// opens for no other, and for no other Cluster.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum PlatformPurpose {
    /// The Cloudflare tunnel token the Tunnel Connector runs with.
    IngressTunnel,
    /// The Tailscale auth key the Private Access router joins the tailnet with.
    PrivateAccess,
}

impl PlatformPurpose {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IngressTunnel => "ingress-tunnel",
            Self::PrivateAccess => "private-access",
        }
    }

    /// HPKE associated data binding a value to this purpose and to the Cluster
    /// whose Sealing Key is `cluster`.
    #[must_use]
    pub fn associated_data(self, cluster: &crate::SealingPublicKey) -> Vec<u8> {
        let mut aad = Vec::new();
        put(&mut aad, PLATFORM_SEALED_INFO);
        put(&mut aad, self.as_str().as_bytes());
        put(&mut aad, cluster.as_bytes());
        aad
    }
}

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
    /// The Environment the value was sealed for, when it is not the Container's own: a
    /// parent's value a preview inherits, opened in place. A daemon that predates it
    /// refuses the field, so a preview never starts without the check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "string")]
    pub environment: Option<Uuid>,
}

/// Where the sealed variable was authored.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SealedScope {
    Shared,
    Service(#[ts(type = "string")] Uuid),
}

/// A Sealed value the browser sealed to one Environment's Sealing Key, bound to
/// the variable it was authored as. Cloud carries it and never opens it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientSealedValue {
    /// The Environment whose Sealing Key opens it: in any other it is Not set.
    #[ts(type = "string")]
    pub sealed_for: Uuid,
    pub value: SealedValue,
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
    /// as, in `identity`'s Organization and in its own Environment: the one it names, or
    /// else `identity`'s.
    #[must_use]
    pub fn associated_data(&self, identity: &SealedIdentity) -> Vec<u8> {
        let mut aad = Vec::new();
        put(&mut aad, SEALED_VALUE_INFO);
        put(&mut aad, identity.organization.as_bytes());
        put(
            &mut aad,
            self.environment.unwrap_or(identity.environment).as_bytes(),
        );
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

base64_newtype!(
    Ciphertext,
    "sealed value",
    "base64 of at least 48 bytes",
    AtLeast(MIN_CIPHERTEXT_LEN)
);

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

    use super::{
        Ciphertext, PLATFORM_SEALED_INFO, PlatformPurpose, SEALED_VALUE_INFO, SealedIdentity,
        SealedScope, SealedValue,
    };
    use crate::{ClientSealedValue, SealingPublicKey};

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
            environment: None,
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

    /// Seals `plaintext` for `purpose` to the Cluster whose identity is `cluster`, under the
    /// key `recipient`: the Cluster's recipient key once it activated, its identity before.
    ///
    /// # Errors
    /// [`SealError`] when `recipient` is not a usable public key or HPKE refuses.
    pub fn seal_platform(
        cluster: &SealingPublicKey,
        recipient: &SealingPublicKey,
        purpose: PlatformPurpose,
        plaintext: &[u8],
    ) -> Result<Ciphertext, SealError> {
        let public = <Kem as hpke::Kem>::PublicKey::from_bytes(recipient.as_bytes())
            .map_err(|_| SealError::Recipient)?;
        let aad = purpose.associated_data(cluster);
        let (encapped, ciphertext) =
            hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem, _>(
                &OpModeS::Base,
                &public,
                PLATFORM_SEALED_INFO,
                plaintext,
                &aad,
                &mut OsRng.unwrap_err(),
            )
            .map_err(|_| SealError::Seal)?;
        let mut sealed = encapped.to_bytes().to_vec();
        sealed.extend(ciphertext);
        Ok(Ciphertext::from_bytes(sealed))
    }

    /// HPKE `info` for a recipient key's private half sealed to one Machine's WireGuard key.
    const RECIPIENT_KEY_INFO: &[u8] = b"ployz-recipient-key-v1";

    /// A Cluster's recipient key, generated where no Cloud page can see it: a Device's CLI,
    /// or the Machine an activation picks.
    pub struct RecipientSecret(zeroize::Zeroizing<[u8; 32]>);

    impl RecipientSecret {
        #[must_use]
        pub fn generate() -> Self {
            let (private, _) = <Kem as hpke::Kem>::gen_keypair(&mut OsRng.unwrap_err());
            Self(zeroize::Zeroizing::new(private.to_bytes().into()))
        }

        #[must_use]
        pub fn from_secret(bytes: [u8; 32]) -> Self {
            Self(zeroize::Zeroizing::new(bytes))
        }

        #[must_use]
        pub fn secret(&self) -> &[u8; 32] {
            &self.0
        }

        #[must_use]
        pub fn public(&self) -> SealingPublicKey {
            let private = <Kem as hpke::Kem>::PrivateKey::from_bytes(self.0.as_ref())
                .expect("an X25519 private key is any 32 bytes");
            SealingPublicKey::from_bytes(<Kem as hpke::Kem>::sk_to_pk(&private).to_bytes().into())
        }

        /// Seal this key to the Machine whose WireGuard public key is `machine`, bound to
        /// `binding` (see [`crate::authority::rotation::sealed_key_binding`]). Anyone who
        /// knows `machine` can build such a blob, so it vouches only for a key the signer
        /// chose itself. Between Machines use [`Self::seal_from`].
        ///
        /// # Errors
        /// [`SealError`] when `machine` is not a usable public key or HPKE refuses.
        pub fn seal_to(
            &self,
            machine: &crate::WireGuardPublicKey,
            binding: &[u8],
        ) -> Result<Ciphertext, SealError> {
            self.sealed(&OpModeS::Base, machine, binding)
        }

        /// Seal this key to `machine` from the Machine whose WireGuard private key is
        /// `sender`, so it opens only as sent by that Machine. See [`Self::open_from`].
        ///
        /// # Errors
        /// [`SealError`] when a key is not usable or HPKE refuses.
        pub fn seal_from(
            &self,
            sender: &[u8; 32],
            machine: &crate::WireGuardPublicKey,
            binding: &[u8],
        ) -> Result<Ciphertext, SealError> {
            let private = <Kem as hpke::Kem>::PrivateKey::from_bytes(sender)
                .map_err(|_| SealError::Recipient)?;
            let public = <Kem as hpke::Kem>::sk_to_pk(&private);
            self.sealed(&OpModeS::Auth((private, public)), machine, binding)
        }

        fn sealed(
            &self,
            mode: &OpModeS<'_, Kem>,
            machine: &crate::WireGuardPublicKey,
            binding: &[u8],
        ) -> Result<Ciphertext, SealError> {
            let public = <Kem as hpke::Kem>::PublicKey::from_bytes(&machine.0)
                .map_err(|_| SealError::Recipient)?;
            let (encapped, ciphertext) =
                hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem, _>(
                    mode,
                    &public,
                    RECIPIENT_KEY_INFO,
                    self.0.as_ref(),
                    binding,
                    &mut OsRng.unwrap_err(),
                )
                .map_err(|_| SealError::Seal)?;
            let mut sealed = encapped.to_bytes().to_vec();
            sealed.extend(ciphertext);
            Ok(Ciphertext::from_bytes(sealed))
        }

        /// Open a key [`Self::seal_to`] sealed to the Machine whose WireGuard private key is
        /// `machine`, under `binding`. `None` when it was sealed to another Machine, under
        /// another binding, or is not a key.
        #[must_use]
        pub fn open(machine: &[u8; 32], binding: &[u8], sealed: &Ciphertext) -> Option<Self> {
            Self::opened(&hpke::OpModeR::Base, machine, binding, sealed)
        }

        /// Open a key [`Self::seal_from`] sealed to `machine` from the Machine whose
        /// WireGuard public key is `sender`. `None` when another Machine or no Machine sent
        /// it, as for [`Self::open`].
        #[must_use]
        pub fn open_from(
            machine: &[u8; 32],
            sender: &crate::WireGuardPublicKey,
            binding: &[u8],
            sealed: &Ciphertext,
        ) -> Option<Self> {
            let sender = <Kem as hpke::Kem>::PublicKey::from_bytes(&sender.0).ok()?;
            Self::opened(&hpke::OpModeR::Auth(sender), machine, binding, sealed)
        }

        fn opened(
            mode: &hpke::OpModeR<'_, Kem>,
            machine: &[u8; 32],
            binding: &[u8],
            sealed: &Ciphertext,
        ) -> Option<Self> {
            let (encapped, ciphertext) = sealed.as_bytes().split_at_checked(32)?;
            let encapped = <Kem as hpke::Kem>::EncappedKey::from_bytes(encapped).ok()?;
            let private = <Kem as hpke::Kem>::PrivateKey::from_bytes(machine).ok()?;
            let opened = zeroize::Zeroizing::new(
                hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
                    mode,
                    &private,
                    &encapped,
                    RECIPIENT_KEY_INFO,
                    ciphertext,
                    binding,
                )
                .ok()?,
            );
            let bytes: [u8; 32] = opened.as_slice().try_into().ok()?;
            Some(Self::from_secret(bytes))
        }
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

        /// The bytes of `sealed` as sealed for `purpose` to this key, or `None` when
        /// it was sealed to another key or for another purpose.
        #[must_use]
        pub fn open_platform(
            &self,
            purpose: PlatformPurpose,
            sealed: &Ciphertext,
        ) -> Option<Vec<u8>> {
            self.open_platform_for(&self.public, purpose, sealed)
        }

        /// The bytes of `sealed` as sealed for `purpose` to the Cluster whose identity is
        /// `cluster`, under this key as its recipient key, or `None` when it doesn't open.
        #[must_use]
        pub fn open_platform_for(
            &self,
            cluster: &SealingPublicKey,
            purpose: PlatformPurpose,
            sealed: &Ciphertext,
        ) -> Option<Vec<u8>> {
            let aad = purpose.associated_data(cluster);
            let (encapped, ciphertext) =
                sealed.as_bytes().split_at_checked(Self::ENCAPPED_KEY_LEN)?;
            let encapped = <Kem as hpke::Kem>::EncappedKey::from_bytes(encapped).ok()?;
            hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
                &hpke::OpModeR::Base,
                &self.private,
                &encapped,
                PLATFORM_SEALED_INFO,
                ciphertext,
                &aad,
            )
            .ok()
        }
    }

    #[cfg(test)]
    mod tests {
        use uuid::Uuid;

        use super::{Keypair, PlatformPurpose, seal_platform, seal_value};
        use crate::{SealedIdentity, SealedScope, SealedValue};

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
                ..identity
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

        #[test]
        fn a_private_access_key_opens_for_its_purpose_only() {
            let cluster = Keypair::generate();
            let sealed = seal_platform(
                &cluster.public(),
                &cluster.public(),
                PlatformPurpose::PrivateAccess,
                b"tskey-auth-k",
            )
            .unwrap();
            assert_eq!(
                cluster
                    .open_platform(PlatformPurpose::PrivateAccess, &sealed)
                    .as_deref(),
                Some(&b"tskey-auth-k"[..])
            );
            assert_eq!(
                cluster.open_platform(PlatformPurpose::IngressTunnel, &sealed),
                None,
                "a tunnel token's purpose does not open it"
            );
        }

        #[test]
        fn a_platform_value_opens_for_its_purpose_on_its_cluster_only() {
            let cluster = Keypair::generate();
            let sealed = seal_platform(
                &cluster.public(),
                &cluster.public(),
                PlatformPurpose::IngressTunnel,
                b"eyJhIjoxfQ",
            )
            .unwrap();
            assert_eq!(
                cluster
                    .open_platform(PlatformPurpose::IngressTunnel, &sealed)
                    .as_deref(),
                Some(&b"eyJhIjoxfQ"[..])
            );
            assert_eq!(
                Keypair::generate().open_platform(PlatformPurpose::IngressTunnel, &sealed),
                None,
                "another Cluster's key does not open it"
            );
            let as_env = SealedValue {
                environment: None,
                sealed: sealed.clone(),
                scope: SealedScope::Shared,
                key: "TUNNEL_TOKEN".into(),
            };
            assert_eq!(
                cluster.open(
                    &authored(),
                    &crate::ClientSealedValue {
                        sealed_for: authored().environment,
                        value: as_env,
                    }
                ),
                None,
                "a platform value is not an env value"
            );
            assert!(!String::from(sealed).contains("eyJhIjoxfQ"));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use base64::{Engine as _, engine::general_purpose::STANDARD};

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
            environment: None,
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
            environment: None,
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

    #[test]
    fn an_inherited_value_binds_the_environment_it_names_in_place_of_its_containers() {
        let parent = identity();
        let preview = SealedIdentity {
            environment: Uuid::from_u128(3),
            ..identity()
        };
        let inherited = SealedValue {
            environment: Some(parent.environment),
            ..sealed("TOKEN")
        };
        assert_eq!(
            inherited.associated_data(&preview),
            sealed("TOKEN").associated_data(&parent)
        );
        assert_ne!(
            inherited.associated_data(&preview),
            sealed("TOKEN").associated_data(&preview)
        );
        let wire = serde_json::to_value(&inherited).unwrap();
        assert_eq!(
            wire.get("environment"),
            Some(&serde_json::json!(parent.environment.to_string()))
        );
        assert_eq!(
            serde_json::from_value::<SealedValue>(wire).unwrap(),
            inherited
        );
    }
}
