use super::*;
use crate::{
    DeviceKey, MachineId, Namespace,
    authority::{
        keys::Signer,
        restart::OwnPrompt,
        session::{ActionFamily, Delegation, GrantBody, SessionGrant},
        trusted_key::tests::{Answer, Authenticator, HONEST},
        trusted_key::{PasskeyApprover, TrustedPasskey},
    },
};

fn device() -> Signer {
    Signer::from_seed(&[7; 32])
}

fn cluster() -> SealingPublicKey {
    SealingPublicKey::from_bytes([9; 32])
}

fn sealed() -> Ciphertext {
    Ciphertext::from_bytes(vec![3; 80])
}

fn at(millis: u64) -> Timestamp {
    Timestamp::from_millis(millis).unwrap()
}

fn trusted(passkey: &Authenticator) -> TrustedKeys {
    TrustedKeys {
        devices: vec![DeviceKey::from(device().public())],
        passkeys: vec![TrustedPasskey {
            passkey: passkey.passkey(),
            approved_by: PasskeyApprover::Server,
        }],
    }
}

fn approval(signer: SignerKey) -> PrivateAccessApproval {
    PrivateAccessApproval {
        cluster: cluster(),
        auth_key: AuthKeyDigest::of(&sealed()),
        issued_at: at(1_000),
        expires_at: at(1_000 + super::super::validity::MAX_LIFETIME_MILLIS),
        signer,
    }
}

fn by_device() -> Signed<PrivateAccessApproval> {
    Signed::sign(
        &approval(SignerKey::Device(device().public().into())),
        &device(),
    )
    .unwrap()
}

fn by_passkey(passkey: &Authenticator) -> Signed<PrivateAccessApproval> {
    let unsigned = Signed::unsigned(&approval(SignerKey::Passkey(OwnPrompt::Passkey))).unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &unsigned.challenge(),
        ..HONEST
    });
    unsigned.answered(assertion)
}

fn check(
    signed: &Signed<PrivateAccessApproval>,
    trusted: &TrustedKeys,
    sealed: &Ciphertext,
) -> Result<Approver, AuthorityRefusal> {
    verify(
        signed,
        &PrivateAccessFacts {
            cluster: Some(cluster()),
            trusted,
            sealed,
            now: at(2_000),
        },
    )
}

#[test]
fn a_device_or_a_passkeys_own_prompt_approves_the_key_it_names() {
    let passkey = Authenticator::es256(4);
    let trusted = trusted(&passkey);
    assert_eq!(
        check(&by_device(), &trusted, &sealed()),
        Ok(Approver::Device(device().public()))
    );
    assert_eq!(
        check(&by_passkey(&passkey), &trusted, &sealed()),
        Ok(Approver::Passkey(passkey.passkey().code()))
    );
}

#[test]
fn another_key_cluster_or_window_is_refused() {
    let passkey = Authenticator::es256(4);
    let trusted = trusted(&passkey);
    assert_eq!(
        check(&by_device(), &trusted, &Ciphertext::from_bytes(vec![4; 80])),
        Err(AuthorityRefusal::Mismatch {
            field: "an auth key"
        }),
        "an approval over one ciphertext admits no other"
    );
    let elsewhere = verify(
        &by_device(),
        &PrivateAccessFacts {
            cluster: Some(SealingPublicKey::from_bytes([8; 32])),
            trusted: &trusted,
            sealed: &sealed(),
            now: at(2_000),
        },
    );
    assert_eq!(
        elsewhere,
        Err(AuthorityRefusal::Mismatch { field: "a Cluster" })
    );
    let late = verify(
        &by_device(),
        &PrivateAccessFacts {
            cluster: Some(cluster()),
            trusted: &trusted,
            sealed: &sealed(),
            now: at(10_000_000),
        },
    );
    assert_eq!(late, Err(AuthorityRefusal::Expired));
}

#[test]
fn an_unknown_signer_or_passkey_is_refused() {
    let passkey = Authenticator::es256(4);
    let stranger = Signer::from_seed(&[8; 32]);
    let signed = Signed::sign(
        &approval(SignerKey::Device(stranger.public().into())),
        &stranger,
    )
    .unwrap();
    assert_eq!(
        check(&signed, &trusted(&passkey), &sealed()),
        Err(AuthorityRefusal::UnknownKey {
            key: stranger.public()
        })
    );
    assert_eq!(
        check(
            &by_passkey(&Authenticator::es256(5)),
            &trusted(&passkey),
            &sealed()
        ),
        Err(AuthorityRefusal::UnknownPasskey)
    );
}

#[test]
fn a_browser_key_under_a_session_grant_is_refused() {
    let passkey = Authenticator::es256(4);
    let browser = Signer::from_seed(&[6; 32]);
    let grant = GrantBody {
        browser_key: browser.public(),
        cluster: cluster(),
        environment: Namespace::parse("app").unwrap(),
        environment_id: uuid::Uuid::from_u128(1),
        machines: vec![MachineId::parse("1".repeat(32)).unwrap()],
        families: vec![ActionFamily::Deploy],
        issued_at: at(0),
        expires_at: at(3_000_000),
        delegation: Delegation::None,
    };
    let unsigned = Signed::unsigned(&grant).unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &unsigned.challenge(),
        ..HONEST
    });
    let grant = SessionGrant {
        body: unsigned.body.clone(),
        assertion,
    };
    let signed = Signed::sign_under(
        &approval(SignerKey::Browser {
            browser: browser.public(),
        }),
        &browser,
        grant,
    )
    .unwrap();
    assert_eq!(
        check(&signed, &trusted(&passkey), &sealed()),
        Err(AuthorityRefusal::Mismatch {
            field: "the signer"
        })
    );
}

#[test]
fn the_approval_signs_its_golden_canonical_body() {
    let body = Signed::unsigned(&approval(SignerKey::Passkey(OwnPrompt::Passkey)))
        .unwrap()
        .body;
    assert_eq!(
        body,
        r#"{"auth_key":"b46bd9542401ebbac0c8ebd84b1f68c3ea7bd953fc9ae765c19df423350ae57f","cluster":"CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=","expires_at":901000,"issued_at":1000,"signer":"passkey"}"#
    );
}
