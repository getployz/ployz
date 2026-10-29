use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};

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
            .map(|value| MachineId::parse(value.to_str().unwrap()).unwrap());
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
                capabilities: (daemon != Some(Daemon::PredatesDevices))
                    .then(|| CapabilityName::parse(SET_DEVICE_CAPABILITY).unwrap())
                    .into_iter()
                    .collect(),
            }),
            RpcRequestBody::SetDevice(_) if daemon == Some(Daemon::Unreachable) => {
                return Err(Status::unavailable("machine-2 is down"));
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

fn approve() -> SetDeviceRequest {
    SetDeviceRequest::Approve { key: key(0) }
}

fn token(cloud: String) -> Credential {
    Credential::Token {
        cloud,
        token: Secret::new("ployz_secret".to_owned()),
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

    let settled = set_everywhere(&mut client, approve()).await.unwrap();

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

    let settled = set_everywhere(&mut client, approve()).await.unwrap();

    assert!(settled.answers.is_empty());
    assert_eq!(settled.gaps.failures.len(), 3);
    assert!(settled.table().is_none());
    assert_eq!(settled.gaps.outcome().unwrap_err().printed_exit(), Some(3));
    server.abort();
}

#[tokio::test]
async fn rerunning_where_every_machine_already_approved_succeeds() {
    let (mut client, server) = cluster([Daemon::AlreadyApproved; 3]).await;

    let settled = set_everywhere(&mut client, approve()).await.unwrap();

    assert!(
        settled
            .answers
            .iter()
            .all(|answer| matches!(answer.set, DeviceSet::AlreadyApproved { .. }))
    );
    assert_eq!(settled.answers.len(), 3);
    settled.gaps.outcome().unwrap();
    server.abort();
}

#[tokio::test]
async fn a_machine_without_a_device_roster_is_an_omission() {
    let (mut client, server) =
        cluster([Daemon::Approves, Daemon::PredatesDevices, Daemon::Approves]).await;

    let settled = set_everywhere(&mut client, approve()).await.unwrap();

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
        (vec!["approve", &code], super::approve),
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
}
