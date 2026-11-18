//! Pins the bytes every sealed form of ployz-core writes and reads: the HPKE suite, each
//! purpose's `info` and associated data, and the encapsulated-key framing. Fixtures were
//! sealed by the code before the suite and label registry moved. The oracles spell each label
//! as a literal and call `hpke` directly, so they stay true to the wire when the labels or
//! the suite are named elsewhere.

use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use hpke::{
    Deserializable as _, OpModeR, OpModeS, Serializable as _, aead::ChaCha20Poly1305,
    kdf::HkdfSha256, kem::X25519HkdfSha256,
};
use rand_core::{OsRng, TryRngCore as _};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::{
    BuildSecretsError, FoundingClaim, ManagementProof, OpenedSealed, PlatformPurpose, SealError,
    SealedIdentity, SealingPublicKey, WireGuardPublicKey, X25519Secret, seal_platform, seal_value,
};

type Kem = X25519HkdfSha256;

const MACHINE: [u8; 32] = [0x11; 32];
const SENDER: [u8; 32] = [0x22; 32];
const SECRET: [u8; 32] = [0x33; 32];
const AUDIENCE: &str = "ployz-build:0123";

/// `seal_for_build` from SENDER as the attest key to SECRET as the recipient, for `AUDIENCE`,
/// of the values in [`opened`].
const BUILD_SECRETS: &str = "6caac79b41ce676cf0d53d1e51ecb5d27c18929ff9b3d074bda7798272e1056612929cbd1f01e6e8c55aa0452cd97ac4f17085f28f271616a994efd86f7278d73147196024d228002cb7435c84f82b375922555fa7689c5d7afc8ff8bd8b183096357d7186ed363776b0a41f82abaa50f847edd92eb991caf9728d3bf341de1b091a";

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2).unwrap(), 16).unwrap())
        .collect()
}

fn public_of(private: [u8; 32]) -> [u8; 32] {
    *X25519Secret::from_bytes(private).recipient().as_bytes()
}

fn opened() -> OpenedSealed {
    OpenedSealed::new(
        BTreeMap::from([("NPM_TOKEN".into(), "marker-build-secret-42".into())]),
        vec![("PART".into(), "marker-part".into())],
    )
}

/// The aad a purpose frames: each field behind its four-byte big-endian length.
fn framed(fields: &[&[u8]]) -> Vec<u8> {
    let mut aad = Vec::new();
    for field in fields {
        aad.extend_from_slice(&u32::try_from(field.len()).unwrap().to_be_bytes());
        aad.extend_from_slice(field);
    }
    aad
}

/// Opens `sealed` the way every HPKE reader here does: the 32-byte encapsulated key, then
/// the ciphertext, under X25519, HKDF-SHA256 and ChaCha20-Poly1305.
fn oracle_open(
    sender: Option<[u8; 32]>,
    private: [u8; 32],
    info: &[u8],
    aad: &[u8],
    sealed: &[u8],
) -> Option<Vec<u8>> {
    let (encapped, ciphertext) = sealed.split_at_checked(32)?;
    let encapped = <Kem as hpke::Kem>::EncappedKey::from_bytes(encapped).ok()?;
    let private = <Kem as hpke::Kem>::PrivateKey::from_bytes(&private).ok()?;
    let mode = match sender {
        Some(sender) => OpModeR::Auth(<Kem as hpke::Kem>::PublicKey::from_bytes(&sender).ok()?),
        None => OpModeR::Base,
    };
    hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
        &mode, &private, &encapped, info, ciphertext, aad,
    )
    .ok()
}

/// Seals the way every HPKE writer here does, with a fresh ephemeral key.
fn oracle_seal(
    sender: Option<[u8; 32]>,
    recipient: [u8; 32],
    info: &[u8],
    aad: &[u8],
    plaintext: &[u8],
) -> Vec<u8> {
    let recipient = <Kem as hpke::Kem>::PublicKey::from_bytes(&recipient).unwrap();
    let mode = match sender {
        Some(sender) => {
            let private = <Kem as hpke::Kem>::PrivateKey::from_bytes(&sender).unwrap();
            let public = <Kem as hpke::Kem>::sk_to_pk(&private);
            OpModeS::Auth((private, public))
        }
        None => OpModeS::Base,
    };
    let (encapped, ciphertext) = hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem, _>(
        &mode,
        &recipient,
        info,
        plaintext,
        aad,
        &mut OsRng.unwrap_err(),
    )
    .unwrap();
    let mut sealed = encapped.to_bytes().to_vec();
    sealed.extend(ciphertext);
    sealed
}

