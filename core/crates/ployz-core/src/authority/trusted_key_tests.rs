use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::Signer as _;
use sha2::{Digest, Sha256};

use super::*;
use crate::authority::{Signed, StartingSet, Timestamp};

const CHALLENGE: &[u8] = b"the canonical body's digest";

fn relying_party() -> RelyingParty {
    RelyingParty::of(&Url::parse("https://ployz.cloud/some/page").unwrap()).unwrap()
}

/// A passkey and the authenticator that answers for it.
enum Authenticator {
    Es256(p256::ecdsa::SigningKey),
    EdDsa(ed25519_dalek::SigningKey),
}

impl Authenticator {
    fn es256(seed: u8) -> Self {
        Self::Es256(p256::ecdsa::SigningKey::from_slice(&[seed; 32]).unwrap())
    }

    fn eddsa(seed: [u8; 32]) -> Self {
        Self::EdDsa(ed25519_dalek::SigningKey::from_bytes(&seed))
    }

    fn public_key(&self) -> PasskeyPublicKey {
        match self {
            Self::Es256(key) => PasskeyPublicKey::parse(
                PasskeyAlgorithm::Es256,
                key.verifying_key().to_sec1_point(false).as_bytes(),
            )
            .unwrap(),
            Self::EdDsa(key) => {
                PasskeyPublicKey::parse(PasskeyAlgorithm::Eddsa, key.verifying_key().as_bytes())
                    .unwrap()
            }
        }
    }

    fn passkey(&self) -> Passkey {
        Passkey {
            credential_id: credential(*self.public_key().as_bytes().last().unwrap()),
            public_key: self.public_key(),
            account: PasskeyAccount::parse("ada@example.com").unwrap(),
            relying_party: relying_party(),
        }
    }

    fn sign(&self, message: &[u8]) -> Vec<u8> {
        match self {
            Self::Es256(key) => {
                let signature: p256::ecdsa::Signature = key.sign(message);
                signature.to_der().as_bytes().to_vec()
            }
            Self::EdDsa(key) => key.sign(message).to_bytes().to_vec(),
        }
    }

    /// What the browser returns for `navigator.credentials.get`, built from parts a test can
    /// spoil one at a time.
    fn answer(&self, answer: &Answer) -> PasskeyAssertion {
        let client_data = serde_json::json!({
            "type": answer.kind,
            "challenge": URL_SAFE_NO_PAD.encode(answer.challenge),
            "origin": answer.origin,
            "crossOrigin": false,
        })
        .to_string()
        .into_bytes();
        let mut authenticator_data = Sha256::digest(answer.rp_id.as_bytes()).to_vec();
        authenticator_data.push(answer.flags);
        authenticator_data.extend_from_slice(&[0, 0, 0, 1]);
        let mut signed = authenticator_data.clone();
        signed.extend_from_slice(&Sha256::digest(&client_data));
        PasskeyAssertion {
            credential_id: self.passkey().credential_id,
            authenticator_data: Base64Url::new(authenticator_data),
            client_data_json: Base64Url::new(client_data),
            signature: Base64Url::new(self.sign(&signed)),
        }
    }
}

struct Answer<'a> {
    kind: &'a str,
    challenge: &'a [u8],
    origin: &'a str,
    rp_id: &'a str,
    flags: u8,
}

const HONEST: Answer<'static> = Answer {
    kind: "webauthn.get",
    challenge: CHALLENGE,
    origin: "https://ployz.cloud",
    rp_id: "ployz.cloud",
    flags: USER_PRESENT | USER_VERIFIED,
};

fn credential(tag: u8) -> CredentialId {
    CredentialId::try_from(Base64Url::new(vec![tag, 1, 2, 3])).unwrap()
}

fn trusting(authenticator: &Authenticator) -> TrustedKeys {
    TrustedKeys {
        devices: vec![DeviceKey::from_bytes([1; 32])],
        passkeys: vec![TrustedPasskey {
            passkey: authenticator.passkey(),
            approved_by: PasskeyApprover::Server,
        }],
    }
}

