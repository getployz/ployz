//! Seals one secret in the CLI to an Environment's Sealing Key, so Cloud carries
//! ciphertext it never opens. Scheme and associated data match what a Machine
//! opens with: HPKE base mode over X25519, HKDF-SHA256 and ChaCha20-Poly1305,
//! bound to the variable, Service, Environment and Organization it was authored as.

use hpke::{
    Deserializable as _, OpModeS, Serializable as _, aead::ChaCha20Poly1305, kdf::HkdfSha256,
    kem::X25519HkdfSha256,
};
use ployz_core::{
    Ciphertext, SEALED_VALUE_INFO, SealedIdentity, SealedScope, SealedValue, SealingPublicKey,
    config::ClientSealedValue,
};
use rand_core::{OsRng, TryRngCore as _};
use thiserror::Error;

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
    let (encapped, ciphertext) = hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem, _>(
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

/// A throwaway Sealing Key for tests that open what the CLI sealed.
#[doc(hidden)]
pub mod testing {
    use hpke::{
        Deserializable as _, Kem as _, OpModeR, Serializable as _, aead::ChaCha20Poly1305,
        kdf::HkdfSha256,
    };
    use ployz_core::{
        SEALED_VALUE_INFO, SealedIdentity, SealingPublicKey, config::ClientSealedValue,
    };
    use rand_core::{OsRng, TryRngCore as _};

    use super::Kem;

    const ENCAPPED_KEY_LEN: usize = 32;

    pub struct Keypair {
        private: <Kem as hpke::Kem>::PrivateKey,
        public: SealingPublicKey,
    }

    impl Keypair {
        #[must_use]
        pub fn generate() -> Self {
            let (private, public) = Kem::gen_keypair(&mut OsRng.unwrap_err());
            Self {
                private,
                public: SealingPublicKey::from_bytes(public.to_bytes().into()),
            }
        }

        #[must_use]
        pub fn public(&self) -> SealingPublicKey {
            self.public
        }

        /// The plaintext of `sealed` as `identity` authored it, or `None` when it
        /// was sealed to another key or another identity.
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
                .split_at_checked(ENCAPPED_KEY_LEN)?;
            let encapped = <Kem as hpke::Kem>::EncappedKey::from_bytes(encapped).ok()?;
            let plaintext = hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
                &OpModeR::Base,
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
}

#[cfg(test)]
mod tests {
    use ployz_core::{SealedIdentity, SealedScope};
    use uuid::Uuid;

    use super::{seal_value, testing::Keypair};

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
