use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};

use ployz_core::authority::trusted_key::PasskeyAlgorithm;
use ployz_core::{
    CapabilityName, ContractDescription, DeviceKey, DeviceSet, MachineList, MachineObservation,
    MembershipObservation, OpaquePayload, PROTOCOL_MAJOR, RpcRequestBody, RpcResponse,
    SET_DEVICE_CAPABILITY,
};
use tonic::{Request, Response, Status};

use super::*;
use crate::cloud_login::Secret;
use crate::global_fixtures::machine;

fn key(first: u8) -> DeviceKey {
    DeviceKey::from_bytes(std::array::from_fn(|index| first + index as u8))
}

fn id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

#[derive(Clone, Copy, PartialEq)]
enum Daemon {
    Approves,
    AlreadyApproved,
    Unreachable,
    PredatesDevices,
    /// Holds the Device at the code, which approved passkey 1 there.
    ApprovedPasskey,
    /// Holds no Device at the code and has never heard of passkeys.
    PredatesPasskeys,
}

async fn cluster(
    daemons: [Daemon; 3],
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| async move {
        let target = rpc
            .metadata()
            .get("machine")
            .map(|value| MachineId::parse(value.to_str().unwrap()).unwrap())
            .or(Some(id('1')));
        let daemon = ['1', '2', '3']
            .into_iter()
            .zip(daemons)
            .find_map(|(hex, daemon)| (target == Some(id(hex))).then_some(daemon));
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "the fixture answers only what a Device fan-out sends"
        )]
        let response = match rpc.into_inner().decode_request().unwrap().body {
            RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                enrollment: None,
                machines: ['1', '2', '3']
                    .map(|hex| {
                        MachineObservation::new(
                            machine(hex, &format!("machine-{hex}")),
                            MembershipObservation::Up,
                        )
                    })
                    .into(),
            }),
            RpcRequestBody::DescribeContract(_) => RpcResponse::from(ContractDescription {
                machine_id: target.unwrap(),
                protocol_major: PROTOCOL_MAJOR,
                daemon_version: "fixture".into(),
                capabilities: match daemon {
                    Some(Daemon::PredatesDevices) => Vec::new(),
                    Some(Daemon::PredatesPasskeys) => vec![SET_DEVICE_CAPABILITY],
                    _ => vec![
                        SET_DEVICE_CAPABILITY,
                        SET_PASSKEY_CAPABILITY,
                        REMOVE_TRUSTED_KEY_CAPABILITY,
                    ],
                }
                .into_iter()
                .map(|capability| CapabilityName::parse(capability).unwrap())
                .collect(),
                sealing: None,
            }),
            RpcRequestBody::SetDevice(_) if daemon == Some(Daemon::Unreachable) => {
                return Err(Status::unavailable("machine-2 is down"));
            }
            RpcRequestBody::Inspect(_) => {
                let target = target.unwrap();
                let listed = machine(target.to_string().chars().next().unwrap(), "fixture");
                RpcResponse::from(ployz_core::MachineDetails {
                    id: target,
                    public_key: listed.public_key,
                    machine: Some(listed),
                    phase: ployz_core::LocalMachinePhase::Participating,
                    advertised_endpoints: Vec::new(),
                    store_version: Default::default(),
                    rtts: Vec::new(),
                    management_clients: Vec::new(),
                    passkeys: if daemon == Some(Daemon::ApprovedPasskey) {
                        vec![ployz_core::authority::TrustedPasskey {
                            passkey: waiting(passkey_key(1)),
                            approved_by: PasskeyApprover::Device {
                                code: key(1).code(),
                            },
                        }]
                    } else {
                        Vec::new()
                    },
                    storage: None,
                    upgrade: None,
                    telemetry: None,
                    token: None,
                    sealing: None,
                    founding: Default::default(),
                })
            }
            RpcRequestBody::RemoveTrustedKey(RemoveTrustedKeyRequest { code }) => {
                RpcResponse::from(if code == key(1).code() {
                    TrustedKeyRemoved::Removed {
                        code,
                        kind: TrustedKeyKind::Device,
                    }
                } else if daemon == Some(Daemon::ApprovedPasskey) && code == passkey_key(1).code() {
                    TrustedKeyRemoved::Removed {
                        code,
                        kind: TrustedKeyKind::Passkey,
                    }
                } else {
                    TrustedKeyRemoved::NotPresent { code }
                })
            }
            RpcRequestBody::SetDevice(SetDeviceRequest::Remove { code })
                if daemon == Some(Daemon::PredatesPasskeys) =>
            {
                RpcResponse::from(DeviceSet::NotPresent { code })
            }
            RpcRequestBody::SetDevice(_) => {
                let code = key(0).code();
                RpcResponse::from(if daemon == Some(Daemon::AlreadyApproved) {
                    DeviceSet::AlreadyApproved { code }
                } else {
                    DeviceSet::Approved { code }
                })
            }
            other => panic!("unexpected request {other:?}"),
        };
        Ok(Response::new(response.encode().unwrap()))
    })
    .await
}

