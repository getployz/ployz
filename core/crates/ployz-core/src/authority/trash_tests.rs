use super::*;
use crate::{
    DeviceKey,
    authority::{
        keys::{OwnPrompt, Signer},
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

fn here() -> MachineId {
    MachineId::parse("1".repeat(32)).unwrap()
}

fn entry(byte: u8) -> TrashId {
    TrashId::from_bytes([byte; 16])
}

fn at(millis: u64) -> Timestamp {
    Timestamp::from_millis(millis).unwrap()
}

fn trusted(passkey: &Authenticator) -> TrustedKeys {
    TrustedKeys::new(
        vec![DeviceKey::from(device().public())],
        vec![TrustedPasskey {
            passkey: passkey.passkey(),
            approved_by: PasskeyApprover::Server,
        }],
        [],
    )
}

fn approval(action: TrashAction, ids: &[TrashId], signer: SignerKey) -> TrashApproval {
    TrashApproval::drafted(&TrashTerms {
        cluster: cluster(),
        machine: here(),
        action,
        ids: ids.to_vec(),
        issued_at: at(1_000),
        signer,
    })
}

fn by_device(action: TrashAction, ids: &[TrashId]) -> Signed<TrashApproval> {
    let signer = SignerKey::Device(device().public().into());
    Signed::sign(&approval(action, ids, signer), &device()).unwrap()
}

fn by_passkey(
    passkey: &Authenticator,
    action: TrashAction,
    ids: &[TrashId],
) -> Signed<TrashApproval> {
    let signed = Signed::unsigned(&approval(
        action,
        ids,
        SignerKey::Passkey(OwnPrompt::Passkey),
    ))
    .unwrap();
    let assertion = passkey.answer(&Answer {
        challenge: &signed.challenge(),
        ..HONEST
    });
    signed.answered(assertion)
}

fn facts(trusted: &TrustedKeys) -> TrashFacts<'_> {
    TrashFacts {
        trusted,
        cluster: Some(cluster()),
        machine: here(),
        now: at(2_000),
    }
}

fn empty(ids: &[TrashId]) -> TrashAsk<'_> {
    TrashAsk::Empty { ids, bulk: false }
}

fn mismatch(field: MismatchField) -> Result<Approver, AuthorityRefusal> {
    Err(AuthorityRefusal::Mismatch { field })
}

#[test]
fn a_passkey_or_device_approves_exactly_the_entries_it_names_on_this_server() {
    let passkey = Authenticator::es256(4);
    let trusted = trusted(&passkey);
    let ids = [entry(2), entry(3)];
    assert_eq!(
        verify(
            &by_passkey(&passkey, TrashAction::Empty, &ids),
            empty(&[entry(3), entry(2)]),
            &facts(&trusted),
        ),
        Ok(Approver::Passkey(passkey.passkey().code())),
        "the order the request names them in doesn't matter"
    );
    let restore = by_device(TrashAction::Restore, &ids);
    for id in ids {
        assert_eq!(
            verify(&restore, TrashAsk::Restore(id), &facts(&trusted)),
            Ok(Approver::Signing(device().public())),
            "each entry's restore is its own request"
        );
    }
}

#[test]
fn an_approval_for_another_server_is_refused() {
    let passkey = Authenticator::es256(4);
    let trusted = trusted(&passkey);
    let ids = [entry(2)];
    let signed = by_passkey(&passkey, TrashAction::Empty, &ids);
    let elsewhere = TrashFacts {
        machine: MachineId::parse("5".repeat(32)).unwrap(),
        ..facts(&trusted)
    };
    assert_eq!(
        verify(&signed, empty(&ids), &elsewhere),
        mismatch(MismatchField::ThisMachine)
    );
    let other_cluster = TrashFacts {
        cluster: Some(SealingPublicKey::from_bytes([8; 32])),
        ..facts(&trusted)
    };
    assert_eq!(
        verify(&signed, empty(&ids), &other_cluster),
        mismatch(MismatchField::Cluster)
    );
}

#[test]
fn an_entry_the_approval_doesnt_name_is_refused() {
    let passkey = Authenticator::es256(4);
    let trusted = trusted(&passkey);
    let signed = by_passkey(&passkey, TrashAction::Empty, &[entry(2)]);
    for (asked, why) in [
        (&[entry(2), entry(3)][..], "an extra entry"),
        (&[entry(3)][..], "another entry"),
        (&[][..], "no entry"),
        (&[entry(2), entry(2)][..], "an entry twice"),
    ] {
        assert_eq!(
            verify(&signed, empty(asked), &facts(&trusted)),
            mismatch(MismatchField::TrashEntries),
            "{why}"
        );
    }
    assert_eq!(
        verify(
            &signed,
            TrashAsk::Empty {
                ids: &[entry(2)],
                bulk: true
            },
            &facts(&trusted)
        ),
        mismatch(MismatchField::TrashEntries),
        "a bulk empty keeps pinned entries the person named, so it isn't what they signed"
    );
    let restore = by_passkey(&passkey, TrashAction::Restore, &[entry(2)]);
    assert_eq!(
        verify(&restore, TrashAsk::Restore(entry(3)), &facts(&trusted)),
        mismatch(MismatchField::TrashEntries)
    );
}

#[test]
fn an_approval_replayed_as_the_other_action_or_after_its_window_is_refused() {
    let passkey = Authenticator::es256(4);
    let trusted = trusted(&passkey);
    let ids = [entry(2)];
    assert_eq!(
        verify(
            &by_passkey(&passkey, TrashAction::Restore, &ids),
            empty(&ids),
            &facts(&trusted)
        ),
        mismatch(MismatchField::TrashAction),
        "a restore's approval empties nothing"
    );
    assert_eq!(
        verify(
            &by_passkey(&passkey, TrashAction::Empty, &ids),
            TrashAsk::Restore(entry(2)),
            &facts(&trusted)
        ),
        mismatch(MismatchField::TrashAction)
    );
    let late = TrashFacts {
        now: at(10_000_000),
        ..facts(&trusted)
    };
    assert_eq!(
        verify(
            &by_passkey(&passkey, TrashAction::Empty, &ids),
            empty(&ids),
            &late
        ),
        Err(AuthorityRefusal::Expired)
    );
}

#[test]
fn an_answer_to_other_entries_or_an_unknown_passkey_is_refused() {
    let passkey = Authenticator::es256(4);
    let trusted = trusted(&passkey);
    let mut swapped = by_passkey(&passkey, TrashAction::Empty, &[entry(2)]);
    swapped.body = Signed::unsigned(&approval(
        TrashAction::Empty,
        &[entry(3)],
        SignerKey::Passkey(OwnPrompt::Passkey),
    ))
    .unwrap()
    .body;
    assert!(
        verify(&swapped, empty(&[entry(3)]), &facts(&trusted)).is_err(),
        "a passkey's answer signs its own body, so Cloud can't swap the entries under it"
    );
    let stranger = Authenticator::es256(5);
    assert_eq!(
        verify(
            &by_passkey(&stranger, TrashAction::Empty, &[entry(2)]),
            empty(&[entry(2)]),
            &facts(&trusted)
        ),
        Err(AuthorityRefusal::UnknownPasskey)
    );
    assert_eq!(
        verify(
            &Signed::unsigned(&approval(
                TrashAction::Empty,
                &[entry(2)],
                SignerKey::Passkey(OwnPrompt::Passkey)
            ))
            .unwrap(),
            empty(&[entry(2)]),
            &facts(&trusted)
        ),
        Err(AuthorityRefusal::Unsigned {
            purpose: Purpose::Workflow
        })
    );
}
