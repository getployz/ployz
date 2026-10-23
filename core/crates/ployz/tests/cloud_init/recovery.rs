//! Founding under Cloud's recovery flow: the first backup, its code, and the seed.

use super::*;
use harness::{CallbackReply, fake_wrapped};
use ployz_core::{RecoveryCode, RecoverySeed, SealingKeyBackup, SealingPublicKey};

const CODE_FIELD: &str = "Recovery code";
const ROTATE_HINT: &str = "ployz recovery rotate";

fn initialize(recovery: Option<serde_json::Value>) -> serde_json::Value {
    let mut body = json!({
        "kind": "initialize",
        "resumed": false,
        "storage": "none",
        "pairing": { "secret": PAIRING },
    });
    if let Some(recovery) = recovery {
        body.as_object_mut()
            .unwrap()
            .insert("recovery".to_owned(), recovery);
    }
    body
}

fn founder_daemon() -> JoinDaemon {
    JoinDaemon::new(Registered {
        assigned_machine: founder_machine(),
        visible_peers: Vec::new(),
        target_versions: Default::default(),
        sealing_key: None,
    })
}

fn backup_digest() -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(fake_wrapped().backup.as_bytes()))
}

fn matching_echo() -> CallbackReply {
    CallbackReply::Body(json!({
        "machineId": founder_machine().id.as_str(),
        "recoveryBackupDigest": backup_digest(),
    }))
}

fn callback_with_backup() -> serde_json::Value {
    let wrapped = fake_wrapped();
    json!({
        "machineId": founder_machine().id.as_str(),
        "pairingCredential": PAIRING,
        "recoveryBackup": {
            "publicKey": wrapped.public_key.to_string(),
            "backup": wrapped.backup,
        },
    })
}

async fn found(
    machine_addr: &std::net::SocketAddr,
    enroll_url: &str,
    json: bool,
    show_code: bool,
) -> std::process::Output {
    let mut command = harness::cli();
    command.env("PLOYZ_ENROLL_TOKEN", TOKEN);
    if json {
        command.arg("--json");
    }
    command.args([
        "--connect",
        &format!("ssh://root@{machine_addr}"),
        "server",
        "add",
        "--cloud-url",
        enroll_url,
        "--name",
        "founder",
        "--accepts-ingress=false",
        "--yes",
    ]);
    if show_code {
        command.arg("--show-recovery-code");
    }
    command.output().await.unwrap()
}

fn code_lines(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .filter(|line| line.contains(CODE_FIELD))
        .collect()
}

fn code_after_field(line: &str) -> RecoveryCode {
    let raw = line
        .trim_start_matches(CODE_FIELD)
        .trim_start_matches(|character: char| character == '=' || character.is_whitespace());
    RecoveryCode::parse(raw).expect("the printed recovery code parses")
}

fn plain_callback() -> serde_json::Value {
    json!({ "machineId": founder_machine().id.as_str(), "pairingCredential": PAIRING })
}

#[tokio::test]
async fn recovery_support_follows_the_daemon_contract() {
    for (recovery_rpcs, expected) in [(true, Some(json!(1))), (false, None)] {
        let enroll = EnrollListen::script([initialize(None)]).await;
        let mut daemon = founder_daemon();
        if recovery_rpcs {
            daemon = daemon.with_recovery_rpcs();
        }
        let machine_addr = serve_machine(daemon.clone()).await;

        let output = found(&machine_addr, &enroll.url, false, false).await;

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{recovery_rpcs}: {stdout}");
        assert_eq!(
            enroll.posts().first().unwrap().get("recoverySupport"),
            expected.as_ref(),
            "{recovery_rpcs}"
        );
        assert!(stdout.contains("Initialised Server founder;"), "{stdout}");
        assert!(code_lines(&stdout).is_empty(), "{stdout}");
        assert_eq!(enroll.callbacks(), [plain_callback()]);
        assert_eq!(daemon.wrap_calls(), 0);
        assert_eq!(daemon.recovery_seeds(), []);
    }
}

#[tokio::test]
async fn seed_enrolls_for_recovery_and_prints_no_code() {
    let expected_public_key = SealingPublicKey::from_bytes([9; 32]);
    let backup = SealingKeyBackup::from_bytes(b"seed backup".to_vec());
    for backup in [None, Some(backup)] {
        let enroll = EnrollListen::script([initialize(Some(json!({
            "kind": "seed",
            "expectedPublicKey": expected_public_key.to_string(),
            "backup": backup,
        })))])
        .await;
        let daemon = founder_daemon().with_recovery_rpcs();
        let machine_addr = serve_machine(daemon.clone()).await;

        let output = found(&machine_addr, &enroll.url, false, false).await;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert_eq!(
            daemon.recovery_seeds(),
            [RecoverySeed {
                expected_public_key,
                backup: backup.clone(),
            }]
        );
        assert_eq!(daemon.initialize_request().name.as_str(), "founder");
        assert_eq!(daemon.wrap_calls(), 0);
        assert!(code_lines(&stdout).is_empty(), "{stdout}");
        assert!(stderr.contains("ployz recover "), "{stderr}");
        assert_eq!(enroll.callbacks(), [plain_callback()]);
    }
}