async fn approve_everywhere(client: &mut Client) -> Result<Settled, Error> {
    set_everywhere(client, |client, machine_id| {
        set_on::<op::SetDevice>(
            client,
            machine_id,
            SET_DEVICE_CAPABILITY,
            SetDeviceRequest::Approve { key: key(0) },
        )
    })
    .await
}

fn token(cloud: String) -> Credential {
    Credential::Token {
        cloud,
        token: Secret::new("ployz_secret".to_owned()),
        from: crate::cloud_account::TokenFrom::Env,
    }
}

#[tokio::test]
async fn request_shows_the_code_its_own_key_hashes_to() {
    let cloud = crate::cloud_login::tests::fake_cloud(|route, body| {
        assert_eq!(route, "POST /api/cli/device-requests");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(body).unwrap(),
            json!({ "key": key(0) })
        );
        (200, json!({ "code": key(0).code(), "expires_at": "x" }))
    });

    let requested = request_with(&token(cloud), key(0)).await.unwrap();

    assert_eq!(requested.code, key(0).code());
    assert_eq!(requested.expires_at, "x");
}

#[tokio::test]
async fn a_refused_request_writes_no_key_and_a_filed_one_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = key::path(&dir.path().join("config.yaml"));
    let refusing = crate::cloud_login::tests::fake_cloud(|_, _| {
        (
            403,
            json!({ "code": "FORBIDDEN", "message": "Only a signed-in person can request a Device. No changes made." }),
        )
    });

    request_at(&token(refusing), &path).await.unwrap_err();
    assert!(!path.exists(), "a refused request left {}", path.display());

    let filing = crate::cloud_login::tests::fake_cloud(|_, body| {
        let key = serde_json::from_str::<Pending>(body).unwrap().key;
        (200, json!({ "code": key.code(), "expires_at": "x" }))
    });
    let requested = request_at(&token(filing.clone()), &path).await.unwrap();
    let secret = key::load(&path).unwrap().unwrap();
    assert_eq!(requested.code, key::public(&secret).code());

    let again = request_at(&token(filing), &path).await.unwrap();
    assert_eq!(again.code, requested.code);
}

#[tokio::test]
async fn a_request_that_loses_a_race_files_the_key_the_winner_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let path = key::path(&dir.path().join("config.yaml"));
    let winner = key::generate();
    let filed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let cloud = crate::cloud_login::tests::fake_cloud({
        let (path, filed) = (path.clone(), filed.clone());
        move |_, body| {
            let key = serde_json::from_str::<Pending>(body).unwrap().key;
            filed.lock().unwrap().push(key);
            if !path.exists() {
                assert!(key::store(&path, &winner).unwrap());
            }
            (200, json!({ "code": key.code(), "expires_at": "x" }))
        }
    });

    let requested = request_at(&token(cloud), &path).await.unwrap();

    assert_eq!(requested.code, key::public(&winner).code());
    assert_eq!(key::load(&path).unwrap(), Some(winner));
    assert_eq!(filed.lock().unwrap().last(), Some(&key::public(&winner)));
}

