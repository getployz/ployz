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

use crate::authority::Signer;
use crate::authority::keys::{MachineKey, machine_signing_input};
use crate::authority::rotation::sealed_key_binding;
use crate::{
    BuildSecretsError, Ciphertext, FoundingClaim, ManagementProof, OpenedSealed, OperationId,
    PlatformPurpose, RecipientSecret, SealError, SealedIdentity, SealingPublicKey,
    WireGuardPublicKey, X25519Secret, seal_platform, seal_value,
};

type Kem = X25519HkdfSha256;

const MACHINE: [u8; 32] = [0x11; 32];
const SENDER: [u8; 32] = [0x22; 32];
const SECRET: [u8; 32] = [0x33; 32];
const AUDIENCE: &str = "ployz-build:0123";

/// `RecipientSecret::from_secret(SECRET).seal_to(MACHINE's public key, b"binding")`.
const RECIPIENT_BASE: &str = "be1dfdc0d427add0431b42afbcf0012502df56aa4efd150bfbbd952b2c855d4bef31c82e01bce06b38e4ae4baf63da43d375ada2447b49de48b4847f23ac41da96c5255d52178198cd3355393f4f478e";
/// `RecipientSecret::from_secret(SECRET).seal_from(&SENDER, MACHINE's public key, b"binding")`.
const RECIPIENT_AUTH: &str = "9b173df3ff38c531d7ba3ee7ea717eb9c82630c5e1dd8977c8c77c96db48567eeba60d832642504e3b24a5b9867ae3968735c0f268d8412878e31f64584b218f763ddac82475a8f7c242400217bbfdf1";
/// `seal_for_build` from SENDER as the attest key to SECRET as the recipient, for `AUDIENCE`,
/// of the values in [`opened`].
const BUILD_SECRETS: &str = "6caac79b41ce676cf0d53d1e51ecb5d27c18929ff9b3d074bda7798272e1056612929cbd1f01e6e8c55aa0452cd97ac4f17085f28f271616a994efd86f7278d73147196024d228002cb7435c84f82b375922555fa7689c5d7afc8ff8bd8b183096357d7186ed363776b0a41f82abaa50f847edd92eb991caf9728d3bf341de1b091a";
const SENDER_PUBLIC: &str = "0faa684ed28867b97f4a6a2dee5df8ce974e76b7018e3f22a1c4cf2678570f20";

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2).unwrap(), 16).unwrap())
        .collect()
}

