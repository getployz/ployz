//! A throwaway Sealing Key for tests that open what the CLI sealed, as a Machine does.

use hpke::{
    Deserializable as _, Kem as _, OpModeR, Serializable as _, aead::ChaCha20Poly1305,
    kdf::HkdfSha256, kem::X25519HkdfSha256 as Kem,
};
use ployz_core::{SEALED_VALUE_INFO, SealedIdentity, SealingPublicKey, config::ClientSealedValue};
use rand_core::{OsRng, TryRngCore as _};

const ENCAPPED_KEY_LEN: usize = 32;

pub struct Keypair {
    private: <Kem as hpke::Kem>::PrivateKey,
    public: SealingPublicKey,
}

impl Keypair {
    pub fn generate() -> Self {
        let (private, public) = Kem::gen_keypair(&mut OsRng.unwrap_err());
        Self {
            private,
            public: SealingPublicKey::from_bytes(public.to_bytes().into()),
        }
    }

    pub fn public(&self) -> SealingPublicKey {
        self.public
    }

    /// The plaintext of `sealed` as `identity` authored it, or `None` when it was
    /// sealed to another key or another identity.
    pub fn open(&self, identity: &SealedIdentity, sealed: &ClientSealedValue) -> Option<String> {
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