#[tokio::test]
async fn request_refuses_a_reply_naming_another_code() {
    let cloud = crate::cloud_login::tests::fake_cloud(|_, _| {
        (200, json!({ "code": key(1).code(), "expires_at": "x" }))
    });

    let error = request_with(&token(cloud), key(0)).await.unwrap_err();

    let message = error.report().message;
    assert!(message.contains(key(0).code().as_str()), "{message}");
    assert!(message.contains(key(1).code().as_str()), "{message}");
}

#[tokio::test]
async fn a_key_that_does_not_hash_to_the_code_is_refused_before_any_machine() {
    let cloud = crate::cloud_login::tests::fake_cloud(|route, _| {
        assert_eq!(route, "GET /api/cli/device-requests/mmg42klgyq");
        (
            200,
            json!({ "code": "mmg42klgyq", "key": key(1), "expires_at": "x" }),
        )
    });
    let credential = token(cloud);
    let calls = AtomicUsize::new(0);
    let code = DeviceCode::parse("mmg42klgyq").unwrap();
    assert_eq!(key(0).code(), code);

    let error = approve_with(&credential, code, async |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .await
    .unwrap_err();

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(error.report().message.contains("doesn't match mmg42klgyq"));
}

#[tokio::test]
async fn one_failing_machine_is_a_partial_result_that_names_it() {
    let (mut client, server) =
        cluster([Daemon::Approves, Daemon::Unreachable, Daemon::Approves]).await;

    let settled = approve_everywhere(&mut client).await.unwrap();

    assert_eq!(settled.answers.len(), 2);
    let failed: Vec<_> = settled
        .gaps
        .failures
        .iter()
        .map(|failure| failure.machine_id)
        .collect();
    assert_eq!(failed, [id('2')]);
    assert!(settled.gaps.omitted.is_empty());
    assert!(settled.table().is_some());
    assert_eq!(settled.gaps.outcome().unwrap_err().printed_exit(), Some(3));
    server.abort();
}

#[tokio::test]
async fn when_no_machine_answers_only_the_gaps_print() {
    let (mut client, server) = cluster([Daemon::Unreachable; 3]).await;

    let settled = approve_everywhere(&mut client).await.unwrap();

    assert!(settled.answers.is_empty());
    assert_eq!(settled.gaps.failures.len(), 3);
    assert!(settled.table().is_none());
    assert_eq!(settled.gaps.outcome().unwrap_err().printed_exit(), Some(3));
    server.abort();
}

#[tokio::test]
async fn rerunning_where_every_machine_already_approved_succeeds() {
    let (mut client, server) = cluster([Daemon::AlreadyApproved; 3]).await;

    let settled = approve_everywhere(&mut client).await.unwrap();

    assert!(settled.answers.iter().all(|answer| matches!(
        answer.outcome.change,
        Change::Set(DeviceSet::AlreadyApproved { .. })
    )));
    assert_eq!(settled.answers.len(), 3);
    settled.gaps.outcome().unwrap();
    server.abort();
}

#[tokio::test]
async fn a_machine_without_a_device_roster_is_an_omission() {
    let (mut client, server) =
        cluster([Daemon::Approves, Daemon::PredatesDevices, Daemon::Approves]).await;

    let settled = approve_everywhere(&mut client).await.unwrap();

    assert_eq!(settled.gaps.omitted, [id('2')]);
    assert!(settled.gaps.failures.is_empty());
    assert_eq!(settled.answers.len(), 2);
    server.abort();
}

#[test]
fn a_device_key_other_users_can_read_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = key::path(&dir.path().join("config.yaml"));
    let secret = key::generate();
    assert!(key::store(&path, &secret).unwrap());
    assert!(!key::store(&path, &key::generate()).unwrap());
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(key::load(&path).unwrap(), Some(secret));

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let error = key::load(&path).unwrap_err().to_string();
    assert!(error.contains("chmod 600"), "{error}");
    assert!(error.contains(&path.display().to_string()), "{error}");
}

