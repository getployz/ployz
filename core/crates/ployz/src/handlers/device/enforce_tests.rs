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
        told: Told::Relayed,
        member: Member::from(&machine),
        report: None,
        identity: None,
        recipient: None,
        enforces: true,
    }
}

fn activated_under(server: Found, operation: OperationId) -> Found {
    Found {
        report: Some(ActivationReport {
            state: ActivationState::Activated,
            operation: Some(operation),
            legacy_members: Vec::new(),
            legacy_keys: Vec::new(),
            private_access: None,
        }),
        ..server
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

    let mut servers = servers;
    servers[1].told = Told::Itself(told::Own::for_test());
    let refused = pin_each(
        &servers,
        &reach,
        &Pinned {
            known: &known,
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
    let over_ssh = Found {
        told: Told::Itself(told::Own::for_test()),
        ..over_ssh
    };
    let servers = [known_server, over_ssh, typed, unpinned, old];
    let reach = reach_of(&servers.iter().collect::<Vec<_>>());
    let mut asked = Vec::new();

    let pinning = pin_each(
        &servers,
        &reach,
        &Pinned {
            known: &known,
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
        private_access: None,
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

#[test]
fn activation_names_a_kept_private_access_router_and_its_tailnet() {
    let named = kept_router(
        "web-1",
        &ployz_core::PrivateAccessKept {
            tailnet: Some("example.com".to_owned()),
        },
    );
    assert!(
        named.starts_with("web-1 keeps the Private Access router"),
        "{named}"
    );
    assert!(named.contains("tailnet example.com"), "{named}");
    let unnamed = kept_router("web-1", &ployz_core::PrivateAccessKept { tailnet: None });
    assert!(unnamed.contains("a tailnet it couldn't name"), "{unnamed}");
}

#[test]
fn enforce_signs_nothing_while_a_server_only_the_mesh_reaches_holds_older_keys() {
    let entry = found('1', "machine-1");
    let mut clean = found('2', "machine-2");
    clean.report = Some(ActivationReport {
        state: ActivationState::Awaiting,
        operation: None,
        legacy_members: Vec::new(),
        legacy_keys: Vec::new(),
        private_access: None,
    });
    let mut older = found('3', "machine-3");
    older.report = Some(ActivationReport {
        state: ActivationState::Awaiting,
        operation: None,
        legacy_members: Vec::new(),
        legacy_keys: vec![legacy(4)],
        private_access: None,
    });
    let servers = [entry, clean, older];
    let reach = reach_of(&servers.iter().collect::<Vec<_>>());
    let direct = BTreeSet::from([servers[0].machine_id]);

    assert!(refuse_unreachable(&[&servers[0], &servers[1]], &direct, &reach).is_ok());
    let refused = refuse_unreachable(&servers.iter().collect::<Vec<_>>(), &direct, &reach)
        .expect_err("a Server that would refuse the set stops the run");

    let message = refused.to_string();
    assert!(message.contains("machine-3"), "{message}");
    assert!(!message.contains("machine-2"), "{message}");
    assert!(message.contains("Nothing was signed"), "{message}");

    let direct = BTreeSet::from([servers[0].machine_id, servers[2].machine_id]);
    assert!(refuse_unreachable(&servers.iter().collect::<Vec<_>>(), &direct, &reach).is_ok());
}

#[test]
fn a_rerun_finishes_the_activation_the_servers_committed_rather_than_starting_another() {
    let first = OperationId::random();
    let committed = activated_under(found('1', "machine-1"), first);
    let stopped = found('2', "machine-2");
    let reach = reach_of(&[&committed, &stopped]);
    assert_eq!(
        resumed(&[&committed, &stopped], &reach).unwrap(),
        Some(first)
    );
    assert!(committed_under(&committed, first));
    assert!(
        !committed_under(&stopped, first),
        "only a Server that committed skips the restart"
    );
    assert!(!committed_under(&committed, OperationId::random()));
    assert_eq!(
        resumed(&[&found('1', "machine-1"), &stopped], &reach).unwrap(),
        None,
        "a first run signs a fresh operation"
    );
}

#[test]
fn a_rerun_signs_nothing_when_servers_committed_different_sets() {
    let one = activated_under(found('1', "machine-1"), OperationId::random());
    let other = activated_under(found('2', "machine-2"), OperationId::random());
    let reach = reach_of(&[&one, &other]);
    let refusal = resumed(&[&one, &other], &reach).unwrap_err().to_string();
    assert!(refusal.contains("machine-1"), "{refusal}");
    assert!(refusal.contains("machine-2"), "{refusal}");
    assert!(refusal.contains("Nothing was signed"), "{refusal}");
}

#[test]
fn a_rerun_shares_the_recipient_key_the_activated_servers_hold_rather_than_making_another() {
    let key = ployz_core::Keypair::generate().public();
    let holder = Found {
        recipient: Some(key),
        told: Told::Itself(told::Own::for_test()),
        ..found('1', "machine-1")
    };
    let lacking = found('2', "machine-2");
    match held_recipient(&[&lacking, &holder], None).unwrap() {
        Some(Recipient::Held { recipient, holder }) => {
            assert_eq!(recipient, key);
            assert_eq!(holder.machine_id, found('1', "machine-1").machine_id);
        }
        _ => panic!("the held key is shared"),
    }
    assert!(
        held_recipient(&[&lacking], None).unwrap().is_none(),
        "with no key held, a new one is made"
    );
    let split = Found {
        recipient: Some(ployz_core::Keypair::generate().public()),
        ..found('2', "machine-2")
    };
    let refusal = held_recipient(&[&holder, &split], None).err().unwrap();
    assert!(refusal.contains("different recipient keys"), "{refusal}");
}

/// Through Cloud's Server Access no Server answers for itself, so Cloud can report the real
/// Members with a recipient key of its own. A rerun must not sign or pin that key.
#[test]
fn a_rerun_takes_a_held_recipient_only_from_a_server_that_answers_for_itself_or_the_pin() {
    let planted = ployz_core::Keypair::generate().public();
    let one = Found {
        recipient: Some(planted),
        ..found('1', "machine-1")
    };
    let two = Found {
        recipient: Some(planted),
        ..found('2', "machine-2")
    };
    let refusal = held_recipient(&[&one, &two], None).err().unwrap();

    assert!(refusal.contains("Nothing was signed"), "{refusal}");
    let other = ployz_core::Keypair::generate().public();
    assert!(
        held_recipient(&[&one, &two], Some(other)).is_err(),
        "a pin naming another key vouches for nothing"
    );
    assert!(matches!(
        held_recipient(&[&one, &two], Some(planted)),
        Ok(Some(Recipient::Held { recipient, .. })) if recipient == planted
    ));
}

/// Before first activation, a Member Cloud added over the mesh can rewrite Server B's
/// replicated row. B has a saved SSH route, but a row the entry relays for B is the entry's
/// word, not B's: only a pin or the person's own check of its fingerprint includes it.
#[test]
fn a_row_the_entry_relays_for_a_server_with_its_own_route_pins_nothing() {
    let entry = Found {
        told: Told::Itself(told::Own::for_test()),
        ..found('1', "machine-1")
    };
    let mut forged = found('2', "machine-2");
    forged.member.public_key = WireGuardPublicKey([9; 32]);
    let servers = [entry, forged];
    let reach = reach_of(&servers.iter().collect::<Vec<_>>());

    let pinning = pin_each(
        &servers,
        &reach,
        &Pinned {
            known: &BTreeMap::new(),
            typed: &BTreeSet::new(),
        },
        never,
    )
    .unwrap();

    assert!(
        !pinning
            .newly
            .iter()
            .chain(pinning.included.iter().map(|server| &server.member))
            .any(|member| member.public_key == WireGuardPublicKey([9; 32])),
        "the forged key was pinned or signed"
    );
}

fn details_of(machine: ployz_core::Machine) -> MachineDetails {
    MachineDetails {
        activation: None,
        sealing_recipient: None,
        id: machine.id,
        public_key: machine.public_key,
        machine: Some(machine),
        phase: ployz_core::LocalMachinePhase::Participating,
        advertised_endpoints: Vec::new(),
        store_version: BTreeMap::new(),
        rtts: Vec::new(),
        management_clients: Vec::new(),
        passkeys: Vec::new(),
        storage: None,
        upgrade: None,
        telemetry: None,
        token: None,
        sealing: None,
        founding: Default::default(),
    }
}

fn describing(id: MachineId) -> ployz_core::ContractDescription {
    ployz_core::ContractDescription {
        attest: None,
        machine_id: id,
        protocol_major: ployz_core::PROTOCOL_MAJOR,
        daemon_version: "fixture".into(),
        capabilities: [ployz_core::CapabilityName::parse(ENFORCED_CAPABILITY).unwrap()].into(),
        sealing: None,
    }
}

/// A daemon on `path` describing itself as `id`, which answers every Inspect, whatever
/// its target, with `row`.
fn daemon_at(
    path: &std::path::Path,
    id: MachineId,
    row: ployz_core::Machine,
) -> tokio::runtime::Runtime {
    crate::connect::test_support::unix_daemon_describing(path, describing(id), move |body| {
        let row = row.clone();
        async move {
            #[expect(
                clippy::wildcard_enum_match_arm,
                reason = "any other call is the bug this fixture catches"
            )]
            match body {
                ployz_core::RpcRequestBody::Inspect(_) => {
                    Ok(ployz_core::RpcResponse::from(details_of(row)))
                }
                other => panic!("enforce sent {other:?}"),
            }
        }
    })
}

/// Entry Server machine-1 on its own socket in `root`, relaying a forged row for
/// `server`, with the route the person chose to `server` at `route`.
async fn entering(
    root: &std::path::Path,
    server: &ployz_core::Machine,
    route: &std::path::Path,
) -> (Client, tokio::runtime::Runtime) {
    let path = root.join("entry.sock");
    let forged = ployz_core::Machine {
        public_key: WireGuardPublicKey([9; 32]),
        ..server.clone()
    };
    let entry = daemon_at(&path, machine('1', "machine-1").id, forged);
    let client = crate::connect::test_support::unix_client(&path).await;
    let route = crate::context::Connection::unix(route)
        .unwrap()
        .with_machine_id(server.id);
    (
        client.with_full_routes(crate::cluster::FullRoutes::chosen(vec![route], false)),
        entry,
    )
}

/// A Server the person saved a connection to answers only over it. When that connection
/// fails, the entry's relayed row for it is no stand-in: the Server is left out.
#[tokio::test]
async fn a_server_whose_own_route_fails_is_left_out_rather_than_read_through_the_entry() {
    let server = machine('2', "machine-2");
    let root = tempfile::tempdir().unwrap();
    let (client, entry) = entering(root.path(), &server, &root.path().join("absent.sock")).await;

    let looked = found_on(client, server.id).await.unwrap();

    entry.shutdown_background();
    assert!(
        matches!(looked, Some(Looked::Silent)),
        "the relayed row was taken"
    );
}

/// Over its own route a Server reports its own key, whatever the entry relays for it.
#[tokio::test]
async fn a_server_with_its_own_route_is_read_over_it() {
    let server = machine('2', "machine-2");
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("own.sock");
    let own = daemon_at(&path, server.id, server.clone());
    let (client, entry) = entering(root.path(), &server, &path).await;

    let looked = found_on(client, server.id).await.unwrap();

    own.shutdown_background();
    entry.shutdown_background();
    let Some(Looked::Found(found)) = looked else {
        panic!("the Server answered over its own route");
    };
    assert!(found.told.is_own());
    assert_eq!(found.member.public_key, server.public_key);
}

/// A Cluster enrolled through Cloud has no saved SSH connections. With one `--connect` for
/// each Server, every Server answers for itself over its own, whatever the entry relays.
#[tokio::test]
async fn each_connect_reaches_its_own_server_rather_than_the_entrys_row_for_it() {
    let server = machine('2', "machine-2");
    let root = tempfile::tempdir().unwrap();
    let entry_path = root.path().join("entry.sock");
    let own_path = root.path().join("own.sock");
    let forged = ployz_core::Machine {
        public_key: WireGuardPublicKey([9; 32]),
        ..server.clone()
    };
    let entry = daemon_at(&entry_path, machine('1', "machine-1").id, forged);
    let own = daemon_at(&own_path, server.id, server.clone());
    let connections = [&entry_path, &own_path]
        .map(|path| crate::context::Connection::unix(path).unwrap())
        .to_vec();

    let client = route::connect_each(
        connections,
        std::sync::Arc::new(crate::connect::SystemConnector::default()),
    )
    .await
    .unwrap();
    let looked = found_on(client, server.id).await.unwrap();

    own.shutdown_background();
    entry.shutdown_background();
    let Some(Looked::Found(found)) = looked else {
        panic!("the Server answered over its own connection");
    };
    assert!(found.told.is_own());
    assert_eq!(found.member.public_key, server.public_key);
}
