//! The Cluster's Sealing Key as this Machine holds it.

use std::{fmt, fs, io, path::Path};

use hpke::{
    Deserializable, Kem as _, OpModeR, OpModeS, Serializable, aead::ChaCha20Poly1305,
    kdf::HkdfSha256, kem::X25519HkdfSha256,
};
use ployz_core::{
    Ciphertext, PLATFORM_SEALED_INFO, PlatformPurpose, SEALED_VALUE_INFO, SealedSealingKey,
    Sealing, SealingPublicKey, WireGuardPublicKey,
};
use rand_core::{OsRng, TryRngCore as _};
use thiserror::Error;
use zeroize::{Zeroize as _, Zeroizing};

use crate::corrosion::ClusterSealing;
use crate::network::WireGuardPrivateKey;

const FILE_NAME: &str = "sealing.key";
/// The recipient key a person named at activation. See [`ClusterKeys`].
const RECIPIENT_FILE_NAME: &str = "recipient.key";
/// A recipient key this Machine generated for one activation, held unused until a person
/// names it: the 32-byte key, then the activation's operation ID.
const GENERATED_FILE_NAME: &str = "recipient-generated.key";
/// The signed rotation that named the installed recipient key, which a joiner after
/// activation verifies before it installs the key.
const ROTATION_FILE_NAME: &str = "recipient-rotation.json";
const INFO: &[u8] = b"ployz-sealing-key-v1";
const SHARE_INFO: &[u8] = b"ployz-sealing-key-share-v1";
/// Distinct from every other label this key is expanded or sealed under, such as the join
/// confirmation's and the Sealed values' associated data.
const ATTEST_INFO: &[u8] = b"ployz build-secrets attest v1";
/// The Cluster's Volume handoff receipt key, distinct from the attest key and every label above.
const RECEIPT_INFO: &[u8] = b"ployz volume handoff v1";
const ENCAPPED_KEY_LEN: usize = 32;

type Kem = X25519HkdfSha256;

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct SealingKey([u8; 32]);

/// What `<data_dir>/sealing.key` holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeyFile<Key = SealingKey> {
    /// Exactly 32 bytes, read as the key.
    Held(Key),
    /// No file.
    Absent,
    /// A file that is not 32 bytes. ployzd never writes one.
    Unusable,
}

impl KeyFile {
    pub(crate) fn public(&self) -> KeyFile<SealingPublicKey> {
        match self {
            Self::Held(key) => KeyFile::Held(key.public_key()),
            Self::Absent => KeyFile::Absent,
            Self::Unusable => KeyFile::Unusable,
        }
    }
}

impl KeyFile<SealingPublicKey> {
    pub(crate) fn held(self) -> Option<SealingPublicKey> {
        match self {
            Self::Held(public_key) => Some(public_key),
            Self::Absent | Self::Unusable => None,
        }
    }

    /// This Machine's Sealing: the key its file holds, or, when the file holds
    /// none, what `cluster` reads of the Cluster's Sealing. `cluster` is
    /// awaited only then, so a held key never waits on or fails with a
    /// Cluster read.
    ///
    /// # Errors
    /// Returns `cluster`'s error.
    pub(crate) async fn sealing<E>(
        self,
        cluster: impl Future<Output = Result<Option<ClusterSealing>, E>>,
    ) -> Result<Sealing, E> {
        match self {
            Self::Held(public_key) => Ok(Sealing::Held { public_key }),
            Self::Absent | Self::Unusable => cluster.await.map(lacking_key),
        }
    }
}