#[test]
fn each_device_command_parses_and_reaches_its_connection() {
    let cloud = crate::cloud_login::tests::fake_cloud(|route, _| {
        assert_eq!(
            route,
            format!("GET /api/cli/device-requests/{}", key(0).code())
        );
        (
            200,
            json!({ "code": key(0).code(), "key": key(0), "expires_at": "x" }),
        )
    });
    let dir = tempfile::tempdir().unwrap();
    let signed_in = json!({
        "state": "signed_in",
        "cloud": cloud,
        "token": "session-token",
        "account": { "id": "u1", "email": "dev@example.test", "name": "Dev" },
        "organization": { "id": "o1", "slug": "acme" },
    });
    std::fs::write(dir.path().join("cloud.json"), signed_in.to_string()).unwrap();
    let config = dir.path().join("config.yaml");
    let socket = dir.path().join("absent.sock");
    std::fs::write(
        &config,
        format!(
            "contexts:\n  staging:\n    connections:\n      - unix: {}\n",
            socket.display()
        ),
    )
    .unwrap();
    let connect = format!("unix://{}", socket.display());
    let code = key(0).code().to_string();
    for ((args, handler), via) in [
        (vec!["ls"], list as fn(&ArgMatches) -> Result<(), Error>),
        (vec!["rm", &code], remove),
        (vec!["approve", &code, "--yes"], super::approve),
    ]
    .into_iter()
    .flat_map(|leaf| {
        [["--connect", connect.as_str()], ["--context", "staging"]].map(|via| (leaf.clone(), via))
    }) {
        let root = crate::cli::command()
            .try_get_matches_from(
                [
                    "ployz",
                    "--ployz-config",
                    config.to_str().unwrap(),
                    "device",
                ]
                .into_iter()
                .chain(args.iter().copied())
                .chain(via),
            )
            .unwrap();

        let message = handler(&root).unwrap_err().report().message;

        assert_eq!(
            message, "Ployz couldn't reach your Server's daemon.",
            "{args:?} {via:?}"
        );
    }

    let unconfirmed = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "--ployz-config",
            config.to_str().unwrap(),
            "device",
            "approve",
            &code,
            "--connect",
            &connect,
        ])
        .unwrap();
    let refused = super::approve(&unconfirmed).unwrap_err().report();
    assert_eq!(refused.code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(
        refused.message,
        format!("Approving device {code} needs confirmation. Nothing changed.")
    );
}

#[tokio::test]
async fn automatic_join_approval_refuses_clouds_substituted_recipient() {
    use ployz_core::{
        PendingJoins, RegisterRequest, SealingPublicKey,
        authority::{JoinApproval, Signer, Timestamp, join::JoinAllocation},
    };
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.yaml");
    super::key::store(&super::key::path(&config), &[1; 32]).unwrap();
    let expected_machine = machine('a', "joiner");
    let expected = RegisterRequest {
        join_approval: None,
        machine_id: expected_machine.id,
        assigned_subnet: Some(expected_machine.subnet),
        initial_policy: Default::default(),
        name: expected_machine.name.clone(),
        storage: ployz_core::StorageChoice::None,
        public_key: expected_machine.public_key,
        public_ip: expected_machine.public_ip,
        advertised_endpoints: expected_machine.advertised_endpoints.clone(),
        runtime: Default::default(),
    };
    let mut allocation = JoinAllocation::from(&expected_machine);
    allocation.public_key = ployz_core::WireGuardPublicKey([9; 32]);
    let signer = Signer::from_seed(&[1; 32]);
    let issued_at = Timestamp::now();
    let body = JoinApproval {
        cluster: SealingPublicKey::from_bytes([2; 32]),
        machine: allocation,
        network: "10.210.0.0/16".parse().unwrap(),
        sealing_recipient: ployz_core::WireGuardPublicKey([9; 32]),
        devices: vec![signer.public().into()],
        bootstrap: Vec::new(),
        admitting_machines: vec![id('b')],
        issued_at,
        expires_at: issued_at.plus(900_000),
        signer: signer.public().into(),
    };
    let (mut client, server) =
        crate::connect::test_support::rpc_client(move |request: Request<OpaquePayload>| {
            let body = body.clone();
            async move {
                assert!(
                    matches!(
                        request.into_inner().decode_request().unwrap().body,
                        RpcRequestBody::PendingJoins(_)
                    ),
                    "a substituted key must never reach ApproveJoin"
                );
                Ok(Response::new(
                    RpcResponse::from(PendingJoins { joins: vec![body] })
                        .encode()
                        .unwrap(),
                ))
            }
        })
        .await;
    let error = sign_waiting_join(&mut client, &config, &expected)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("does not match"));
    server.abort();
}