fn public_of(private: [u8; 32]) -> [u8; 32] {
    *RecipientSecret::from_secret(private).public().as_bytes()
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

fn machine_wg() -> WireGuardPublicKey {
    WireGuardPublicKey(public_of(MACHINE))
}

fn identity() -> SealedIdentity {
    SealedIdentity {
        organization: "org_1".into(),
        environment: Uuid::from_u128(2),
        service: Uuid::from_u128(3),
    }
}

#[test]
fn a_recipient_key_sealed_to_a_machine_opens_in_base_mode_only() {
    let sealed = Ciphertext::from_bytes(unhex(RECIPIENT_BASE));
    let opened = RecipientSecret::open(&MACHINE, b"binding", &sealed).unwrap();
    assert_eq!(opened.secret(), &SECRET);

    let sender = WireGuardPublicKey(public_of(SENDER));
    assert!(RecipientSecret::open_from(&MACHINE, &sender, b"binding", &sealed).is_none());
    assert!(RecipientSecret::open(&MACHINE, b"other", &sealed).is_none());
    assert!(RecipientSecret::open(&[0x44; 32], b"binding", &sealed).is_none());
    for bytes in mutations(sealed.as_bytes()) {
        let mutated = Ciphertext::from_bytes(bytes);
        assert!(RecipientSecret::open(&MACHINE, b"binding", &mutated).is_none());
    }
}

#[test]
fn a_recipient_key_sealed_from_a_machine_opens_in_auth_mode_for_that_sender_only() {
    let sealed = Ciphertext::from_bytes(unhex(RECIPIENT_AUTH));
    let sender = WireGuardPublicKey(unhex(SENDER_PUBLIC).try_into().unwrap());
    let opened = RecipientSecret::open_from(&MACHINE, &sender, b"binding", &sealed).unwrap();
    assert_eq!(opened.secret(), &SECRET);

    assert!(RecipientSecret::open(&MACHINE, b"binding", &sealed).is_none());
    let other = WireGuardPublicKey(public_of([0x55; 32]));
    assert!(RecipientSecret::open_from(&MACHINE, &other, b"binding", &sealed).is_none());
    assert!(RecipientSecret::open_from(&MACHINE, &sender, b"other", &sealed).is_none());
    for bytes in mutations(sealed.as_bytes()) {
        let mutated = Ciphertext::from_bytes(bytes);
        assert!(RecipientSecret::open_from(&MACHINE, &sender, b"binding", &mutated).is_none());
    }
}

#[test]
fn a_recipient_key_is_sealed_under_its_own_info_in_either_mode() {
    let secret = RecipientSecret::from_secret(SECRET);
    let base = secret.seal_to(&machine_wg(), b"binding").unwrap();
    assert_eq!(
        oracle_open(
            None,
            MACHINE,
            b"ployz-recipient-key-v1",
            b"binding",
            base.as_bytes()
        )
        .as_deref(),
        Some(&SECRET[..])
    );
    let auth = secret
        .seal_from(&SENDER, &machine_wg(), b"binding")
        .unwrap();
    assert_eq!(
        oracle_open(
            Some(public_of(SENDER)),
            MACHINE,
            b"ployz-recipient-key-v1",
            b"binding",
            auth.as_bytes()
        )
        .as_deref(),
        Some(&SECRET[..])
    );
    assert_eq!(base.as_bytes().len(), 32 + 32 + 16);
    assert_eq!(auth.as_bytes().len(), 32 + 32 + 16);

    let from_oracle = oracle_seal(
        None,
        public_of(MACHINE),
        b"ployz-recipient-key-v1",
        b"binding",
        &SECRET,
    );
    let opened =
        RecipientSecret::open(&MACHINE, b"binding", &Ciphertext::from_bytes(from_oracle)).unwrap();
    assert_eq!(opened.secret(), &SECRET);
    let from_oracle = oracle_seal(
        Some(SENDER),
        public_of(MACHINE),
        b"ployz-recipient-key-v1",
        b"binding",
        &SECRET,
    );
    let sender = WireGuardPublicKey(public_of(SENDER));
    let opened = RecipientSecret::open_from(
        &MACHINE,
        &sender,
        b"binding",
        &Ciphertext::from_bytes(from_oracle),
    )
    .unwrap();
    assert_eq!(opened.secret(), &SECRET);
}

#[test]
fn a_recipient_key_opens_only_when_its_plaintext_is_32_bytes() {
    for plaintext in [&[7; 33][..], &[7; 31][..]] {
        let sealed = oracle_seal(
            None,
            public_of(MACHINE),
            b"ployz-recipient-key-v1",
            b"b",
            plaintext,
        );
        assert!(RecipientSecret::open(&MACHINE, b"b", &Ciphertext::from_bytes(sealed)).is_none());
    }
}

#[test]
fn the_binding_of_a_sealed_recipient_key_and_the_aad_of_a_platform_value_are_framed_as_before() {
    let cluster = SealingPublicKey::from_bytes(public_of(MACHINE));
    let operation = OperationId::parse("a".repeat(32)).unwrap();
    assert_eq!(
        sealed_key_binding(&cluster, &operation),
        [
            b"ployz-recipient-key-v1".as_slice(),
            cluster.as_bytes(),
            "a".repeat(32).as_bytes()
        ]
        .concat()
    );
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
    let cluster = SealingPublicKey::from_bytes(public_of(MACHINE));
    let recipient = SealingPublicKey::from_bytes(public_of(SECRET));
    for (purpose, name) in [
        (PlatformPurpose::IngressTunnel, "ingress-tunnel"),
        (PlatformPurpose::PrivateAccess, "private-access"),
    ] {
        let sealed = seal_platform(&cluster, &recipient, purpose, b"platform-secret").unwrap();
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
        Err(SealError::Seal)
    ));
    assert!(matches!(
        seal_platform(&low_order, &low_order, PlatformPurpose::IngressTunnel, b"x"),
        Err(SealError::Seal)
    ));
    let secret = RecipientSecret::from_secret(SECRET);
    let zero = WireGuardPublicKey([0; 32]);
    assert!(matches!(
        secret.seal_to(&zero, b"binding"),
        Err(SealError::Seal)
    ));
    assert!(matches!(
        secret.seal_from(&SENDER, &zero, b"binding"),
        Err(SealError::Seal)
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
fn a_management_proof_and_a_machine_statement_sign_under_their_own_domains() {
    assert_eq!(
        ManagementProof::message(&[7; ManagementProof::EXPORTER_LEN]),
        [b"ployz management client proof v1".as_slice(), &[7; 32]].concat()
    );
    let key = Signer::from_seed(&[9; 32]).public();
    let input = machine_signing_input("kind", MachineKey::Cluster { key }, "{body}");
    let text = String::from_utf8(input).unwrap();
    assert!(text.starts_with("ployz-machine/v1\nkind\ncluster:"));
    assert!(text.ends_with("\n{body}"));
    assert_eq!(text.matches('\n').count(), 3);
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
    for (purpose, name) in [
        (Purpose::Workflow, "workflow"),
        (Purpose::Enforce, "enforce"),
        (Purpose::Rule, "rule"),
        (Purpose::Volume, "volume"),
    ] {
        assert_eq!(
            signing_input(purpose, ""),
            [b"ployz-authority/v1\n".as_slice(), name.as_bytes(), b"\n"].concat()
        );
    }
}