/// Every form of `sealed` a reader must refuse: each prefix, a trailing byte, and each bit
/// flipped.
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

fn identity() -> SealedIdentity {
    SealedIdentity {
        organization: "org_1".into(),
        environment: Uuid::from_u128(2),
        service: Uuid::from_u128(3),
    }
}

#[test]
fn the_aad_of_a_platform_value_is_framed_as_before() {
    let cluster = SealingPublicKey::from_bytes(public_of(MACHINE));
    for (purpose, name) in [
        (PlatformPurpose::IngressTunnel, "ingress-tunnel"),
        (PlatformPurpose::PrivateAccess, "private-access"),
    ] {
        assert_eq!(
            purpose.associated_data(&cluster),
            framed(&[
                b"ployz-platform-sealed-v1",
                name.as_bytes(),
                cluster.as_bytes()
            ])
        );
    }
}

#[test]
fn a_sealed_value_is_sealed_under_the_env_info_and_its_variables_associated_data() {
    let recipient = SealingPublicKey::from_bytes(public_of(MACHINE));
    let identity = identity();
    let sealed = seal_value(&recipient, &identity, "TOKEN", "hunter2").unwrap();
    assert_eq!(sealed.sealed_for, identity.environment);
    let aad = framed(&[
        b"ployz-sealed-v1",
        b"org_1",
        Uuid::from_u128(2).as_bytes(),
        b"service",
        Uuid::from_u128(3).as_bytes(),
        b"TOKEN",
    ]);
    assert_eq!(sealed.value.associated_data(&identity), aad);
    assert_eq!(
        oracle_open(
            None,
            MACHINE,
            b"ployz-sealed-v1",
            &aad,
            sealed.value.sealed.as_bytes()
        )
        .as_deref(),
        Some(&b"hunter2"[..])
    );
    assert_eq!(sealed.value.sealed.as_bytes().len(), 32 + 7 + 16);
}

#[test]
fn a_platform_value_is_sealed_under_the_platform_info_and_its_purposes_associated_data() {
    let cluster = SealingPublicKey::from_bytes(public_of(SECRET));
    for (purpose, name) in [
        (PlatformPurpose::IngressTunnel, "ingress-tunnel"),
        (PlatformPurpose::PrivateAccess, "private-access"),
    ] {
        let sealed = seal_platform(&cluster, purpose, b"platform-secret").unwrap();
        let aad = framed(&[
            b"ployz-platform-sealed-v1",
            name.as_bytes(),
            cluster.as_bytes(),
        ]);
        assert_eq!(
            oracle_open(
                None,
                SECRET,
                b"ployz-platform-sealed-v1",
                &aad,
                sealed.as_bytes()
            )
            .as_deref(),
            Some(&b"platform-secret"[..])
        );
        assert_eq!(sealed.as_bytes().len(), 32 + 15 + 16);
    }
}

#[test]
fn a_value_sealed_to_a_low_order_key_is_refused_as_a_failed_seal() {
    let low_order = SealingPublicKey::from_bytes([0; 32]);
    assert!(matches!(
        seal_value(&low_order, &identity(), "TOKEN", "hunter2"),
        Err(SealError)
    ));
    assert!(matches!(
        seal_platform(&low_order, PlatformPurpose::IngressTunnel, b"x"),
        Err(SealError)
    ));
}

#[test]
fn build_secrets_open_in_auth_mode_for_the_attest_key_and_audience_they_were_sealed_for() {
    let recipient = X25519Secret::from_bytes(SECRET);
    let attest = X25519Secret::from_bytes(SENDER).attest();
    let sealed = unhex(BUILD_SECRETS);
    let opened = OpenedSealed::open_build(&sealed, &recipient, &attest, AUDIENCE).unwrap();
    assert_eq!(opened.get("NPM_TOKEN"), Some("marker-build-secret-42"));
    assert_eq!(
        serde_json::to_vec(&opened).unwrap(),
        serde_json::to_vec(&self::opened()).unwrap()
    );

    let other_attest = X25519Secret::from_bytes([0x55; 32]).attest();
    assert!(matches!(
        OpenedSealed::open_build(&sealed, &recipient, &other_attest, AUDIENCE),
        Err(BuildSecretsError::Unopenable)
    ));
    assert!(matches!(
        OpenedSealed::open_build(&sealed, &recipient, &attest, "ployz-build:other"),
        Err(BuildSecretsError::Unopenable)
    ));
    let other_recipient = X25519Secret::from_bytes([0x66; 32]);
    assert!(matches!(
        OpenedSealed::open_build(&sealed, &other_recipient, &attest, AUDIENCE),
        Err(BuildSecretsError::Unopenable)
    ));
    for bytes in mutations(&sealed) {
        assert!(matches!(
            OpenedSealed::open_build(&bytes, &recipient, &attest, AUDIENCE),
            Err(BuildSecretsError::Unopenable)
        ));
    }
}

