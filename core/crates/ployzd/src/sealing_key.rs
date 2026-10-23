//! The Cluster's Sealing Key as this Machine holds it.

use std::{fmt, fs, io, path::Path};

use hpke::{
    Deserializable, Kem as _, OpModeR, OpModeS, Serializable, aead::ChaCha20Poly1305,
    kdf::HkdfSha256, kem::X25519HkdfSha256,
};
use ployz_core::{
    Ciphertext, SEALED_VALUE_INFO, SealedSealingKey, Sealing, SealingPublicKey, WireGuardPublicKey,
};
use rand_core::{OsRng, TryRngCore as _};
use thiserror::Error;
use zeroize::{Zeroize as _, Zeroizing};

use crate::network::WireGuardPrivateKey;

const FILE_NAME: &str = "sealing.key";
const INFO: &[u8] = b"ployz-sealing-key-v1";
const SHARE_INFO: &[u8] = b"ployz-sealing-key-share-v1";
const ENCAPPED_KEY_LEN: usize = 32;

type Kem = X25519HkdfSha256;

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct SealingKey([u8; 32]);

#[derive(Debug, Error)]
pub enum OpenError {
    #[error("sealed Sealing Key is malformed")]
    Malformed,
    #[error("sealed Sealing Key does not open with this Machine's WireGuard key")]
    Unopenable,
}

#[derive(Debug, Error)]
#[error("cannot seal the Sealing Key to a low-order WireGuard key")]
pub struct SealError;

impl SealingKey {
    pub(crate) fn generate() -> Self {
        let (private, _) = Kem::gen_keypair(&mut OsRng.unwrap_err());
        let mut bytes = private.to_bytes();
        let key = Self(bytes.into());
        bytes.as_mut_slice().zeroize();
        key
    }

    pub(crate) fn public_key(&self) -> SealingPublicKey {
        SealingPublicKey::from_bytes(Kem::sk_to_pk(&self.private()).to_bytes().into())
    }