#[tokio::test]
async fn back_up_prints_one_code_after_cloud_echoes_the_digest() {
    let enroll = EnrollListen::script([initialize(Some(json!({ "kind": "back_up" })))]).await;
    enroll.set_callback_reply(matching_echo());
    let daemon = founder_daemon().with_recovery_rpcs();
    let machine_addr = serve_machine(daemon.clone()).await;

    let output = found(&machine_addr, &enroll.url, false, false).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    let [line] = code_lines(&stdout)[..] else {
        panic!("one recovery code line: {stdout}");
    };
    let code = code_after_field(line);
    assert!(!stderr.contains(code.reveal().as_str()), "{stderr}");
    assert!(stderr.contains("ployz recover "), "{stderr}");
    assert_eq!(daemon.wrap_calls(), 1);
    assert_eq!(enroll.callbacks(), [callback_with_backup()]);
}

#[tokio::test]
async fn back_up_without_a_matching_echo_exits_without_a_code() {
    let wrong_digest = json!({
        "machineId": founder_machine().id.as_str(),
        "recoveryBackupDigest": hex::encode([0; 32]),
    });
    for reply in [CallbackReply::Echo, CallbackReply::Body(wrong_digest)] {
        let enroll = EnrollListen::script([initialize(Some(json!({ "kind": "back_up" })))]).await;
        enroll.set_callback_reply(reply);
        let daemon = founder_daemon().with_recovery_rpcs();
        let machine_addr = serve_machine(daemon.clone()).await;

        let output = found(&machine_addr, &enroll.url, false, false).await;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{stdout}\n{stderr}");
        assert!(code_lines(&stdout).is_empty(), "{stdout}");
        assert!(stderr.contains("Recovery code not saved"), "{stderr}");
        assert!(stderr.contains(ROTATE_HINT), "{stderr}");
        assert_eq!(daemon.wrap_calls(), 1);
        assert_eq!(enroll.callbacks().len(), 1);
    }
}

#[tokio::test]
async fn back_up_with_a_dropped_echo_retries_then_exits_without_a_code() {
    back_up_without_an_echo(CallbackReply::Drop).await;
}

#[tokio::test]
async fn back_up_with_a_hanging_echo_retries_then_exits_without_a_code() {
    back_up_without_an_echo(CallbackReply::Hang).await;
}

async fn back_up_without_an_echo(reply: CallbackReply) {
    let enroll = EnrollListen::script([initialize(Some(json!({ "kind": "back_up" })))]).await;
    enroll.set_callback_reply(reply);
    let daemon = founder_daemon().with_recovery_rpcs();
    let machine_addr = serve_machine(daemon.clone()).await;

    let output = found(&machine_addr, &enroll.url, false, false).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stdout}\n{stderr}");
    assert!(code_lines(&stdout).is_empty(), "{stdout}");
    assert!(stderr.contains("Recovery code not saved"), "{stderr}");
    assert!(stderr.contains(ROTATE_HINT), "{stderr}");
    let callbacks = enroll.callbacks();
    assert!(callbacks.len() > 1, "the callback retries: {callbacks:?}");
    assert!(
        callbacks
            .iter()
            .all(|callback| *callback == callback_with_backup())
    );
    assert_eq!(daemon.wrap_calls(), 1);
}

#[tokio::test]
async fn plain_json_founding_leaves_the_backup_for_rotate() {
    let enroll = EnrollListen::script([initialize(Some(json!({ "kind": "back_up" })))]).await;
    let daemon = founder_daemon().with_recovery_rpcs();
    let machine_addr = serve_machine(daemon.clone()).await;

    let output = found(&machine_addr, &enroll.url, true, false).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    let result: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(result.get("recoveryCode"), None, "{stdout}");
    assert!(stderr.contains(ROTATE_HINT), "{stderr}");
    assert_eq!(daemon.wrap_calls(), 0);
    assert_eq!(enroll.callbacks(), [plain_callback()]);
}

#[tokio::test]
async fn json_show_recovery_code_carries_the_code() {
    let enroll = EnrollListen::script([initialize(Some(json!({ "kind": "back_up" })))]).await;
    enroll.set_callback_reply(matching_echo());
    let daemon = founder_daemon().with_recovery_rpcs();
    let machine_addr = serve_machine(daemon.clone()).await;

    let output = found(&machine_addr, &enroll.url, true, true).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    let result: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let code = result
        .get("recoveryCode")
        .and_then(serde_json::Value::as_str)
        .expect("recoveryCode is a string");
    RecoveryCode::parse(code).expect("the JSON recovery code parses");
    assert!(!stderr.contains(code), "{stderr}");
    assert_eq!(daemon.wrap_calls(), 1);
    assert_eq!(enroll.callbacks(), [callback_with_backup()]);
}
