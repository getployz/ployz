//! The one HPKE suite every sealed form here uses: DHKEM(X25519, HKDF-SHA256), HKDF-SHA256 and
//! ChaCha20-Poly1305, in one shot. A blob is the 32-byte encapsulated key, then the ciphertext
//! with its tag. What a seal is for lives in its `info` and associated data, named in
//! [`crate::separation`] and by each caller; this module only seals and opens.

use hpke::{
    Deserializable as _, OpModeR, OpModeS, Serializable as _, aead::ChaCha20Poly1305,
    kdf::HkdfSha256, kem::X25519HkdfSha256,
};
use rand_core::{OsRng, TryRngCore as _};
use thiserror::Error;
use zeroize::{Zeroize as _, Zeroizing};

type Kem = X25519HkdfSha256;

/// The length of the encapsulated key a blob starts with.
pub const ENCAPPED_KEY_LEN: usize = 32;

/// Who a seal is sent as.
#[derive(Clone, Copy)]
pub enum SealAs<'a> {
    /// No sender: anyone who knows the recipient's public key can make the blob.
    Base,
    /// The holder of this X25519 private key, so the blob opens only as sent by it.
    Auth(&'a [u8; 32]),
}

/// Who an open accepts a blob from.
#[derive(Clone, Copy)]
pub enum OpenAs<'a> {
    /// A blob with no sender. A blob sealed by a sender never opens this way.
    Base,
    /// A blob sent by the holder of the private key of this X25519 public key, and by no one
    /// else.
    Auth(&'a [u8; 32]),
}

/// HPKE refused to seal: the recipient's key is of low order, or the sender's is unusable.
#[derive(Debug, Eq, Error, PartialEq)]
#[error("HPKE refused to seal")]
pub struct SealFailed;

/// Why a blob did not open.
#[derive(Debug, Eq, Error, PartialEq)]
pub enum OpenFailed {
    /// It is not an encapsulated key followed by ciphertext.
    #[error("not an encapsulated key followed by ciphertext")]
    Malformed,
    /// It was sealed to another recipient, by another sender or none, under another `info` or
    /// associated data, or was altered.
    #[error("does not open")]
    Unopenable,
}

#[must_use]
pub fn generate_private() -> Zeroizing<[u8; 32]> {
    let (private, _) = <Kem as hpke::Kem>::gen_keypair(&mut OsRng.unwrap_err());
    let mut bytes = private.to_bytes();
    let key: Zeroizing<[u8; 32]> = Zeroizing::new(bytes.into());
    bytes.as_mut_slice().zeroize();
    key
}

/// The public half of an X25519 private key, which is any 32 bytes.
#[must_use]
pub fn public_key(private: &[u8; 32]) -> [u8; 32] {
    let private = <Kem as hpke::Kem>::PrivateKey::from_bytes(private)
        .expect("an X25519 private key is any 32 bytes");
    <Kem as hpke::Kem>::sk_to_pk(&private).to_bytes().into()
}