#[tokio::test]
async fn full_mutations_report_servers_without_a_direct_connection_as_omissions() {
    let (client, server) = cluster([Daemon::Approves; 3]).await;
    let connection =
        crate::context::Connection::tcp("127.0.0.1:7569".parse().unwrap()).with_machine_id(id('1'));
    let mut client = client.with_full_connections(vec![connection]);
    let removed = set_everywhere(&mut client, |client, machine_id| {
        remove_on(client, machine_id, key(1).code())
    })
    .await
    .unwrap();
    assert_eq!(removed.answers.len(), 1);
    assert_eq!(removed.gaps.omitted, vec![id('2'), id('3')]);
    assert_eq!(removed.gaps.outcome().unwrap_err().printed_exit(), Some(3));
    let reach = Reach::full(&mut client).await.unwrap();
    let (_, gaps) = reach
        .fan_out(|_| async { Ok(Some(DeviceEnforcementSet { enforced: true })) })
        .await;
    assert_eq!(gaps.omitted, vec![id('2'), id('3')]);
    assert_eq!(gaps.outcome().unwrap_err().printed_exit(), Some(3));
    server.abort();
}

#[test]
fn join_summary_names_the_server_subnet_and_endpoints_before_signing() {
    use ployz_core::{
        AdvertisedEndpoint, SealingPublicKey,
        authority::{JoinApproval, Timestamp, join::JoinAllocation},
    };
    let mut joiner = machine('a', "joiner");
    joiner.subnet = "10.210.7.0/24".parse().unwrap();
    joiner.advertised_endpoints = vec![AdvertisedEndpoint("[2001:db8::7]:51820".parse().unwrap())];
    let issued_at = Timestamp::now();
    let body = JoinApproval {
        cluster: SealingPublicKey::from_bytes([2; 32]),
        machine: JoinAllocation::from(&joiner),
        network: "10.210.0.0/16".parse().unwrap(),
        sealing_recipient: joiner.public_key,
        devices: Vec::new(),
        bootstrap: Vec::new(),
        admitting_machines: vec![id('b')],
        issued_at,
        expires_at: issued_at.plus(900_000),
        signer: key(1),
    };
    assert_eq!(
        join_summary(&body),
        format!(
            "Approving Server joiner on 10.210.7.0/24 with endpoints [2001:db8::7]:51820 (join code {}).",
            body.code()
        )
    );
}

#[test]
fn starting_set_summary_names_each_trusted_server_before_signing() {
    let first = machine('1', "machine-1");
    let second = machine('2', "machine-2");
    let reach = Reach {
        asked: vec![first.id, second.id],
        silent: Vec::new(),
        entry: None,
        names: BTreeMap::from([(first.id, first.name.clone())]),
    };
    assert_eq!(
        starting_set_summary(&reach, &[(&first).into(), (&second).into()]),
        format!(
            "Trusting 2 Servers from the start: machine-1 ({}, {}, key {}), {} ({}, {}, key {}).",
            first.subnet,
            first.id,
            first.public_key.fingerprint(),
            second.id,
            second.subnet,
            second.id,
            second.public_key.fingerprint()
        )
    );
    assert_eq!(
        starting_set_summary(&reach, &[]),
        "Trusting 0 Servers from the start: none."
    );
}

