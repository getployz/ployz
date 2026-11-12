use super::*;
use crate::{
    SealingPublicKey, WireGuardPublicKey,
    authority::{OwnPrompt, Statement as _, keys::Purpose},
};

fn member(fill: u8) -> Member {
    Member {
        id: MachineId::parse(char::from(b'0' + fill).to_string().repeat(32)).unwrap(),
        public_key: WireGuardPublicKey([fill; 32]),
        subnet: format!("10.210.{fill}.0/24").parse().unwrap(),
    }
}

fn operation() -> OperationId {
    OperationId::parse("a".repeat(32)).unwrap()
}

fn names() -> BTreeMap<MachineId, String> {
    BTreeMap::from([
        (member(1).id, "web-1".to_owned()),
        (member(2).id, "web 2\u{202e}".to_owned()),
    ])
}

#[test]
fn a_starting_set_draft_is_the_canonical_set_a_server_reads_with_its_lines() {
    let draft = StartingSet::draft(
        StartingSetTerms {
            operation: operation(),
            members: vec![member(2), member(1), member(2)],
            issued_at: Timestamp::from_millis(1_000).unwrap(),
            signer: SignerKey::Passkey(OwnPrompt::Passkey),
        },
        named(&names()),
    )
    .unwrap();
    let set: StartingSet = canonical::from_canonical(&draft.body).unwrap();
    assert_eq!(StartingSet::PURPOSE, Purpose::Enforce);
    assert_eq!(set.members, vec![member(1), member(2)]);
    assert_eq!(
        set.expires_at,
        Timestamp::from_millis(1_000 + MAX_LIFETIME_MILLIS).unwrap()
    );
    assert!(set.kept_keys.is_empty());
    assert_eq!(draft.lines, set.lines(named(&names())));
    let [head, web_1, web_2, retired] = draft.lines.as_slice() else {
        panic!("four lines: {:?}", draft.lines);
    };
    assert_eq!(
        head,
        "Approved: turn on device enforcement for good on these servers. There's no off."
    );
    assert!(web_1.starts_with("  web-1, key "));
    assert!(web_1.contains(&member(1).public_key.fingerprint()));
    assert!(web_2.starts_with("  \"web 2\\u{202e}\", key "), "{web_2}");
    assert_eq!(
        retired,
        "Approved: remove every device and passkey added while enforcement was off."
    );
}

#[test]
fn a_starting_set_shows_the_members_its_body_holds_not_the_names_alone() {
    let draft = StartingSet::draft(
        StartingSetTerms {
            operation: operation(),
            members: vec![member(3)],
            issued_at: Timestamp::from_millis(1).unwrap(),
            signer: SignerKey::Passkey(OwnPrompt::Passkey),
        },
        named(&names()),
    )
    .unwrap();
    let [head, only, _] = draft.lines.as_slice() else {
        panic!("three lines: {:?}", draft.lines);
    };
    assert_eq!(
        head,
        "Approved: turn on device enforcement for good on this server. There's no off."
    );
    assert!(only.contains(&member(3).id.to_string()));
}

#[test]
fn a_rotation_draft_names_its_recipient_and_only_its_members() {
    let rotation = SealingRotation {
        cluster: SealingPublicKey::from_bytes([1; 32]),
        operation: operation(),
        recipient: SealingPublicKey::from_bytes([2; 32]),
        members: vec![member(2), member(1)],
        signer: SignerKey::Passkey(OwnPrompt::Passkey),
    };
    let draft = rotation.clone().draft(named(&names())).unwrap();
    let signed: SealingRotation = canonical::from_canonical(&draft.body).unwrap();
    assert_eq!(signed.members, vec![member(1), member(2)]);
    assert_eq!(signed.recipient, rotation.recipient);
    assert_eq!(draft.lines, signed.lines(named(&names())));
    let [head, _, _, cluster, _] = draft.lines.as_slice() else {
        panic!("five lines: {:?}", draft.lines);
    };
    assert!(head.contains(&rotation.recipient.to_string()));
    assert!(cluster.contains(&rotation.cluster.to_string()));
}