    pub(crate) fn load(data_dir: &Path) -> io::Result<Option<Self>> {
        match fs::read(data_dir.join(FILE_NAME)).map(Zeroizing::new) {
            Ok(bytes) => <[u8; 32]>::try_from(bytes.as_slice())
                .map(|bytes| Some(Self(bytes)))
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "Sealing Key is not 32 bytes")
                }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// What the Machine at `data_dir` holds of the Sealing Key; `None` when the file
    /// could not be read at all.
    pub(crate) fn observe(data_dir: &Path) -> Option<Sealing> {
        match Self::load(data_dir) {
            Ok(Some(key)) => Some(Sealing::Held {
                public_key: key.public_key(),
            }),
            Ok(None) => Some(Sealing::NeedsAccess),
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                tracing::warn!(%error, "this Machine's Sealing Key is unusable");
                Some(Sealing::NeedsAccess)
            }
            Err(error) => {
                tracing::warn!(%error, "could not read the Sealing Key");
                None
            }
        }
    }

    pub(crate) fn set(data_dir: &Path, key: Option<&Self>) -> io::Result<()> {
        let path = data_dir.join(FILE_NAME);
        match key {
            Some(key) => crate::filesystem::atomic_write(&path, &key.0, 0o600),
            None => match fs::remove_file(path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            },
        }
    }

    pub(crate) fn seal_to(
        &self,
        recipient: &WireGuardPublicKey,
    ) -> Result<SealedSealingKey, SealError> {
        self.seal(&OpModeS::Base, INFO, recipient)
    }

    pub(crate) fn open(
        sealed: &SealedSealingKey,
        wireguard: &WireGuardPrivateKey,
    ) -> Result<Self, OpenError> {
        Self::open_with(&OpModeR::Base, INFO, sealed, wireguard)
    }

    /// Seal the key to `recipient` so only `sender`'s WireGuard key could
    /// have produced the blob.
    ///
    /// # Errors
    /// Refuses a low-order recipient key, which no HPKE secret can be derived
    /// from.
    pub(crate) fn seal_from(
        &self,
        sender: &WireGuardPrivateKey,
        recipient: &WireGuardPublicKey,
    ) -> Result<SealedSealingKey, SealError> {
        let sender_sk = <Kem as hpke::Kem>::PrivateKey::from_bytes(sender.as_bytes())
            .expect("an X25519 private key is any 32 bytes");
        let sender_pk = Kem::sk_to_pk(&sender_sk);
        self.seal(
            &OpModeS::Auth((sender_sk, sender_pk)),
            SHARE_INFO,
            recipient,
        )
    }

    /// Open a blob that [`Self::seal_from`] sealed to `recipient` from the
    /// Machine whose WireGuard key is `sender`.
    ///
    /// # Errors
    /// Returns [`OpenError::Malformed`] when the blob is not an encapsulated
    /// key followed by ciphertext, and [`OpenError::Unopenable`] when it was
    /// sealed to another recipient, by another sender, or without a sender.
    pub(crate) fn open_from(
        sealed: &SealedSealingKey,
        recipient: &WireGuardPrivateKey,
        sender: &WireGuardPublicKey,
    ) -> Result<Self, OpenError> {
        let sender = <Kem as hpke::Kem>::PublicKey::from_bytes(&sender.0)
            .expect("an X25519 public key is any 32 bytes");
        Self::open_with(&OpModeR::Auth(sender), SHARE_INFO, sealed, recipient)
    }

    fn seal(
        &self,
        mode: &OpModeS<Kem>,
        info: &[u8],
        recipient: &WireGuardPublicKey,
    ) -> Result<SealedSealingKey, SealError> {
        let recipient = <Kem as hpke::Kem>::PublicKey::from_bytes(&recipient.0)
            .expect("an X25519 public key is any 32 bytes");
        let (encapped, ciphertext) =
            hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem, _>(
                mode,
                &recipient,
                info,
                &self.0,
                &[],
                &mut OsRng.unwrap_err(),
            )
            .map_err(|_| SealError)?;
        let mut sealed = encapped.to_bytes().to_vec();
        sealed.extend(ciphertext);
        Ok(SealedSealingKey::from_bytes(sealed))
    }

    fn open_with(
        mode: &OpModeR<Kem>,
        info: &[u8],
        sealed: &SealedSealingKey,
        recipient: &WireGuardPrivateKey,
    ) -> Result<Self, OpenError> {
        let bytes = sealed.as_bytes();
        if bytes.len() < ENCAPPED_KEY_LEN {
            return Err(OpenError::Malformed);
        }
        let (encapped, ciphertext) = bytes.split_at(ENCAPPED_KEY_LEN);
        let encapped = <Kem as hpke::Kem>::EncappedKey::from_bytes(encapped)
            .map_err(|_| OpenError::Malformed)?;
        let recipient = <Kem as hpke::Kem>::PrivateKey::from_bytes(recipient.as_bytes())
            .expect("an X25519 private key is any 32 bytes");
        let opened = Zeroizing::new(
            hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
                mode,
                &recipient,
                &encapped,
                info,
                ciphertext,
                &[],
            )
            .map_err(|_| OpenError::Unopenable)?,
        );
        <[u8; 32]>::try_from(opened.as_slice())
            .map(Self)
            .map_err(|_| OpenError::Malformed)
    }

    /// Opens one env value sealed to this key under `aad`, or `None` when it
    /// does not open.
    pub(crate) fn open_value(&self, aad: &[u8], sealed: &Ciphertext) -> Option<Zeroizing<Vec<u8>>> {
        let (encapped, ciphertext) = sealed.as_bytes().split_at_checked(ENCAPPED_KEY_LEN)?;
        let encapped = <Kem as hpke::Kem>::EncappedKey::from_bytes(encapped).ok()?;
        hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
            &OpModeR::Base,
            &self.private(),
            &encapped,
            SEALED_VALUE_INFO,
            ciphertext,
            aad,
        )
        .ok()
        .map(Zeroizing::new)
    }

    #[cfg(test)]
    pub(crate) fn seal_value(&self, aad: &[u8], plaintext: &[u8]) -> Ciphertext {
        let recipient = Kem::sk_to_pk(&self.private());
        let (encapped, ciphertext) =
            hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem, _>(
                &OpModeS::Base,
                &recipient,
                SEALED_VALUE_INFO,
                plaintext,
                aad,
                &mut OsRng.unwrap_err(),
            )
            .expect("sealing to a generated key succeeds");
        let mut sealed = encapped.to_bytes().to_vec();
        sealed.extend(ciphertext);
        Ciphertext::from_bytes(sealed)
    }

    fn private(&self) -> <Kem as hpke::Kem>::PrivateKey {
        <Kem as hpke::Kem>::PrivateKey::from_bytes(&self.0)
            .expect("an X25519 private key is any 32 bytes")
    }

    pub(crate) const fn from_secret(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn secret(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Drop for SealingKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SealingKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "SealingKey({})", self.public_key())
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::test_dir::TestDir;

    #[test]
    fn sealing_key_is_stored_root_only_and_loads_after_restart() {
        let dir = TestDir::new("sealing-key");
        fs::create_dir_all(&dir.0).unwrap();
        assert_eq!(SealingKey::load(&dir.0).unwrap(), None);

        let key = SealingKey::generate();
        SealingKey::set(&dir.0, Some(&key)).unwrap();

        let mode = fs::metadata(dir.0.join(FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(SealingKey::load(&dir.0).unwrap(), Some(key));

        SealingKey::set(&dir.0, None).unwrap();
        assert_eq!(SealingKey::load(&dir.0).unwrap(), None);
        SealingKey::set(&dir.0, None).unwrap();
    }

    #[test]
    fn sealing_key_generates_a_new_key_each_time() {
        assert_ne!(SealingKey::generate(), SealingKey::generate());
    }

    #[test]
    fn sealing_key_opens_only_with_the_recipient_wireguard_key() {
        let key = SealingKey::generate();
        let joiner = WireGuardPrivateKey::generate();
        let sealed = key.seal_to(&joiner.public_key()).unwrap();

        assert_eq!(SealingKey::open(&sealed, &joiner).unwrap(), key);
        assert!(matches!(
            SealingKey::open(&sealed, &WireGuardPrivateKey::generate()),
            Err(OpenError::Unopenable)
        ));
        assert!(matches!(
            SealingKey::open(&SealedSealingKey::from_bytes(vec![1; 8]), &joiner),
            Err(OpenError::Malformed)
        ));
    }

    #[test]
    fn sealing_key_refuses_a_low_order_recipient() {
        let key = SealingKey::generate();
        assert!(key.seal_to(&WireGuardPublicKey([0; 32])).is_err());
    }

    #[test]
    fn sealing_key_sealed_bytes_hide_the_private_half() {
        let key = SealingKey::generate();
        let sealed = key
            .seal_to(&WireGuardPrivateKey::generate().public_key())
            .unwrap();
        assert!(
            !sealed
                .as_bytes()
                .windows(key.0.len())
                .any(|window| window == key.0)
        );
        let private = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, key.0);
        assert!(!format!("{key:?}").contains(&private));
    }

    #[test]
    fn a_sealed_value_opens_only_under_its_own_key_and_associated_data() {
        let key = SealingKey::generate();
        let sealed = key.seal_value(b"aad", b"hunter2");
        assert_eq!(
            key.open_value(b"aad", &sealed).unwrap().as_slice(),
            b"hunter2"
        );
        assert!(key.open_value(b"other", &sealed).is_none());
        assert!(SealingKey::generate().open_value(b"aad", &sealed).is_none());
    }

    #[test]
    fn a_sealed_value_with_any_byte_flipped_does_not_open() {
        let key = SealingKey::generate();
        let sealed = key.seal_value(b"aad", b"hunter2");
        assert!(sealed.as_bytes().len() > ENCAPPED_KEY_LEN);

        for index in 0..sealed.as_bytes().len() {
            let mut tampered = sealed.as_bytes().to_vec();
            *tampered.get_mut(index).unwrap() ^= 0x01;
            let part = if index < ENCAPPED_KEY_LEN {
                "encapsulated key"
            } else {
                "ciphertext"
            };
            assert!(
                key.open_value(b"aad", &Ciphertext::from_bytes(tampered))
                    .is_none(),
                "{part} byte {index}"
            );
        }
    }

    #[test]
    fn sealing_key_uses_the_wireguard_public_key_as_the_hpke_recipient() {
        let wireguard = WireGuardPrivateKey::generate();
        let hpke = <Kem as hpke::Kem>::PrivateKey::from_bytes(wireguard.as_bytes()).unwrap();
        assert_eq!(
            Kem::sk_to_pk(&hpke).to_bytes().as_slice(),
            wireguard.public_key().0.as_slice()
        );
    }

    #[test]
    fn shared_sealing_key_opens_only_for_the_recipient_from_the_sender() {
        let key = SealingKey::generate();
        let holder = WireGuardPrivateKey::generate();
        let recipient = WireGuardPrivateKey::generate();
        let sealed = key.seal_from(&holder, &recipient.public_key()).unwrap();

        assert_eq!(
            SealingKey::open_from(&sealed, &recipient, &holder.public_key()).unwrap(),
            key
        );
        assert!(matches!(
            SealingKey::open_from(
                &sealed,
                &WireGuardPrivateKey::generate(),
                &holder.public_key()
            ),
            Err(OpenError::Unopenable)
        ));
        assert!(matches!(
            SealingKey::open_from(
                &sealed,
                &recipient,
                &WireGuardPrivateKey::generate().public_key()
            ),
            Err(OpenError::Unopenable)
        ));
        assert!(matches!(
            SealingKey::open_from(
                &SealedSealingKey::from_bytes(vec![1; 8]),
                &recipient,
                &holder.public_key()
            ),
            Err(OpenError::Malformed)
        ));
    }

    #[test]
    fn a_join_sealed_key_does_not_pass_as_a_shared_one() {
        let key = SealingKey::generate();
        let holder = WireGuardPrivateKey::generate();
        let recipient = WireGuardPrivateKey::generate();
        let joined = key.seal_to(&recipient.public_key()).unwrap();
        assert!(matches!(
            SealingKey::open_from(&joined, &recipient, &holder.public_key()),
            Err(OpenError::Unopenable)
        ));
        let shared = key.seal_from(&holder, &recipient.public_key()).unwrap();
        assert!(matches!(
            SealingKey::open(&shared, &recipient),
            Err(OpenError::Unopenable)
        ));
    }

    #[test]
    fn sharing_refuses_a_low_order_recipient() {
        let key = SealingKey::generate();
        let holder = WireGuardPrivateKey::generate();
        assert!(
            key.seal_from(&holder, &WireGuardPublicKey([0; 32]))
                .is_err()
        );
    }
}