fn passkey_key(seed: u8) -> PasskeyPublicKey {
    let key = iroh::SecretKey::from_bytes(&[seed; 32]).public();
    PasskeyPublicKey::parse(PasskeyAlgorithm::Eddsa, key.as_bytes()).unwrap()
}

fn waiting_passkey(public_key: PasskeyPublicKey) -> serde_json::Value {
    json!({
        "credential_id": "AQID",
        "public_key": public_key,
        "account": "ada@example.com",
        "relying_party": { "id": "evil.example", "origin": "https://evil.example" },
    })
}

#[tokio::test]
async fn a_waiting_passkey_answers_for_this_clis_cloud_not_the_one_cloud_names() {
    let cloud = crate::cloud_login::tests::fake_cloud(|route, _| {
        assert_eq!(
            route,
            format!("GET /api/cli/passkey-requests/{}", passkey_key(1).code())
        );
        (200, waiting_passkey(passkey_key(1)))
    });
    let credential = token(cloud.clone());

    let passkey = pending_passkey(&credential, passkey_key(1).code())
        .await
        .unwrap();

    let cloud = url::Url::parse(&cloud).unwrap();
    assert_eq!(passkey.public_key, passkey_key(1));
    assert_eq!(passkey.account.as_str(), "ada@example.com");
    assert_eq!(passkey.relying_party, RelyingParty::of(&cloud).unwrap());
}

#[tokio::test]
async fn a_passkey_whose_key_does_not_hash_to_the_code_is_refused() {
    let cloud =
        crate::cloud_login::tests::fake_cloud(|_, _| (200, waiting_passkey(passkey_key(2))));

    let error = pending_passkey(&token(cloud), passkey_key(1).code())
        .await
        .unwrap_err();

    assert!(error.report().message.contains("doesn't match"));
}

fn waiting(public_key: PasskeyPublicKey) -> Passkey {
    Passkey {
        credential_id: serde_json::from_value(json!("AQID")).unwrap(),
        public_key,
        account: PasskeyAccount::parse("ada@example.com").unwrap(),
        relying_party: RelyingParty::of(&"https://ployz.cloud".parse().unwrap()).unwrap(),
    }
}

#[tokio::test]
async fn rm_names_each_passkey_the_removed_device_approved_which_stays_trusted() {
    let (mut client, server) = cluster([
        Daemon::ApprovedPasskey,
        Daemon::PredatesPasskeys,
        Daemon::Approves,
    ])
    .await;
    let code = key(1).code();

    let removed = set_everywhere(&mut client, |client, machine_id| {
        remove_on(client, machine_id, code)
    })
    .await
    .unwrap();

    let outcome = |hex| {
        removed
            .answers
            .iter()
            .find(|answer| answer.machine_id == id(hex))
            .map(|answer| {
                (
                    answer.outcome.change,
                    answer.outcome.approved_passkeys.clone(),
                )
            })
    };
    let device_removed = Change::Removed(TrustedKeyRemoved::Removed {
        code,
        kind: TrustedKeyKind::Device,
    });
    assert_eq!(
        outcome('1'),
        Some((device_removed, vec![passkey_key(1).code()]))
    );
    assert_eq!(
        outcome('2'),
        Some((
            Change::Removed(TrustedKeyRemoved::NotPresent { code }),
            Vec::new()
        ))
    );
    assert_eq!(outcome('3'), Some((device_removed, Vec::new())));
    let json = serde_json::to_value(removed.answers.first().unwrap()).unwrap();
    assert_eq!(
        json.get("approved_passkeys"),
        Some(&json!([passkey_key(1).code()]))
    );
    server.abort();
}

