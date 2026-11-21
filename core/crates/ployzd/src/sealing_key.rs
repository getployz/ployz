//! The Cluster's Sealing Key as this Machine holds it.

use std::{fmt, fs, io, path::Path};

use ployz_core::{
    Ciphertext, PlatformPurpose, SealedSealingKey, Sealing, SealingPublicKey, WireGuardPublicKey,
    hpke_suite::{self, OpenAs, OpenFailed, SealAs},
    separation,
};
use thiserror::Error;
use zeroize::{Zeroize as _, Zeroizing};

use crate::corrosion::ClusterSealing;
use crate::network::WireGuardPrivateKey;

const FILE_NAME: &str = "sealing.key";

/// Each Service's floor in one Cluster, sealed beside its Sealing Key. Only the key's
/// holders can seal it, so a joiner installs it unchecked, as it installs the key.
pub(crate) type Floors =
    std::collections::BTreeMap<ployz_core::QualifiedService, ployz_core::authority::Timestamp>;
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
        Self(*hpke_suite::generate_private())
    }

    pub(crate) fn public_key(&self) -> SealingPublicKey {
        SealingPublicKey::from_bytes(hpke_suite::public_key(&self.0))
    }

    /// The Cluster's build attest key pair. Every Machine holding this Sealing Key derives
    /// the same one, and nothing without the Sealing Key can. It seals build secrets to a
    /// rule build, which writes its public half into the image token.
    pub(crate) fn attest(&self) -> ployz_core::X25519Secret {
        let mut key = Zeroizing::new([0; 32]);
        hkdf::Hkdf::<sha2_0_10::Sha256>::new(None, &self.0)
            .expand(separation::BUILD_SECRETS_ATTEST, key.as_mut())
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        ployz_core::X25519Secret::from_bytes(*key)
    }

    /// The MAC a verified build is, keyed under a label nothing else uses. Every Machine
    /// holding this Sealing Key derives the same key, and nothing without it can.
    pub(crate) fn verified_build(&self) -> hkdf::hmac::Hmac<sha2_0_10::Sha256> {
        use hkdf::hmac::Mac as _;
        let mut key = Zeroizing::new([0; 32]);
        hkdf::Hkdf::<sha2_0_10::Sha256>::new(None, &self.0)
            .expand(separation::VERIFIED_BUILD_KEY, key.as_mut())
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        hkdf::hmac::Hmac::<sha2_0_10::Sha256>::new_from_slice(key.as_ref())
            .expect("HMAC takes a key of any length")
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

    /// A seal with no join approval in it, which no joiner accepts: tests use it to build
    /// one.
    #[cfg(test)]
    pub(crate) fn seal_to(
        &self,
        recipient: &WireGuardPublicKey,
    ) -> Result<SealedSealingKey, SealError> {
        self.seal(SealAs::Base, separation::SEALING_KEY, recipient)
    }

    #[cfg(test)]
    pub(crate) fn open(
        sealed: &SealedSealingKey,
        wireguard: &WireGuardPrivateKey,
    ) -> Result<Self, OpenError> {
        Self::open_with(OpenAs::Base, separation::SEALING_KEY, sealed, wireguard)
    }

    /// Seal the key to `recipient` so only `sender`'s WireGuard key could
    /// have produced the blob, with `floors` after it for a recipient that opens them.
    ///
    /// # Errors
    /// Refuses a low-order recipient key, which no HPKE secret can be derived
    /// from.
    pub(crate) fn seal_from(
        &self,
        sender: &WireGuardPrivateKey,
        recipient: &WireGuardPublicKey,
        floors: Option<&Floors>,
    ) -> Result<SealedSealingKey, SealError> {
        let mut payload = Zeroizing::new(self.0.to_vec());
        if let Some(floors) = floors {
            payload.extend(serde_json::to_vec(floors).expect("floors serialize"));
        }
        Self::seal_bytes(
            &payload,
            SealAs::Auth(sender.as_bytes()),
            separation::SEALING_KEY_SHARE,
            recipient,
        )
    }

    /// Open a blob that [`Self::seal_from`] sealed to `recipient` from the
    /// Machine whose WireGuard key is `sender`, with the floors sealed beside the key.
    ///
    /// # Errors
    /// Returns [`OpenError::Malformed`] when the blob is not an encapsulated
    /// key followed by ciphertext, and [`OpenError::Unopenable`] when it was
    /// sealed to another recipient, by another sender, or without a sender.
    pub(crate) fn open_from(
        sealed: &SealedSealingKey,
        recipient: &WireGuardPrivateKey,
        sender: &WireGuardPublicKey,
    ) -> Result<(Self, Floors), OpenError> {
        let payload = Self::open_bytes(
            OpenAs::Auth(&sender.0),
            separation::SEALING_KEY_SHARE,
            sealed,
            recipient,
        )?;
        let (key, rest) = Self::split(&payload)?;
        let mut values = serde_json::Deserializer::from_slice(rest).into_iter::<Floors>();
        let floors = values
            .next()
            .transpose()
            .map_err(|_| OpenError::Malformed)?;
        if values.next().is_some() {
            return Err(OpenError::Malformed);
        }
        Ok((key, floors.unwrap_or_default()))
    }

    /// The key at the head of an opened payload, and what follows it.
    fn split(payload: &[u8]) -> Result<(Self, &[u8]), OpenError> {
        let (bytes, rest) = payload.split_at_checked(32).ok_or(OpenError::Malformed)?;
        Ok((
            Self(bytes.try_into().map_err(|_| OpenError::Malformed)?),
            rest,
        ))
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
            .expand(separation::JOIN_CONFIRMATION, key.as_mut())
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        let mut mac = hkdf::hmac::Hmac::<sha2_0_10::Sha256>::new_from_slice(key.as_ref())
            .expect("HMAC takes a key of any length");
        mac.update(&approval.signing_input());
        mac
    }

    /// Seal the key and `approval` to `recipient`, with `floors` after them for a joiner
    /// that opens them. An older joiner refuses a payload with anything after the approval.
    pub(crate) fn seal_join(
        &self,
        recipient: &WireGuardPublicKey,
        approval: &ployz_core::authority::Signed<ployz_core::authority::JoinApproval>,
        floors: Option<&Floors>,
    ) -> Result<SealedSealingKey, SealError> {
        let mut payload = Zeroizing::new(self.0.to_vec());
        payload.extend(serde_json::to_vec(approval).expect("signed join serializes"));
        if let Some(floors) = floors {
            payload.extend(serde_json::to_vec(floors).expect("floors serialize"));
        }
        Self::seal_bytes(&payload, SealAs::Base, separation::SEALING_KEY, recipient)
    }

    pub(crate) fn open_join(
        sealed: &SealedSealingKey,
        recipient: &WireGuardPrivateKey,
    ) -> Result<
        (
            Self,
            Option<ployz_core::authority::Signed<ployz_core::authority::JoinApproval>>,
            Floors,
        ),
        OpenError,
    > {
        let payload = Self::open_bytes(OpenAs::Base, separation::SEALING_KEY, sealed, recipient)?;
        let (key, body) = Self::split(&payload)?;
        let mut values =
            serde_json::Deserializer::from_slice(body).into_iter::<serde_json::Value>();
        let mut next = || values.next().transpose().map_err(|_| OpenError::Malformed);
        let approval = next()?
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| OpenError::Malformed)?;
        let floors = next()?
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| OpenError::Malformed)?;
        if next()?.is_some() {
            return Err(OpenError::Malformed);
        }
        Ok((key, approval, floors.unwrap_or_default()))
    }

    #[cfg(test)]
    fn seal(
        &self,
        mode: SealAs<'_>,
        info: &[u8],
        recipient: &WireGuardPublicKey,
    ) -> Result<SealedSealingKey, SealError> {
        Self::seal_bytes(&self.0, mode, info, recipient)
    }

    fn seal_bytes(
        plaintext: &[u8],
        mode: SealAs<'_>,
        info: &[u8],
        recipient: &WireGuardPublicKey,
    ) -> Result<SealedSealingKey, SealError> {
        hpke_suite::seal(mode, &recipient.0, info, &[], plaintext)
            .map(SealedSealingKey::from_bytes)
            .map_err(|_| SealError)
    }

    #[cfg(test)]
    fn open_with(
        mode: OpenAs<'_>,
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
        mode: OpenAs<'_>,
        info: &[u8],
        sealed: &SealedSealingKey,
        recipient: &WireGuardPrivateKey,
    ) -> Result<Zeroizing<Vec<u8>>, OpenError> {
        hpke_suite::open(mode, recipient.as_bytes(), info, &[], sealed.as_bytes()).map_err(
            |failed| match failed {
                OpenFailed::Malformed => OpenError::Malformed,
                OpenFailed::Unopenable => OpenError::Unopenable,
            },
        )
    }

    /// Opens one env value sealed to this key under `aad`, or `None` when it
    /// does not open.
    pub(crate) fn open_value(&self, aad: &[u8], sealed: &Ciphertext) -> Option<Zeroizing<Vec<u8>>> {
        self.open_under(separation::SEALED_VALUE, aad, sealed)
    }

    /// Opens a value the platform sealed to this Cluster for `purpose`, or `None`
    /// when it does not open: sealed for another Cluster, another purpose, or as
    /// an env value.
    pub(crate) fn open_platform(
        &self,
        purpose: PlatformPurpose,
        sealed: &Ciphertext,
    ) -> Option<Zeroizing<Vec<u8>>> {
        let aad = purpose.associated_data(&self.public_key());
        self.open_under(separation::PLATFORM_SEALED, &aad, sealed)
    }

    /// Seal `plaintext` under `info` and `aad` to this key's public half.
    #[cfg(test)]
    pub(crate) fn seal_under(&self, info: &[u8], aad: &[u8], plaintext: &[u8]) -> Ciphertext {
        Ciphertext::from_bytes(
            hpke_suite::seal(
                SealAs::Base,
                &hpke_suite::public_key(&self.0),
                info,
                aad,
                plaintext,
            )
            .expect("sealing to a held key succeeds"),
        )
    }

    fn open_under(
        &self,
        info: &[u8],
        aad: &[u8],
        sealed: &Ciphertext,
    ) -> Option<Zeroizing<Vec<u8>>> {
        hpke_suite::open(OpenAs::Base, &self.0, info, aad, sealed.as_bytes()).ok()
    }

    #[cfg(test)]
    pub(crate) fn seal_value(&self, aad: &[u8], plaintext: &[u8]) -> Ciphertext {
        self.seal_under(separation::SEALED_VALUE, aad, plaintext)
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

    use hpke::{
        Deserializable, Kem as _, OpModeR, OpModeS, Serializable, aead::ChaCha20Poly1305,
        kdf::HkdfSha256, kem::X25519HkdfSha256,
    };
    use rand_core::{OsRng, TryRngCore as _};

    use super::*;
    use crate::test_dir::TestDir;

    type Kem = X25519HkdfSha256;
    const ENCAPPED_KEY_LEN: usize = hpke_suite::ENCAPPED_KEY_LEN;

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
        let sealed = key
            .seal_from(&holder, &recipient.public_key(), None)
            .unwrap();

        assert_eq!(
            SealingKey::open_from(&sealed, &recipient, &holder.public_key())
                .unwrap()
                .0,
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
        let shared = key
            .seal_from(&holder, &recipient.public_key(), None)
            .unwrap();
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
            key.seal_from(&holder, &WireGuardPublicKey([0; 32]), None)
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

    /// Bytes this key sealed before the suite and the labels moved into their registries,
    /// from fixed keys: the payload `SealingKey([7; 32])`, the recipient `[0x11; 32]` and
    /// the sender `[0x22; 32]`.
    mod pins {
        use hkdf::hmac::Mac as _;
        use ployz_core::authority::{
            Signed,
            join::{JoinAllocation, JoinApproval},
        };

        use super::*;

        const KEY: [u8; 32] = [7; 32];
        const RECIPIENT: [u8; 32] = [0x11; 32];
        const SENDER: [u8; 32] = [0x22; 32];
        const SENDER_PUBLIC: &str =
            "0faa684ed28867b97f4a6a2dee5df8ce974e76b7018e3f22a1c4cf2678570f20";
        const SEALED_TO: &str = "ea3b0f856fc2ea99673a7811280385623a71c30882ebeca9a31f318483edc8132afcf598a0847cd8deee04eb621cb87a930c50f4952e3f92d3d9e24867754784a778067ec2c1f9f633a64b4d100c8701";
        const SEALED_FROM: &str = "0ee7c345eb8fe674cbed09188ba49e727a0f2c6296ce6be1f77ff984a216ad18eb09c7ac16ef7ed91ea2485c9b37a9f0b0c809d7b375750b9d413455e7765281f903fcdadec44ef8993f23d6fd0b58be";
        const SEALED_UNDER: &str = "9795619d6ad1fd562f23b35681fb793cbc3f24dc7e52b48319ec0fff6a25f771d1f6c55d7f70f72a720683b3d2d80a27f26ac3a098";

        fn recipient() -> WireGuardPrivateKey {
            WireGuardPrivateKey::from_bytes(RECIPIENT)
        }

        fn sender() -> WireGuardPrivateKey {
            WireGuardPrivateKey::from_bytes(SENDER)
        }

        fn sealed(text: &str) -> SealedSealingKey {
            SealedSealingKey::from_bytes(hex::decode(text).unwrap())
        }

        fn oracle_open(
            sender: Option<[u8; 32]>,
            info: &[u8],
            aad: &[u8],
            sealed: &[u8],
        ) -> Option<Vec<u8>> {
            let (encapped, ciphertext) = sealed.split_at_checked(32)?;
            let encapped = <Kem as hpke::Kem>::EncappedKey::from_bytes(encapped).ok()?;
            let private = <Kem as hpke::Kem>::PrivateKey::from_bytes(&RECIPIENT).ok()?;
            let mode = match sender {
                Some(sender) => {
                    OpModeR::Auth(<Kem as hpke::Kem>::PublicKey::from_bytes(&sender).ok()?)
                }
                None => OpModeR::Base,
            };
            hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
                &mode, &private, &encapped, info, ciphertext, aad,
            )
            .ok()
        }

        fn oracle_seal(
            sender: Option<[u8; 32]>,
            recipient: [u8; 32],
            info: &[u8],
            plaintext: &[u8],
        ) -> SealedSealingKey {
            let recipient = <Kem as hpke::Kem>::PublicKey::from_bytes(&recipient).unwrap();
            let mode = match sender {
                Some(sender) => {
                    let private = <Kem as hpke::Kem>::PrivateKey::from_bytes(&sender).unwrap();
                    let public = Kem::sk_to_pk(&private);
                    OpModeS::Auth((private, public))
                }
                None => OpModeS::Base,
            };
            let (encapped, ciphertext) =
                hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem, _>(
                    &mode,
                    &recipient,
                    info,
                    plaintext,
                    &[],
                    &mut OsRng.unwrap_err(),
                )
                .unwrap();
            let mut sealed = encapped.to_bytes().to_vec();
            sealed.extend(ciphertext);
            SealedSealingKey::from_bytes(sealed)
        }

        fn mutations(sealed: &[u8]) -> Vec<Vec<u8>> {
            let mut all: Vec<Vec<u8>> = (0..sealed.len())
                .map(|len| sealed.get(..len).unwrap().to_vec())
                .collect();
            all.push([sealed, &[0]].concat());
            for at in 0..sealed.len() {
                for bit in 0..8 {
                    let mut flipped = sealed.to_vec();
                    *flipped.get_mut(at).unwrap() ^= 1 << bit;
                    all.push(flipped);
                }
            }
            all
        }

        fn approval() -> Signed<JoinApproval> {
            let signer = ployz_core::authority::Signer::from_seed(&[5; 32]);
            let issued_at = ployz_core::authority::Timestamp::now();
            let body = JoinApproval {
                cluster: SealingPublicKey::from_bytes([1; 32]),
                machine: JoinAllocation {
                    id: ployz_core::MachineId::parse("6".repeat(32)).unwrap(),
                    name: ployz_core::MachineName::parse("joiner").unwrap(),
                    subnet: "10.210.6.0/24".parse().unwrap(),
                    public_key: WireGuardPublicKey([6; 32]),
                    public_ip: None,
                    advertised_endpoints: vec![],
                    policy: ployz_core::InitialMachinePolicy {
                        labels: std::collections::BTreeMap::new(),
                        accepts_builds: true,
                        accepts_services: true,
                        accepts_ingress: true,
                    },
                },
                network: "10.210.0.0/16".parse().unwrap(),
                sealing_recipient: WireGuardPublicKey([6; 32]),
                devices: vec![signer.public().into()],
                passkeys: Vec::new(),
                admitting_machines: vec![],
                bootstrap: vec![],
                issued_at,
                expires_at: issued_at.plus(600_000),
                signer: signer.public().into(),
                proposal: None,
            };
            Signed::sign(&body, &signer).unwrap()
        }

        #[test]
        fn a_key_sealed_for_a_join_opens_in_base_mode_under_the_key_info_only() {
            let sealed = sealed(SEALED_TO);
            assert_eq!(SealingKey::open(&sealed, &recipient()).unwrap().0, KEY);
            let (joined, approval, _) = SealingKey::open_join(&sealed, &recipient()).unwrap();
            assert_eq!(joined.0, KEY);
            assert!(approval.is_none());
            let sender = sender().public_key();
            assert!(matches!(
                SealingKey::open_from(&sealed, &recipient(), &sender),
                Err(OpenError::Unopenable)
            ));
            assert!(matches!(
                SealingKey::open(&sealed, &sender_private()),
                Err(OpenError::Unopenable)
            ));
            for bytes in mutations(sealed.as_bytes()) {
                let mutated = SealedSealingKey::from_bytes(bytes);
                assert!(SealingKey::open(&mutated, &recipient()).is_err());
            }
        }

        fn sender_private() -> WireGuardPrivateKey {
            WireGuardPrivateKey::from_bytes([0x44; 32])
        }

        #[test]
        fn a_key_shared_between_machines_opens_in_auth_mode_under_the_share_info_only() {
            let sealed = sealed(SEALED_FROM);
            let sender_public =
                WireGuardPublicKey(hex::decode(SENDER_PUBLIC).unwrap().try_into().unwrap());
            assert_eq!(
                SealingKey::open_from(&sealed, &recipient(), &sender_public)
                    .unwrap()
                    .0
                    .0,
                KEY
            );
            assert!(matches!(
                SealingKey::open(&sealed, &recipient()),
                Err(OpenError::Unopenable)
            ));
            let other = sender_private().public_key();
            assert!(matches!(
                SealingKey::open_from(&sealed, &recipient(), &other),
                Err(OpenError::Unopenable)
            ));
            for bytes in mutations(sealed.as_bytes()) {
                let mutated = SealedSealingKey::from_bytes(bytes);
                assert!(SealingKey::open_from(&mutated, &recipient(), &sender_public).is_err());
            }
        }

        #[test]
        fn a_short_blob_is_malformed_until_it_holds_an_encapsulated_key_and_unopenable_after() {
            let sender_public = sender().public_key();
            for len in 0..32 {
                let blob = SealedSealingKey::from_bytes(vec![1; len]);
                assert!(matches!(
                    SealingKey::open_from(&blob, &recipient(), &sender_public),
                    Err(OpenError::Malformed)
                ));
                assert!(matches!(
                    SealingKey::open_join(&blob, &recipient()),
                    Err(OpenError::Malformed)
                ));
            }
            for len in 32..=48 {
                let blob = SealedSealingKey::from_bytes(vec![1; len]);
                assert!(matches!(
                    SealingKey::open_from(&blob, &recipient(), &sender_public),
                    Err(OpenError::Unopenable)
                ));
            }
        }

        #[test]
        fn each_seal_is_under_its_own_info_with_no_associated_data() {
            let key = SealingKey(KEY);
            let to = key.seal_to(&recipient().public_key()).unwrap();
            assert_eq!(
                oracle_open(None, b"ployz-sealing-key-v1", &[], to.as_bytes()).as_deref(),
                Some(&KEY[..])
            );
            let from = key
                .seal_from(&sender(), &recipient().public_key(), None)
                .unwrap();
            assert_eq!(
                oracle_open(
                    Some(sender().public_key().0),
                    b"ployz-sealing-key-share-v1",
                    &[],
                    from.as_bytes()
                )
                .as_deref(),
                Some(&KEY[..])
            );
            assert_eq!(to.as_bytes().len(), 32 + 32 + 16);
            assert_eq!(from.as_bytes().len(), 32 + 32 + 16);

            let public = recipient().public_key().0;
            let by_oracle = oracle_seal(None, public, b"ployz-sealing-key-v1", &KEY);
            assert_eq!(SealingKey::open(&by_oracle, &recipient()).unwrap().0, KEY);
            let by_oracle = oracle_seal(Some(SENDER), public, b"ployz-sealing-key-share-v1", &KEY);
            assert_eq!(
                SealingKey::open_from(&by_oracle, &recipient(), &sender().public_key())
                    .unwrap()
                    .0
                    .0,
                KEY
            );
        }

        #[test]
        fn a_join_seal_carries_the_key_then_the_signed_approval_under_the_key_info() {
            let key = SealingKey(KEY);
            let approval = approval();
            let sealed = key
                .seal_join(&recipient().public_key(), &approval, None)
                .unwrap();
            let opened =
                oracle_open(None, b"ployz-sealing-key-v1", &[], sealed.as_bytes()).unwrap();
            assert_eq!(opened.get(..32), Some(&KEY[..]));
            assert_eq!(
                opened.get(32..).unwrap(),
                serde_json::to_vec(&approval).unwrap()
            );
            let (joined, opened, _) = SealingKey::open_join(&sealed, &recipient()).unwrap();
            assert_eq!(joined.0, KEY);
            assert_eq!(opened.unwrap(), approval);
            assert!(matches!(
                SealingKey::open(&sealed, &recipient()),
                Err(OpenError::Malformed)
            ));
        }

        /// A joiner that keeps floors takes them from either seal, after the key; one
        /// sealed without them reads as no floors.
        #[test]
        fn a_join_or_share_seal_carries_the_floors_sealed_after_the_key() {
            let key = SealingKey(KEY);
            let approval = approval();
            let floors = Floors::from([(
                ployz_core::QualifiedService::parse("app/api").unwrap(),
                ployz_core::authority::Timestamp::from_millis(1_800_000_000_000).unwrap(),
            )]);
            let joined = key
                .seal_join(&recipient().public_key(), &approval, Some(&floors))
                .unwrap();
            let (_, approved, carried) = SealingKey::open_join(&joined, &recipient()).unwrap();
            assert_eq!(approved, Some(approval));
            assert_eq!(carried, floors);
            let shared = key
                .seal_from(&sender(), &recipient().public_key(), Some(&floors))
                .unwrap();
            let (_, carried) =
                SealingKey::open_from(&shared, &recipient(), &sender().public_key()).unwrap();
            assert_eq!(carried, floors);
            let bare = key
                .seal_from(&sender(), &recipient().public_key(), None)
                .unwrap();
            let (_, carried) =
                SealingKey::open_from(&bare, &recipient(), &sender().public_key()).unwrap();
            assert!(carried.is_empty());
        }

        #[test]
        fn a_join_seal_with_a_body_that_is_not_an_approval_is_malformed() {
            let by_oracle = oracle_seal(
                None,
                recipient().public_key().0,
                b"ployz-sealing-key-v1",
                &[KEY.as_slice(), b"not json"].concat(),
            );
            assert!(matches!(
                SealingKey::open_join(&by_oracle, &recipient()),
                Err(OpenError::Malformed)
            ));
            let short = oracle_seal(
                None,
                recipient().public_key().0,
                b"ployz-sealing-key-v1",
                &KEY[..31],
            );
            assert!(matches!(
                SealingKey::open_join(&short, &recipient()),
                Err(OpenError::Malformed)
            ));
        }

        #[test]
        fn a_join_confirmation_is_a_mac_of_the_approval_under_its_own_derived_key() {
            let key = SealingKey(KEY);
            let approval = approval();
            let mut derived = [0; 32];
            hkdf::Hkdf::<sha2_0_10::Sha256>::new(None, &KEY)
                .expand(b"ployz-join-confirmation-v1", &mut derived)
                .unwrap();
            let mut mac = hkdf::hmac::Hmac::<sha2_0_10::Sha256>::new_from_slice(&derived).unwrap();
            mac.update(&approval.signing_input());
            let expected: [u8; 32] = mac.finalize().into_bytes().into();

            let confirmation = key.confirm_join(&approval);
            assert_eq!(confirmation.0, expected);
            assert!(key.confirms_join(&approval, &confirmation));
            assert!(!SealingKey([8; 32]).confirms_join(&approval, &confirmation));
            let mut flipped = confirmation;
            flipped.0[0] ^= 1;
            assert!(!key.confirms_join(&approval, &flipped));
        }

        #[test]
        fn a_key_sealed_to_a_low_order_machine_is_refused_in_every_form() {
            let key = SealingKey(KEY);
            let zero = WireGuardPublicKey([0; 32]);
            assert!(key.seal_to(&zero).is_err());
            assert!(key.seal_from(&sender(), &zero, None).is_err());
            assert!(key.seal_join(&zero, &approval(), None).is_err());
        }

        #[test]
        fn a_value_sealed_under_labels_opens_for_those_labels_and_aad_only() {
            let key = SealingKey(KEY);
            let sealed = Ciphertext::from_bytes(hex::decode(SEALED_UNDER).unwrap());
            assert_eq!(
                key.open_under(b"info", b"aad", &sealed).unwrap().as_slice(),
                b"plain"
            );
            assert!(key.open_under(b"other", b"aad", &sealed).is_none());
            assert!(key.open_under(b"info", b"other", &sealed).is_none());
            for bytes in mutations(sealed.as_bytes()) {
                assert!(
                    key.open_under(b"info", b"aad", &Ciphertext::from_bytes(bytes))
                        .is_none()
                );
            }
        }

        #[test]
        fn a_value_is_sealed_to_the_key_in_base_mode_under_the_info_it_is_given() {
            let key = SealingKey(KEY);
            let sealed = key.seal_under(b"info", b"aad", b"plain");
            let private = <Kem as hpke::Kem>::PrivateKey::from_bytes(&KEY).unwrap();
            let (encapped, ciphertext) = sealed.as_bytes().split_at(32);
            let opened = hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
                &OpModeR::Base,
                &private,
                &<Kem as hpke::Kem>::EncappedKey::from_bytes(encapped).unwrap(),
                b"info",
                ciphertext,
                b"aad",
            )
            .unwrap();
            assert_eq!(opened, b"plain");

            let env = key.seal_value(b"aad", b"hunter2");
            let opened = hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
                &OpModeR::Base,
                &private,
                &<Kem as hpke::Kem>::EncappedKey::from_bytes(env.as_bytes().get(..32).unwrap())
                    .unwrap(),
                b"ployz-sealed-v1",
                env.as_bytes().get(32..).unwrap(),
                b"aad",
            )
            .unwrap();
            assert_eq!(opened, b"hunter2");
        }
    }
}