#[test]
fn build_secrets_are_sealed_in_auth_mode_under_the_info_of_their_audience() {
    let recipient = X25519Secret::from_bytes(SECRET);
    let sender = X25519Secret::from_bytes(SENDER);
    let sealed = opened()
        .seal_for_build(&sender, &recipient.recipient(), AUDIENCE)
        .unwrap();
    let info = [b"ployz build secrets v1".as_slice(), AUDIENCE.as_bytes()].concat();
    let plaintext = oracle_open(Some(public_of(SENDER)), SECRET, &info, &[], &sealed).unwrap();
    assert_eq!(plaintext, serde_json::to_vec(&opened()).unwrap());
    assert!(oracle_open(None, SECRET, &info, &[], &sealed).is_none());

    let from_oracle = oracle_seal(Some(SENDER), public_of(SECRET), &info, &[], &plaintext);
    let back =
        OpenedSealed::open_build(&from_oracle, &recipient, &sender.attest(), AUDIENCE).unwrap();
    assert_eq!(back.get("NPM_TOKEN"), Some("marker-build-secret-42"));
    let base = oracle_seal(None, public_of(SECRET), &info, &[], &plaintext);
    assert!(matches!(
        OpenedSealed::open_build(&base, &recipient, &sender.attest(), AUDIENCE),
        Err(BuildSecretsError::Unopenable)
    ));
}

#[test]
fn build_secrets_that_open_but_are_not_a_set_of_values_are_malformed() {
    let recipient = X25519Secret::from_bytes(SECRET);
    let sender = X25519Secret::from_bytes(SENDER);
    let info = [b"ployz build secrets v1".as_slice(), AUDIENCE.as_bytes()].concat();
    let sealed = oracle_seal(Some(SENDER), public_of(SECRET), &info, &[], b"not json");
    assert!(matches!(
        OpenedSealed::open_build(&sealed, &recipient, &sender.attest(), AUDIENCE),
        Err(BuildSecretsError::Malformed)
    ));
}

#[test]
fn a_pairing_secret_names_its_founding_claim_under_its_own_domain() {
    let claim = FoundingClaim::of_pairing("ppair_fake");
    let digest = Sha256::digest([b"ployz founding claim v1\0".as_slice(), b"ppair_fake"].concat());
    assert_eq!(String::from(claim), STANDARD.encode(digest));
}

#[test]
fn a_management_proof_signs_under_its_own_domain() {
    assert_eq!(
        ManagementProof::message(&[7; ManagementProof::EXPORTER_LEN]),
        [b"ployz management client proof v1".as_slice(), &[7; 32]].concat()
    );
}

#[test]
fn a_join_code_and_a_key_fingerprint_are_digests_under_their_own_domains() {
    let machine = crate::MachineId::parse("6".repeat(32)).unwrap();
    let key = WireGuardPublicKey([6; 32]);
    // Computed apart from this code, with Python's hashlib.
    assert_eq!(
        crate::authority::join::code(machine, key),
        "uj4zvg5giruofv77wb2w"
    );
    assert_eq!(key.fingerprint(), "fptjg2oge4mcdzgx");
}

#[test]
fn an_authority_statement_signs_under_its_domain_and_purpose() {
    use crate::authority::keys::{Purpose, signing_input};

    assert_eq!(
        signing_input(Purpose::Join, "{body}"),
        b"ployz-authority/v1\njoin\n{body}".to_vec()
    );
    for (purpose, name) in [(Purpose::Workflow, "workflow"), (Purpose::Rule, "rule")] {
        assert_eq!(
            signing_input(purpose, ""),
            [b"ployz-authority/v1\n".as_slice(), name.as_bytes(), b"\n"].concat()
        );
    }
}