#[tokio::test]
async fn rm_on_a_passkey_code_removes_the_passkey() {
    let (mut client, server) = cluster([
        Daemon::ApprovedPasskey,
        Daemon::PredatesPasskeys,
        Daemon::PredatesPasskeys,
    ])
    .await;
    let code = passkey_key(1).code();

    let removed = set_everywhere(&mut client, |client, machine_id| {
        remove_on(client, machine_id, code)
    })
    .await
    .unwrap();

    let outcomes: Vec<_> = removed
        .answers
        .iter()
        .map(|answer| (answer.machine_id, answer.outcome.change))
        .collect();
    let absent = Change::Removed(TrustedKeyRemoved::NotPresent { code });
    assert_eq!(
        outcomes,
        [
            (
                id('1'),
                Change::Removed(TrustedKeyRemoved::Removed {
                    code,
                    kind: TrustedKeyKind::Passkey,
                })
            ),
            (id('2'), absent),
            (id('3'), absent),
        ]
    );
    let json = serde_json::to_value(removed.answers.first().unwrap()).unwrap();
    assert_eq!(json.get("outcome"), Some(&json!("removed")));
    assert_eq!(json.get("kind"), Some(&json!("passkey")));
    server.abort();
}

/// A Server whose Inspect still shows `listed` under the code while it trusts `held`
/// there, as when an approval lands between a read and `device rm`. Only
/// RemoveTrustedKey may change what it trusts.
async fn raced(
    listed: TrustedKeyKind,
    held: TrustedKeyKind,
) -> (
    Client,
    std::sync::Arc<std::sync::Mutex<Option<TrustedKeyKind>>>,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let trusted = std::sync::Arc::new(std::sync::Mutex::new(Some(held)));
    let state = trusted.clone();
    let (client, server) = crate::connect::test_support::rpc_client(move |rpc: Request<_>| {
        let trusted = state.clone();
        async move {
            #[expect(
                clippy::wildcard_enum_match_arm,
                reason = "any other call is the bug this fixture catches"
            )]
            let response = match rpc.into_inner().decode_request().unwrap().body {
                RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                    enrollment: None,
                    machines: vec![MachineObservation::new(
                        machine('1', "machine-1"),
                        MembershipObservation::Up,
                    )],
                }),
                RpcRequestBody::DescribeContract(_) => RpcResponse::from(ContractDescription {
                    machine_id: id('1'),
                    protocol_major: PROTOCOL_MAJOR,
                    daemon_version: "fixture".into(),
                    capabilities: [
                        SET_DEVICE_CAPABILITY,
                        SET_PASSKEY_CAPABILITY,
                        REMOVE_TRUSTED_KEY_CAPABILITY,
                    ]
                    .map(|capability| CapabilityName::parse(capability).unwrap())
                    .into(),
                    sealing: None,
                }),
                RpcRequestBody::Inspect(_) => {
                    let listed_machine = machine('1', "fixture");
                    RpcResponse::from(ployz_core::MachineDetails {
                        id: id('1'),
                        public_key: listed_machine.public_key,
                        machine: Some(listed_machine),
                        phase: ployz_core::LocalMachinePhase::Participating,
                        advertised_endpoints: Vec::new(),
                        store_version: Default::default(),
                        rtts: Vec::new(),
                        management_clients: Vec::new(),
                        passkeys: if listed == TrustedKeyKind::Passkey {
                            vec![ployz_core::authority::TrustedPasskey {
                                passkey: waiting(passkey_key(1)),
                                approved_by: PasskeyApprover::Server,
                            }]
                        } else {
                            Vec::new()
                        },
                        storage: None,
                        upgrade: None,
                        telemetry: None,
                        token: None,
                        sealing: None,
                        founding: Default::default(),
                    })
                }
                RpcRequestBody::RemoveTrustedKey(RemoveTrustedKeyRequest { code }) => {
                    RpcResponse::from(match trusted.lock().unwrap().take() {
                        Some(kind) => TrustedKeyRemoved::Removed { code, kind },
                        None => TrustedKeyRemoved::NotPresent { code },
                    })
                }
                other => panic!("device rm sent {other:?}"),
            };
            Ok(Response::new(response.encode().unwrap()))
        }
    })
    .await;
    (client, trusted, server)
}