/// A trusted EdDSA passkey whose public key is the Ed25519 key of `seed`, the shape a
/// Device's key also has.
pub(crate) fn trusted_eddsa_passkey(seed: &[u8; 32]) -> TrustedPasskey {
    TrustedPasskey {
        passkey: Authenticator::eddsa(*seed).passkey(),
        approved_by: PasskeyApprover::Server,
    }
}

#[test]
fn an_es256_and_an_eddsa_passkey_each_approve_a_workflow() {
    for authenticator in [Authenticator::es256(7), Authenticator::eddsa([7; 32])] {
        let trusted = trusting(&authenticator);
        let approved = trusted
            .passkey_approval(Purpose::Workflow, &authenticator.answer(&HONEST), CHALLENGE)
            .unwrap();
        assert_eq!(approved.passkey, authenticator.passkey());
    }
}

#[test]
fn a_passkey_approves_nothing_but_workflows() {
    let authenticator = Authenticator::es256(7);
    let trusted = trusting(&authenticator);
    for purpose in [Purpose::Enforce, Purpose::Join] {
        assert_eq!(
            trusted.passkey_approval(purpose, &authenticator.answer(&HONEST), CHALLENGE),
            Err(AuthorityRefusal::ApproveOnly {
                code: authenticator.passkey().code(),
                purpose,
            }),
            "{purpose}"
        );
    }
}

#[test]
fn an_untrusted_passkey_approves_nothing() {
    let trusted = trusting(&Authenticator::es256(7));
    let stranger = Authenticator::es256(8);
    assert_eq!(
        trusted.passkey_approval(Purpose::Workflow, &stranger.answer(&HONEST), CHALLENGE),
        Err(AuthorityRefusal::UnknownPasskey)
    );
}

#[test]
fn each_spoiled_part_of_an_assertion_is_refused_by_name() {
    let authenticator = Authenticator::es256(7);
    let trusted = trusting(&authenticator);
    let cases = [
        (
            Answer {
                kind: "webauthn.create",
                ..HONEST
            },
            "an assertion type",
        ),
        (
            Answer {
                challenge: b"another body",
                ..HONEST
            },
            "a challenge",
        ),
        (
            Answer {
                origin: "https://evil.example",
                ..HONEST
            },
            "an origin",
        ),
        (
            Answer {
                rp_id: "evil.example",
                ..HONEST
            },
            "a relying party",
        ),
        (
            Answer {
                flags: USER_PRESENT,
                ..HONEST
            },
            "user verification",
        ),
    ];
    for (answer, field) in cases {
        assert_eq!(
            trusted.passkey_approval(Purpose::Workflow, &authenticator.answer(&answer), CHALLENGE),
            Err(AuthorityRefusal::Mismatch { field }),
            "{field}"
        );
    }
}

#[test]
fn a_signature_from_another_key_is_refused() {
    let authenticator = Authenticator::es256(7);
    let trusted = trusting(&authenticator);
    let mut assertion = authenticator.answer(&HONEST);
    let forged = Authenticator::es256(9).answer(&HONEST);
    assertion.signature = forged.signature;
    assert_eq!(
        trusted.passkey_approval(Purpose::Workflow, &assertion, CHALLENGE),
        Err(AuthorityRefusal::Mismatch {
            field: "a passkey signature"
        })
    );
}

#[test]
fn a_passkey_never_signs_an_envelope_even_when_its_key_is_ed25519() {
    let trusted = TrustedKeys {
        devices: Vec::new(),
        passkeys: vec![trusted_eddsa_passkey(&[5; 32])],
    };
    let signer = crate::authority::Signer::from_seed(&[5; 32]);
    let now = Timestamp::from_millis(100_000).unwrap();
    let enrollment = Signed::sign(
        &StartingSet {
            operation: crate::OperationId::random(),
            members: Vec::new(),
            issued_at: now,
            expires_at: now.plus(1_000),
            signer: signer.public().into(),
        },
        &signer,
    )
    .unwrap();
    assert_eq!(
        enrollment.verified_signers(trusted.signing_keys()),
        Err(AuthorityRefusal::UnknownKey {
            key: signer.public()
        })
    );
}