/// `plaintext` sealed to `recipient` under `info` and `aad`, as the encapsulated key followed
/// by the ciphertext.
///
/// # Errors
/// [`SealFailed`] when `recipient` is of low order.
pub fn seal(
    mode: SealAs<'_>,
    recipient: &[u8; 32],
    info: &[u8],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, SealFailed> {
    let recipient = <Kem as hpke::Kem>::PublicKey::from_bytes(recipient).map_err(|_| SealFailed)?;
    let mode = match mode {
        SealAs::Base => OpModeS::Base,
        SealAs::Auth(sender) => {
            let private =
                <Kem as hpke::Kem>::PrivateKey::from_bytes(sender).map_err(|_| SealFailed)?;
            let public = <Kem as hpke::Kem>::sk_to_pk(&private);
            OpModeS::Auth((private, public))
        }
    };
    let (encapped, ciphertext) = hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem, _>(
        &mode,
        &recipient,
        info,
        plaintext,
        aad,
        &mut OsRng.unwrap_err(),
    )
    .map_err(|_| SealFailed)?;
    let mut sealed = encapped.to_bytes().to_vec();
    sealed.extend(ciphertext);
    Ok(sealed)
}

/// What `sealed` holds, opened by the holder of `recipient`'s private key under the same
/// `info` and `aad` it was sealed under.
///
/// # Errors
/// [`OpenFailed::Malformed`] when `sealed` is shorter than an encapsulated key, and
/// [`OpenFailed::Unopenable`] when it does not open.
pub fn open(
    mode: OpenAs<'_>,
    recipient: &[u8; 32],
    info: &[u8],
    aad: &[u8],
    sealed: &[u8],
) -> Result<Zeroizing<Vec<u8>>, OpenFailed> {
    let (encapped, ciphertext) = sealed
        .split_at_checked(ENCAPPED_KEY_LEN)
        .ok_or(OpenFailed::Malformed)?;
    let encapped =
        <Kem as hpke::Kem>::EncappedKey::from_bytes(encapped).map_err(|_| OpenFailed::Malformed)?;
    let mode = match mode {
        OpenAs::Base => OpModeR::Base,
        OpenAs::Auth(sender) => OpModeR::Auth(
            <Kem as hpke::Kem>::PublicKey::from_bytes(sender)
                .map_err(|_| OpenFailed::Unopenable)?,
        ),
    };
    let recipient = <Kem as hpke::Kem>::PrivateKey::from_bytes(recipient)
        .expect("an X25519 private key is any 32 bytes");
    hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
        &mode, &recipient, &encapped, info, ciphertext, aad,
    )
    .map(Zeroizing::new)
    .map_err(|_| OpenFailed::Unopenable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blob_opens_for_its_recipient_under_its_labels_and_sender_only() {
        let recipient = generate_private();
        let sender = generate_private();
        let sender_public = public_key(&sender);
        let sealed = seal(
            SealAs::Auth(&sender),
            &public_key(&recipient),
            b"info",
            b"aad",
            b"plain",
        )
        .unwrap();
        assert_eq!(sealed.len(), ENCAPPED_KEY_LEN + 5 + 16);

        let open = |mode, recipient: &[u8; 32], info: &[u8], aad: &[u8]| {
            open(mode, recipient, info, aad, &sealed)
        };
        assert_eq!(
            open(OpenAs::Auth(&sender_public), &recipient, b"info", b"aad")
                .unwrap()
                .as_slice(),
            b"plain"
        );
        for refused in [
            open(OpenAs::Base, &recipient, b"info", b"aad"),
            open(
                OpenAs::Auth(&public_key(&recipient)),
                &recipient,
                b"info",
                b"aad",
            ),
            open(
                OpenAs::Auth(&sender_public),
                &generate_private(),
                b"info",
                b"aad",
            ),
            open(OpenAs::Auth(&sender_public), &recipient, b"other", b"aad"),
            open(OpenAs::Auth(&sender_public), &recipient, b"info", b"other"),
        ] {
            assert_eq!(refused.unwrap_err(), OpenFailed::Unopenable);
        }
    }

    #[test]
    fn a_blob_shorter_than_an_encapsulated_key_is_malformed_and_a_low_order_recipient_is_refused() {
        let recipient = generate_private();
        for len in 0..ENCAPPED_KEY_LEN {
            let blob = vec![1; len];
            assert_eq!(
                open(OpenAs::Base, &recipient, b"", b"", &blob).unwrap_err(),
                OpenFailed::Malformed
            );
        }
        let blob = vec![1; ENCAPPED_KEY_LEN];
        assert_eq!(
            open(OpenAs::Base, &recipient, b"", b"", &blob).unwrap_err(),
            OpenFailed::Unopenable
        );
        assert_eq!(
            seal(SealAs::Base, &[0; 32], b"", b"", b"plain"),
            Err(SealFailed)
        );
    }
}
