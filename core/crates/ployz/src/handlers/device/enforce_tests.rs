use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    DeviceCode, DeviceKey, LegacyKey, OperationId, TrustedKeyKind, WireGuardPublicKey,
    authority::{Member, SignerKey, StartingSet, Timestamp},
};

use super::*;
use crate::global_fixtures::machine;

fn found(hex: char, name: &str) -> Found {
    let machine = machine(hex, name);
    Found {
        machine_id: machine.id,
        member: Member::from(&machine),
        report: None,
        identity: None,
        enforces: true,
    }
}

fn reach_of(servers: &[&Found]) -> Reach {
    Reach {
        asked: servers.iter().map(|server| server.machine_id).collect(),
        silent: Vec::new(),
        entry: None,
        names: servers
            .iter()
            .map(|server| {
                let name = machine(
                    server.machine_id.to_string().chars().next().unwrap(),
                    &format!(
                        "machine-{}",
                        server.machine_id.to_string().chars().next().unwrap()
                    ),
                )
                .name;
                (server.machine_id, name)
            })
            .collect(),
    }
}

fn never(_: &str, _: &Member) -> Result<bool, Error> {
    Ok(false)
}

fn device(seed: u8) -> DeviceCode {
    DeviceKey::from_bytes([seed; 32]).code()
}

fn legacy(seed: u8) -> LegacyKey {
    LegacyKey {
        code: device(seed),
        kind: TrustedKeyKind::Device,
        account: None,
    }
}

#[test]
fn enforce_refuses_a_substituted_identity_and_answer() {
    let honest = found('1', "machine-1");
    let mut answered = found('2', "machine-2");
    let pinned_member = answered.member.clone();
    answered.member.public_key = WireGuardPublicKey([9; 32]);
    let known = BTreeMap::from([
        (honest.machine_id, honest.member.clone()),
        (answered.machine_id, pinned_member.clone()),
    ]);
    let servers = [honest, answered];
    let reach = reach_of(&servers.iter().collect::<Vec<_>>());

    let refused = pin_each(
        &servers,
        &reach,
        &Pinned {
            known: &known,
            over_ssh: Some(servers[1].machine_id),
            typed: &BTreeSet::from([servers[1].member.public_key.fingerprint()]),
        },
        |_, _| Ok(true),
    )
    .err()
    .expect("a substituted key is refused");

    let message = refused.to_string();
    assert!(message.contains("machine-2"), "{message}");
    assert!(
        message.contains(&WireGuardPublicKey([9; 32]).fingerprint()),
        "{message}"
    );
    assert!(
        message.contains(&pinned_member.public_key.fingerprint()),
        "{message}"
    );
    assert!(message.contains("nothing was signed"), "{message}");
}

#[test]
fn enforce_leaves_out_an_unpinned_legacy_member() {
    let known_server = found('1', "machine-1");
    let over_ssh = found('2', "machine-2");
    let typed = found('3', "machine-3");
    let unpinned = found('4', "machine-4");
    let mut old = found('5', "machine-5");
    old.enforces = false;
    let known = BTreeMap::from([(known_server.machine_id, known_server.member.clone())]);
    let servers = [known_server, over_ssh, typed, unpinned, old];
    let reach = reach_of(&servers.iter().collect::<Vec<_>>());
    let mut asked = Vec::new();

    let pinning = pin_each(
        &servers,
        &reach,
        &Pinned {
            known: &known,
            over_ssh: Some(servers[1].machine_id),
            typed: &BTreeSet::from([servers[2].member.public_key.fingerprint()]),
        },
        |name, _| {
            asked.push(name.to_owned());
            never(name, &servers[0].member)
        },
    )
    .unwrap();

    let included: Vec<_> = pinning
        .included
        .iter()
        .map(|server| server.machine_id)
        .collect();
    assert_eq!(
        included,
        vec![
            servers[0].machine_id,
            servers[1].machine_id,
            servers[2].machine_id
        ]
    );
    assert_eq!(
        pinning.newly,
        vec![servers[1].member.clone(), servers[2].member.clone()]
    );
    assert_eq!(asked, vec!["machine-4".to_owned()]);
    let left: Vec<_> = pinning
        .outcomes
        .iter()
        .map(|outcome| {
            (
                outcome.machine_id,
                matches!(outcome.state, State::LeftOut(_)),
            )
        })
        .collect();
    assert_eq!(
        left,
        vec![(servers[3].machine_id, true), (servers[4].machine_id, true)]
    );
}

#[test]
fn enforce_lists_and_retires_legacy_keys() {
    let own = device(1);
    let mut first = found('1', "machine-1");
    let mut second = found('2', "machine-2");
    let report = |keys: Vec<LegacyKey>| ployz_core::ActivationReport {
        state: ployz_core::ActivationState::Awaiting,
        operation: None,
        legacy_members: Vec::new(),
        legacy_keys: keys,
    };
    first.report = Some(report(vec![legacy(1), legacy(2), legacy(3)]));
    second.report = Some(report(vec![legacy(2)]));
    let included = [&first, &second];
    let reach = reach_of(&included);

    let listed = legacy_keys(&included, &reach, own);

    assert_eq!(
        listed.keys().copied().collect::<Vec<_>>(),
        BTreeSet::from([device(2), device(3)])
            .into_iter()
            .collect::<Vec<_>>()
    );
    assert_eq!(
        listed.get(&device(2)).unwrap().1,
        vec!["machine-1".to_owned(), "machine-2".to_owned()]
    );
    let kept = kept_keys(&listed, |key| Ok(key.code == device(3))).unwrap();
    assert_eq!(kept, BTreeSet::from([device(3)]));
    assert!(kept_keys(&listed, |_| Ok(false)).unwrap().is_empty());
}

#[test]
fn set_summary_names_each_server_and_kept_key_before_signing() {
    let first = found('1', "machine-1");
    let second = found('2', "machine-2");
    let reach = Reach {
        asked: vec![first.machine_id, second.machine_id],
        silent: Vec::new(),
        entry: None,
        names: BTreeMap::from([(first.machine_id, machine('1', "machine-1").name)]),
    };
    let issued_at = Timestamp::now();
    let mut set = StartingSet {
        operation: OperationId::random(),
        members: vec![first.member.clone(), second.member.clone()],
        issued_at,
        expires_at: issued_at.plus(60_000),
        signer: SignerKey::Device(DeviceKey::from_bytes([1; 32])),
        kept_keys: Vec::new(),
    };
    assert_eq!(
        set_summary(&reach, &set),
        format!(
            "Signing the 2 Servers every Server trusts from now on: machine-1 ({}, {}, key {}), {} ({}, {}, key {}). Keeping older keys: none.",
            first.member.subnet,
            first.machine_id,
            first.member.public_key.fingerprint(),
            second.machine_id,
            second.member.subnet,
            second.machine_id,
            second.member.public_key.fingerprint()
        )
    );
    set.kept_keys = vec![device(3)];
    assert!(set_summary(&reach, &set).ends_with(&format!("Keeping older keys: {}.", device(3))));
}

#[test]
fn enforce_has_no_off() {
    let mut command = enforce_command(clap::Command::new("enforce"));
    let help = command.render_long_help().to_string();
    assert!(!help.contains("--off"), "{help}");
    assert!(
        command
            .try_get_matches_from_mut(["enforce", "--off"])
            .is_err()
    );
}