#[test]
fn a_public_key_round_trips_and_its_code_hashes_its_bytes() {
    let key = Authenticator::es256(7).public_key();
    let json = serde_json::to_value(key).unwrap();
    assert_eq!(json.get("algorithm"), Some(&serde_json::json!("es256")));
    assert_eq!(
        serde_json::from_value::<PasskeyPublicKey>(json).unwrap(),
        key
    );
    assert_eq!(key.code(), DeviceCode::hashing(key.as_bytes()));
    assert_ne!(
        key.code(),
        Authenticator::eddsa([7; 32]).public_key().code()
    );
}

#[test]
fn a_key_off_its_curve_or_of_the_wrong_length_is_refused() {
    let mut off_curve = [0; 65];
    off_curve[0] = 4;
    off_curve[64] = 1;
    assert!(PasskeyPublicKey::parse(PasskeyAlgorithm::Es256, &off_curve).is_err());
    assert!(PasskeyPublicKey::parse(PasskeyAlgorithm::Eddsa, &[1; 31]).is_err());
    let wire = serde_json::json!({ "algorithm": "es256", "key": "AAAA" });
    assert!(serde_json::from_value::<PasskeyPublicKey>(wire).is_err());
}

#[test]
fn an_account_that_could_rewrite_a_terminal_is_refused() {
    assert!(PasskeyAccount::parse("ada@example.com\u{1b}[2J").is_err());
    assert!(PasskeyAccount::parse("ada\u{202E}moc.elpmaxe@").is_err());
    assert!(PasskeyAccount::parse("ada\u{200B}@example.com").is_err());
    assert!(PasskeyAccount::parse("").is_err());
    assert!(PasskeyAccount::parse(&"a".repeat(255)).is_err());
    assert!(PasskeyAccount::parse("ada@example.com").is_ok());
}

#[test]
fn passkeys_sharing_a_credential_id_answer_only_for_their_own_signature() {
    let decoy = Authenticator::es256(7);
    let holder = Authenticator::eddsa([5; 32]);
    let mut decoy_passkey = decoy.passkey();
    decoy_passkey.credential_id = holder.passkey().credential_id;
    let trusted = TrustedKeys {
        devices: Vec::new(),
        passkeys: [decoy_passkey, holder.passkey()]
            .map(|passkey| TrustedPasskey {
                passkey,
                approved_by: PasskeyApprover::Server,
            })
            .into(),
    };
    let approved = trusted
        .passkey_approval(Purpose::Workflow, &holder.answer(&HONEST), CHALLENGE)
        .unwrap();
    assert_eq!(approved.passkey.code(), holder.passkey().code());
}

#[test]
fn an_inspect_answer_from_a_newer_daemon_keeps_the_passkeys_it_can_read() {
    let readable = serde_json::to_value(TrustedPasskey {
        passkey: Authenticator::es256(7).passkey(),
        approved_by: PasskeyApprover::Server,
    })
    .unwrap();
    let mut newer = readable.clone();
    set_field(
        &mut newer,
        &["approved_by"],
        serde_json::json!({ "by": "passkey", "code": "aaaaaaaaaa" }),
    );
    set_field(
        &mut newer,
        &["passkey", "transports"],
        serde_json::json!(["usb"]),
    );
    let mut unreadable = readable.clone();
    set_field(
        &mut unreadable,
        &["passkey", "public_key", "algorithm"],
        serde_json::json!("rs256"),
    );

    #[derive(Deserialize)]
    struct Details {
        #[serde(deserialize_with = "readable_passkeys")]
        passkeys: Vec<TrustedPasskey>,
    }
    let details: Details = serde_json::from_value(serde_json::json!({
        "passkeys": [readable, newer, unreadable],
    }))
    .unwrap();
    assert_eq!(
        details
            .passkeys
            .iter()
            .map(|trusted| trusted.approved_by)
            .collect::<Vec<_>>(),
        [PasskeyApprover::Server, PasskeyApprover::Unknown]
    );
}

/// Set the field at `path` in `value`, adding it when absent.
fn set_field(value: &mut serde_json::Value, path: &[&str], new: serde_json::Value) {
    let (last, parents) = path.split_last().unwrap();
    let parent = parents
        .iter()
        .fold(value, |value, key| value.get_mut(*key).unwrap());
    parent
        .as_object_mut()
        .unwrap()
        .insert((*last).to_owned(), new);
}