/// What a Machine whose key file holds no key reports, given the Cluster's
/// Sealing as it reads it (`None` with no Cluster store): the key a recovery
/// awaits, and otherwise that it needs access.
pub(crate) fn lacking_key(cluster: Option<ClusterSealing>) -> Sealing {
    match cluster {
        Some(ClusterSealing::AwaitsRecovery(public_key)) => Sealing::AwaitsRecovery { public_key },
        Some(ClusterSealing::Published { .. } | ClusterSealing::Unsealed) | None => {
            Sealing::NeedsAccess
        }
    }
}

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

    /// The Cluster's build attest key pair. Every Machine holding this Sealing Key derives
    /// the same one, and nothing without the Sealing Key can. It seals build secrets to a
    /// rule build, which writes its public half into the image token.
    pub(crate) fn attest(&self) -> ployz_core::X25519Secret {
        let mut key = Zeroizing::new([0; 32]);
        hkdf::Hkdf::<sha2_0_10::Sha256>::new(None, &self.0)
            .expand(ATTEST_INFO, key.as_mut())
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        ployz_core::X25519Secret::from_bytes(*key)
    }

    /// The Cluster's receipt key, which a Move's source signs its handoff receipt with and
    /// its target checks. Every Machine holding this Sealing Key derives the same one, and
    /// nothing without the Sealing Key can, so any Machine of the Cluster could sign one.
    pub(crate) fn receipt_signer(&self) -> ployz_core::authority::Signer {
        let mut seed = Zeroizing::new([0; 32]);
        hkdf::Hkdf::<sha2_0_10::Sha256>::new(None, &self.0)
            .expand(RECEIPT_INFO, seed.as_mut())
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        ployz_core::authority::Signer::from_seed(&seed)
    }

    /// Classifies `<data_dir>/sealing.key`: 32 bytes are the key, no file is
    /// [`KeyFile::Absent`], and any other length is [`KeyFile::Unusable`].
    ///
    /// # Errors
    /// Returns the read error when the file exists but cannot be read. That is
    /// never [`KeyFile::Unusable`].
    pub(crate) fn read(data_dir: &Path) -> io::Result<KeyFile> {
        match fs::read(data_dir.join(FILE_NAME)).map(Zeroizing::new) {
            Ok(bytes) => Ok(<[u8; 32]>::try_from(bytes.as_slice())
                .map_or(KeyFile::Unusable, |bytes| KeyFile::Held(Self(bytes)))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(KeyFile::Absent),
            Err(error) => Err(error),
        }
    }

    /// The key `<data_dir>/sealing.key` holds, or `None` with no file.
    ///
    /// # Errors
    /// Returns [`io::ErrorKind::InvalidData`] for a file that is not 32 bytes,
    /// and the read error for a file that cannot be read.
    pub(crate) fn load(data_dir: &Path) -> io::Result<Option<Self>> {
        match Self::read(data_dir)? {
            KeyFile::Held(key) => Ok(Some(key)),
            KeyFile::Absent => Ok(None),
            KeyFile::Unusable => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Sealing Key is not 32 bytes",
            )),
        }
    }

    /// What `<data_dir>/sealing.key` holds, with only the public half of a
    /// key; `None` when the file exists but could not be read.
    pub(crate) fn observe(data_dir: &Path) -> Option<KeyFile<SealingPublicKey>> {
        match Self::read(data_dir) {
            Ok(KeyFile::Unusable) => {
                tracing::warn!("this Machine's Sealing Key is unusable: it is not 32 bytes");
                Some(KeyFile::Unusable)
            }
            Ok(file) => Some(file.public()),
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

    /// The recipient key `<data_dir>/recipient.key` holds, or `None` with no file.
    ///
    /// # Errors
    /// Returns [`io::ErrorKind::InvalidData`] for a file that is not 32 bytes, and the read
    /// error for a file that cannot be read.
    pub(crate) fn load_recipient(data_dir: &Path) -> io::Result<Option<Self>> {
        match fs::read(data_dir.join(RECIPIENT_FILE_NAME)).map(Zeroizing::new) {
            Ok(bytes) => <[u8; 32]>::try_from(bytes.as_slice())
                .map(|bytes| Some(Self(bytes)))
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "recipient key is not 32 bytes")
                }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn set_recipient(data_dir: &Path, key: &Self) -> io::Result<()> {
        crate::filesystem::atomic_write(&data_dir.join(RECIPIENT_FILE_NAME), &key.0, 0o600)
    }

    /// Keep the signed rotation that named the installed recipient key.
    pub(crate) fn set_recipient_rotation(
        data_dir: &Path,
        rotation: &ployz_core::authority::Signed<ployz_core::authority::SealingRotation>,
    ) -> io::Result<()> {
        let bytes = serde_json::to_vec(rotation).map_err(io::Error::other)?;
        crate::filesystem::atomic_write(&data_dir.join(ROTATION_FILE_NAME), &bytes, 0o600)
    }

    /// The rotation [`Self::set_recipient_rotation`] kept, or `None` with no file.
    pub(crate) fn load_recipient_rotation(
        data_dir: &Path,
    ) -> io::Result<Option<ployz_core::authority::Signed<ployz_core::authority::SealingRotation>>>
    {
        match fs::read(data_dir.join(ROTATION_FILE_NAME)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The key this Machine generated for `operation`, generating and keeping it on the
    /// first call, so a resend under the same operation returns the same key. A key kept
    /// for another operation is replaced: only one activation is ever pending here.
    pub(crate) fn generated_for(
        data_dir: &Path,
        operation: ployz_core::OperationId,
    ) -> io::Result<Self> {
        if let Some((kept, key)) = Self::load_generated(data_dir)?
            && kept == operation
        {
            return Ok(key);
        }
        let key = Self::generate();
        let mut bytes = Zeroizing::new(key.0.to_vec());
        bytes.extend(operation.to_string().as_bytes());
        crate::filesystem::atomic_write(&data_dir.join(GENERATED_FILE_NAME), &bytes, 0o600)?;
        Ok(key)
    }

    /// The key [`Self::generated_for`] kept, and its operation.
    pub(crate) fn load_generated(
        data_dir: &Path,
    ) -> io::Result<Option<(ployz_core::OperationId, Self)>> {
        let bytes = match fs::read(data_dir.join(GENERATED_FILE_NAME)).map(Zeroizing::new) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let malformed = || io::Error::new(io::ErrorKind::InvalidData, "generated key is malformed");
        let (key, operation) = bytes.split_at_checked(32).ok_or_else(malformed)?;
        let operation = std::str::from_utf8(operation)
            .ok()
            .and_then(|text| text.parse().ok())
            .ok_or_else(malformed)?;
        Ok(Some((
            operation,
            Self(key.try_into().map_err(|_| malformed())?),
        )))
    }

    pub(crate) fn seal_to(
        &self,
        recipient: &WireGuardPublicKey,
    ) -> Result<SealedSealingKey, SealError> {
        self.seal(&OpModeS::Base, INFO, recipient)
    }

    #[cfg(test)]
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

    /// What a joiner that opened `approval`'s capsule with this key sends back.
    pub(crate) fn confirm_join(
        &self,
        approval: &ployz_core::authority::Signed<ployz_core::authority::JoinApproval>,
    ) -> ployz_core::authority::join::JoinConfirmation {
        use hkdf::hmac::Mac as _;
        ployz_core::authority::join::JoinConfirmation(
            self.join_mac(approval).finalize().into_bytes().into(),
        )
    }

    /// Whether `confirmation` proves its sender opened `approval`'s capsule with this key.
    pub(crate) fn confirms_join(
        &self,
        approval: &ployz_core::authority::Signed<ployz_core::authority::JoinApproval>,
        confirmation: &ployz_core::authority::join::JoinConfirmation,
    ) -> bool {
        use hkdf::hmac::Mac as _;
        self.join_mac(approval)
            .verify_slice(&confirmation.0)
            .is_ok()
    }

    fn join_mac(
        &self,
        approval: &ployz_core::authority::Signed<ployz_core::authority::JoinApproval>,
    ) -> hkdf::hmac::Hmac<sha2_0_10::Sha256> {
        use hkdf::hmac::Mac as _;
        let mut key = Zeroizing::new([0; 32]);
        hkdf::Hkdf::<sha2_0_10::Sha256>::new(None, &self.0)
            .expand(b"ployz-join-confirmation-v1", key.as_mut())
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        let mut mac = hkdf::hmac::Hmac::<sha2_0_10::Sha256>::new_from_slice(key.as_ref())
            .expect("HMAC takes a key of any length");
        mac.update(&approval.signing_input());
        mac
    }

    pub(crate) fn seal_join(
        &self,
        recipient: &WireGuardPublicKey,
        approval: &ployz_core::authority::Signed<ployz_core::authority::JoinApproval>,
    ) -> Result<SealedSealingKey, SealError> {
        let mut payload = Zeroizing::new(self.0.to_vec());
        payload.extend(serde_json::to_vec(approval).expect("signed join serializes"));
        Self::seal_bytes(&payload, &OpModeS::Base, INFO, recipient)
    }

    pub(crate) fn open_join(
        sealed: &SealedSealingKey,
        recipient: &WireGuardPrivateKey,
    ) -> Result<
        (
            Self,
            Option<ployz_core::authority::Signed<ployz_core::authority::JoinApproval>>,
        ),
        OpenError,
    > {
        let payload = Self::open_bytes(&OpModeR::Base, INFO, sealed, recipient)?;
        let (bytes, body) = payload.split_at_checked(32).ok_or(OpenError::Malformed)?;
        let key = Self(bytes.try_into().map_err(|_| OpenError::Malformed)?);
        let approval = if body.is_empty() {
            None
        } else {
            Some(serde_json::from_slice(body).map_err(|_| OpenError::Malformed)?)
        };
        Ok((key, approval))
    }

    fn seal(
        &self,
        mode: &OpModeS<Kem>,
        info: &[u8],
        recipient: &WireGuardPublicKey,
    ) -> Result<SealedSealingKey, SealError> {
        Self::seal_bytes(&self.0, mode, info, recipient)
    }

    fn seal_bytes(
        plaintext: &[u8],
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
                plaintext,
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
        let opened = Self::open_bytes(mode, info, sealed, recipient)?;
        <[u8; 32]>::try_from(opened.as_slice())
            .map(Self)
            .map_err(|_| OpenError::Malformed)
    }

    fn open_bytes(
        mode: &OpModeR<Kem>,
        info: &[u8],
        sealed: &SealedSealingKey,
        recipient: &WireGuardPrivateKey,
    ) -> Result<Zeroizing<Vec<u8>>, OpenError> {
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
        Ok(opened)
    }

    /// Opens one env value sealed to this key under `aad`, or `None` when it
    /// does not open.
    pub(crate) fn open_value(&self, aad: &[u8], sealed: &Ciphertext) -> Option<Zeroizing<Vec<u8>>> {
        self.open_under(SEALED_VALUE_INFO, aad, sealed)
    }

    /// Opens a value the platform sealed to this Cluster for `purpose`, or `None`
    /// when it does not open: sealed for another Cluster, another purpose, or as
    /// an env value.
    #[cfg(test)]
    pub(crate) fn open_platform(
        &self,
        purpose: PlatformPurpose,
        sealed: &Ciphertext,
    ) -> Option<Zeroizing<Vec<u8>>> {
        let aad = purpose.associated_data(&self.public_key());
        self.open_under(PLATFORM_SEALED_INFO, &aad, sealed)
    }

    /// Opens a value sealed for `purpose` to the Cluster whose identity is `cluster`, under
    /// this key, which may be the Cluster's recipient key rather than its identity.
    fn open_platform_of(
        &self,
        cluster: &SealingPublicKey,
        purpose: PlatformPurpose,
        sealed: &Ciphertext,
    ) -> Option<Zeroizing<Vec<u8>>> {
        self.open_under(
            PLATFORM_SEALED_INFO,
            &purpose.associated_data(cluster),
            sealed,
        )
    }

    /// Seal `plaintext` under `info` and `aad` to this key's public half.
    pub(crate) fn seal_under(&self, info: &[u8], aad: &[u8], plaintext: &[u8]) -> Ciphertext {
        let recipient = Kem::sk_to_pk(&self.private());
        let (encapped, ciphertext) =
            hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem, _>(
                &OpModeS::Base,
                &recipient,
                info,
                plaintext,
                aad,
                &mut OsRng.unwrap_err(),
            )
            .expect("sealing to a held key succeeds");
        let mut sealed = encapped.to_bytes().to_vec();
        sealed.extend(ciphertext);
        Ciphertext::from_bytes(sealed)
    }

    fn open_under(
        &self,
        info: &[u8],
        aad: &[u8],
        sealed: &Ciphertext,
    ) -> Option<Zeroizing<Vec<u8>>> {
        let (encapped, ciphertext) = sealed.as_bytes().split_at_checked(ENCAPPED_KEY_LEN)?;
        let encapped = <Kem as hpke::Kem>::EncappedKey::from_bytes(encapped).ok()?;
        hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
            &OpModeR::Base,
            &self.private(),
            &encapped,
            info,
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

/// The Cluster's keys as this Machine holds them. The identity is the Cluster's first
/// Sealing Key: statements, pins and every sealed value's binding name its public half.
/// After activation a recipient key sits beside it. New values seal to the recipient, so a
/// past holder of the identity's private half opens none of them. The identity still opens
/// what was sealed before activation, and the build attest key derives from the recipient.
pub(crate) struct ClusterKeys {
    identity: SealingKey,
    recipient: Option<SealingKey>,
}

impl ClusterKeys {
    /// The keys `data_dir` holds, or `None` with no identity key.
    ///
    /// # Errors
    /// Returns the read errors of [`SealingKey::load`] and [`SealingKey::load_recipient`].
    pub(crate) fn load(data_dir: &Path) -> io::Result<Option<Self>> {
        let Some(identity) = SealingKey::load(data_dir)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            identity,
            recipient: SealingKey::load_recipient(data_dir)?,
        }))
    }

    pub(crate) fn identity(&self) -> &SealingKey {
        &self.identity
    }

    pub(crate) fn recipient(&self) -> Option<&SealingKey> {
        self.recipient.as_ref()
    }

    /// The key new values seal to: the recipient once installed, the identity before.
    pub(crate) fn sealing_to(&self) -> &SealingKey {
        self.recipient.as_ref().unwrap_or(&self.identity)
    }

    pub(crate) fn open_value(&self, aad: &[u8], sealed: &Ciphertext) -> Option<Zeroizing<Vec<u8>>> {
        self.recipient
            .as_ref()
            .and_then(|key| key.open_value(aad, sealed))
            .or_else(|| self.identity.open_value(aad, sealed))
    }

    pub(crate) fn open_platform(
        &self,
        purpose: PlatformPurpose,
        sealed: &Ciphertext,
    ) -> Option<Zeroizing<Vec<u8>>> {
        let cluster = self.identity.public_key();
        self.recipient
            .as_ref()
            .and_then(|key| key.open_platform_of(&cluster, purpose, sealed))
            .or_else(|| self.identity.open_platform_of(&cluster, purpose, sealed))
    }

    /// The keys a Move's handoff receipt may be signed under: the recipient's derivation
    /// once one is installed, and the identity's too for an approval issued before this
    /// Machine activated, so a receipt already issued keeps verifying while a past holder
    /// of the identity can't sign one for a later Move.
    pub(crate) fn receipt_keys(
        &self,
        issued_before_activation: bool,
    ) -> impl Iterator<Item = &SealingKey> {
        let identity =
            (self.recipient.is_some() && issued_before_activation).then_some(&self.identity);
        std::iter::once(self.sealing_to()).chain(identity)
    }

    /// The build attest key: derived from the recipient once installed.
    pub(crate) fn attest(&self) -> ployz_core::X25519Secret {
        self.sealing_to().attest()
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
    fn the_key_file_reads_as_held_only_at_32_bytes() {
        let dir = TestDir::new("sealing-key-file");
        fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join(FILE_NAME);

        assert_eq!(SealingKey::read(&dir.0).unwrap(), KeyFile::Absent);
        assert_eq!(SealingKey::load(&dir.0).unwrap(), None);

        for len in [0, 31, 33] {
            fs::write(&path, vec![7; len]).unwrap();
            assert_eq!(
                SealingKey::read(&dir.0).unwrap(),
                KeyFile::Unusable,
                "{len} bytes"
            );
            assert_eq!(
                SealingKey::load(&dir.0).unwrap_err().kind(),
                io::ErrorKind::InvalidData,
                "{len} bytes"
            );
        }

        fs::write(&path, [7; 32]).unwrap();
        assert_eq!(
            SealingKey::read(&dir.0).unwrap(),
            KeyFile::Held(SealingKey([7; 32]))
        );
        assert_eq!(SealingKey::load(&dir.0).unwrap(), Some(SealingKey([7; 32])));

        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let unread = SealingKey::read(&dir.0).unwrap_err();
        assert!(
            !matches!(
                unread.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidData
            ),
            "{unread:?}"
        );
        let unloaded = SealingKey::load(&dir.0).unwrap_err();
        assert_eq!(unloaded.kind(), unread.kind());
        assert_eq!(SealingKey::observe(&dir.0), None);
    }

    #[test]
    fn a_key_file_shows_only_the_public_half_of_its_key() {
        let key = SealingKey::generate();
        let public_key = key.public_key();

        assert_eq!(KeyFile::Held(key).public(), KeyFile::Held(public_key));
        assert_eq!(KeyFile::Absent.public(), KeyFile::Absent);
        assert_eq!(KeyFile::Unusable.public(), KeyFile::Unusable);
        assert_eq!(KeyFile::Held(public_key).held(), Some(public_key));
        assert_eq!(KeyFile::<SealingPublicKey>::Absent.held(), None);
        assert_eq!(KeyFile::<SealingPublicKey>::Unusable.held(), None);
    }

    #[test]
    fn a_machine_lacking_the_key_reports_only_an_awaited_key() {
        let published = SealingKey::generate().public_key();
        let awaited = SealingKey::generate().public_key();

        assert_eq!(lacking_key(None), Sealing::NeedsAccess);
        assert_eq!(
            lacking_key(Some(ClusterSealing::Unsealed)),
            Sealing::NeedsAccess
        );
        assert_eq!(
            lacking_key(Some(ClusterSealing::Published {
                key: published,
                superseded: Some(awaited),
            })),
            Sealing::NeedsAccess
        );
        assert_eq!(
            lacking_key(Some(ClusterSealing::AwaitsRecovery(awaited))),
            Sealing::AwaitsRecovery {
                public_key: awaited
            }
        );
    }

    #[tokio::test]
    async fn a_held_key_reads_no_cluster_and_a_missing_one_reads_it() {
        let held = SealingKey::generate().public_key();
        let awaited = SealingKey::generate().public_key();
        let unread = async { unreachable!("a held key reads no Cluster") };
        let awaits = || async { Ok::<_, ()>(Some(ClusterSealing::AwaitsRecovery(awaited))) };

        assert_eq!(
            KeyFile::Held(held).sealing::<()>(unread).await,
            Ok(Sealing::Held { public_key: held })
        );
        for file in [KeyFile::Absent, KeyFile::Unusable] {
            assert_eq!(
                file.sealing(awaits()).await,
                Ok(Sealing::AwaitsRecovery {
                    public_key: awaited
                }),
                "{file:?}"
            );
            assert_eq!(file.sealing(async { Err("unread") }).await, Err("unread"));
        }
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
    fn a_value_sealed_by_the_core_sealer_opens_under_this_key() {
        let key = SealingKey::generate();
        let identity = ployz_core::SealedIdentity {
            organization: "org_1".into(),
            environment: uuid::Uuid::from_u128(2),
            service: uuid::Uuid::from_u128(3),
        };
        let sealed = ployz_core::seal_value(&key.public_key(), &identity, "TOKEN", "hunter2")
            .unwrap()
            .value;
        let aad = sealed.associated_data(&identity);
        assert_eq!(
            key.open_value(&aad, &sealed.sealed).unwrap().as_slice(),
            b"hunter2"
        );
        let other = ployz_core::SealedIdentity {
            environment: uuid::Uuid::from_u128(4),
            ..identity
        };
        assert!(
            key.open_value(&sealed.associated_data(&other), &sealed.sealed)
                .is_none()
        );
        assert!(
            SealingKey::generate()
                .open_value(&aad, &sealed.sealed)
                .is_none()
        );
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
    fn every_holder_of_a_sealing_key_derives_the_same_attest_key_and_no_other_key_does() {
        let key = SealingKey([7; 32]);
        let attest = key.attest().attest();
        assert_eq!(SealingKey([7; 32]).attest().attest(), attest);
        assert_ne!(SealingKey([8; 32]).attest().attest(), attest);
        assert_ne!(attest.as_bytes(), key.public_key().as_bytes());
        // Computed apart from this code, with Python's hmac and cryptography: HKDF-SHA256
        // with no salt, then X25519. A change strands every image token minted before it.
        assert_eq!(
            attest.to_string(),
            "f68dfa2222a3be1f41768b8b1e5ddc3baeaa6b05b4462501fa74f583068ee215"
        );
    }

    #[test]
    fn after_activation_only_an_earlier_approval_takes_a_receipt_under_the_identity() {
        let dir = TestDir::new("receipt-keys");
        fs::create_dir_all(&dir.0).unwrap();
        let identity = SealingKey::generate();
        SealingKey::set(&dir.0, Some(&identity)).unwrap();
        let signers = |keys: &ClusterKeys, earlier| {
            keys.receipt_keys(earlier)
                .map(|key| key.receipt_signer().public())
                .collect::<Vec<_>>()
        };
        let before = ClusterKeys::load(&dir.0).unwrap().unwrap();
        assert_eq!(
            signers(&before, true),
            vec![identity.receipt_signer().public()]
        );

        let recipient = SealingKey::generate();
        SealingKey::set_recipient(&dir.0, &recipient).unwrap();
        let after = ClusterKeys::load(&dir.0).unwrap().unwrap();

        assert_eq!(
            signers(&after, false),
            vec![recipient.receipt_signer().public()]
        );
        assert_eq!(
            signers(&after, true),
            vec![
                recipient.receipt_signer().public(),
                identity.receipt_signer().public()
            ]
        );
    }

    #[test]
    fn every_holder_of_a_sealing_key_derives_the_same_receipt_key_and_no_other_key_does() {
        let key = SealingKey([7; 32]);
        let receipt = key.receipt_signer().public();
        assert_eq!(SealingKey([7; 32]).receipt_signer().public(), receipt);
        assert_ne!(SealingKey([8; 32]).receipt_signer().public(), receipt);
        assert_ne!(receipt.as_bytes(), key.public_key().as_bytes());
        assert_ne!(receipt.as_bytes(), key.attest().attest().as_bytes());
        // Computed apart from this code, with Python's hmac and cryptography: HKDF-SHA256
        // with no salt, then the Ed25519 public key of that seed. A change strands every
        // receipt a Move's target has not yet promoted on.
        assert_eq!(
            receipt.to_string(),
            "4c3a67d63762ee395941475f4fa5fa832568187702269718832a590e4175a4ea"
        );
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

    /// The vector `dashboard/src/modules/variables/seal-value.test.ts` seals in the
    /// browser, from the same literals. Test keys only.
    #[test]
    fn a_value_the_browser_sealed_opens_only_for_the_variable_it_was_sealed_as() {
        use ployz_core::{SealedIdentity, SealedScope, SealedValue};
        use uuid::Uuid;

        let key = SealingKey(
            hex::decode("347ace27753bb0420141f071765286281c5fb4c450a9811d4ab13b67ac3da146")
                .unwrap()
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            key.public_key().to_string(),
            "NdiV1QY9P3ZyoC82YA/FaBs0zlWTG/CXU1tLfcS0CDg="
        );
        let sealed = Ciphertext::try_from(
            "aQ3JpLLRSRFxTny/uywru4DsdBNaz2ed/8puzj80FnXxzlL2DR0xHieAOAisLAf2fr0pN6dractyCf7l0M6K2KvTlZZRDfI84Nb+Oja16vnS"
                .to_owned(),
        )
        .unwrap();
        let environment = Uuid::parse_str("0e7c2f3a-5b1d-4c8e-9f60-1a2b3c4d5e6f").unwrap();
        let service = Uuid::parse_str("7d4e9a12-3c5b-4f6e-8a90-b1c2d3e4f5a6").unwrap();
        let aad = |organization: &str, environment: Uuid, service: Uuid, variable: &str| {
            SealedValue {
                environment: None,
                sealed: sealed.clone(),
                scope: SealedScope::Service(service),
                key: variable.into(),
            }
            .associated_data(&SealedIdentity {
                organization: organization.into(),
                environment,
                service,
            })
        };
        let own = aad("org_2xVq8LmN4rTb", environment, service, "SIGNING_KEY");
        assert_eq!(
            hex::encode(&own),
            "0000000f706c6f797a2d7365616c65642d7631000000106f72675f32785671384c6d4e34725462000000100e7c2f3a5b1d4c8e9f601a2b3c4d5e6f0000000773657276696365000000107d4e9a123c5b4f6e8a90b1c2d3e4f5a60000000b5349474e494e475f4b4559"
        );
        assert_eq!(
            key.open_value(&own, &sealed).unwrap().as_slice(),
            "línea uno 🔐\nzweite Zeile ✓\n".as_bytes()
        );

        for other in [
            aad("org_other", environment, service, "SIGNING_KEY"),
            aad("org_2xVq8LmN4rTb", service, service, "SIGNING_KEY"),
            aad("org_2xVq8LmN4rTb", environment, environment, "SIGNING_KEY"),
            aad("org_2xVq8LmN4rTb", environment, service, "OTHER_KEY"),
        ] {
            assert!(key.open_value(&other, &sealed).is_none());
        }
        assert!(SealingKey::generate().open_value(&own, &sealed).is_none());
        let bytes = sealed.as_bytes();
        let mut truncated = bytes.to_vec();
        truncated.pop();
        assert!(
            key.open_value(&own, &Ciphertext::from_bytes(truncated))
                .is_none()
        );
        let mut flipped = bytes.to_vec();
        *flipped.last_mut().unwrap() ^= 0x01;
        assert!(
            key.open_value(&own, &Ciphertext::from_bytes(flipped))
                .is_none()
        );
    }

    /// The Private Access vector `dashboard/src/modules/variables/seal-value.test.ts`
    /// seals in the browser with `sealPlatform`. Test keys only.
    #[test]
    fn an_auth_key_the_browser_sealed_opens_only_for_private_access() {
        let key = SealingKey(
            hex::decode("347ace27753bb0420141f071765286281c5fb4c450a9811d4ab13b67ac3da146")
                .unwrap()
                .try_into()
                .unwrap(),
        );
        let sealed = Ciphertext::try_from(
            "aQ3JpLLRSRFxTny/uywru4DsdBNaz2ed/8puzj80FnVgGTBxgtUPPvJq60VaDYOLiLov+SoN+uqbTWZL7UG7g0lob9gVuQq8vl0PFXHHIyTb5VnuvCj0"
                .to_owned(),
        )
        .unwrap();
        assert_eq!(
            hex::encode(PlatformPurpose::PrivateAccess.associated_data(&key.public_key())),
            "00000018706c6f797a2d706c6174666f726d2d7365616c65642d76310000000e707269766174652d6163636573730000002035d895d5063d3f7672a02f36600fc5681b34ce55931bf097535b4b7dc4b40838"
        );
        assert_eq!(
            key.open_platform(PlatformPurpose::PrivateAccess, &sealed)
                .unwrap()
                .as_slice(),
            b"tskey-auth-kTEST1CNTRL-frozenVectorOnly"
        );
        assert!(
            key.open_platform(PlatformPurpose::IngressTunnel, &sealed)
                .is_none()
        );
        assert!(
            SealingKey::generate()
                .open_platform(PlatformPurpose::PrivateAccess, &sealed)
                .is_none()
        );
    }
}