#[tokio::test]
async fn rm_removes_a_passkey_approved_after_the_server_last_listed_a_device() {
    let (mut client, trusted, server) =
        raced(TrustedKeyKind::Device, TrustedKeyKind::Passkey).await;
    let code = passkey_key(1).code();

    let removed = set_everywhere(&mut client, |client, machine_id| {
        remove_on(client, machine_id, code)
    })
    .await
    .unwrap();

    assert_eq!(*trusted.lock().unwrap(), None);
    assert_eq!(
        removed.answers.first().map(|answer| answer.outcome.change),
        Some(Change::Removed(TrustedKeyRemoved::Removed {
            code,
            kind: TrustedKeyKind::Passkey,
        }))
    );
    server.abort();
}

#[tokio::test]
async fn rm_removes_a_device_that_replaced_the_passkey_the_server_last_listed() {
    let (mut client, trusted, server) =
        raced(TrustedKeyKind::Passkey, TrustedKeyKind::Device).await;
    let code = passkey_key(1).code();

    let removed = set_everywhere(&mut client, |client, machine_id| {
        remove_on(client, machine_id, code)
    })
    .await
    .unwrap();

    assert_eq!(*trusted.lock().unwrap(), None);
    assert_eq!(
        removed.answers.first().map(|answer| answer.outcome.change),
        Some(Change::Removed(TrustedKeyRemoved::Removed {
            code,
            kind: TrustedKeyKind::Device,
        }))
    );
    server.abort();
}

#[test]
fn ls_lists_passkeys_under_the_devices_with_their_account_and_approvers() {
    let account = PasskeyAccount::parse("ada@example.com").unwrap();
    let passkey = |by| Listed {
        code: passkey_key(1).code(),
        key: KeyKind::Passkey {
            account: account.clone(),
        },
        approved_by: Some(by),
    };
    let device = || Listed {
        code: key(0).code(),
        key: KeyKind::Device,
        approved_by: None,
    };
    let roster = roster(&[
        (id('1'), vec![passkey(PasskeyApprover::Server), device()]),
        (id('2'), vec![device()]),
        (
            id('3'),
            vec![passkey(PasskeyApprover::Device {
                code: key(0).code(),
            })],
        ),
    ]);

    let rows: Vec<_> = roster
        .iter()
        .map(|row| (row.code, row.key.clone(), row.missing.clone()))
        .collect();
    assert_eq!(
        rows,
        [
            (key(0).code(), KeyKind::Device, vec![id('3')]),
            (
                passkey_key(1).code(),
                KeyKind::Passkey { account },
                vec![id('2')]
            ),
        ]
    );
    let reach = Reach {
        asked: Vec::new(),
        silent: Vec::new(),
        entry: None,
        names: [(id('1'), MachineName::parse("machine-1").unwrap())].into(),
    };
    let [device, passkey] = roster.as_slice() else {
        panic!("one device and one passkey");
    };
    assert_eq!(device.approvers(&reach), "");
    assert_eq!(
        passkey.approvers(&reach),
        format!("on machine-1, device {}", key(0).code())
    );
    let json = serde_json::to_value(passkey).unwrap();
    assert_eq!(json.get("kind"), Some(&json!("passkey")));
    assert_eq!(json.get("account"), Some(&json!("ada@example.com")));
    assert_eq!(
        json.get("approved_by"),
        Some(&json!([
            { "machine_id": id('1'), "by": "server" },
            { "machine_id": id('3'), "by": "device", "code": key(0).code() },
        ]))
    );
}
