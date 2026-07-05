//! Machine-add credential minting against a secured (TLS +
//! NKey-authorized) NATS server.
//!
//! Minting runs as bounded operation work: these tests drive the full
//! mint → render → reload → verify → material-ready sequence against the
//! fixture's real `nats-server`, including the ADR-0015 single-writer
//! fence and the ADR-0001 authority-file durability rules.

use ployz_core::machine::MachineAddFailure;
use ployz_core::nats_config::{NatsUserPublicKey, parse_authorized_users, render_authorized_users};
use ployz_core::ops::MachineAddOperationState;
use ployz_core::ops::OperationStatus;
use ployz_core::roles::InstallRolePolicy;
use ployz_core::security::NatsPrincipal;
use ployz_core::subjects::{OPS_STREAM_SUBJECT, OperationApiEndpoint};
use ployz_nats::operation_api_client::{OperationApiClient, OperationApiClientError};
use ployz_sdk_types::{
    MachineAddAccepted, MachineAddError, MachineAddRequest, MachineJoinRedeemError,
    MachineJoinRedeemRequest,
};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{Mutex, OwnedMutexGuard};

mod support;

use ployz_test_support::ids::{idempotency_key, machine_id, operation_id};
use ployz_test_support::ops::wait_for_terminal_status;
use support::control::{RecordingReload, TestNats, redeem_when_ready};

static MACHINE_ADD_MINT_TEST_LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();

async fn lock_machine_add_mint_test() -> OwnedMutexGuard<()> {
    MACHINE_ADD_MINT_TEST_LOCK
        .get_or_init(|| Arc::new(Mutex::new(())))
        .clone()
        .lock_owned()
        .await
}

/// The machine-add handler returns its operation id + join material before
/// any reload occurs; minting runs as owned operation work afterwards and
/// produces a usable per-machine NKey seed.
#[tokio::test]
async fn machine_add_accepts_before_reload_then_mints_material() {
    let _guard = lock_machine_add_mint_test().await;
    let nats = TestNats::start().await;
    let reload = RecordingReload::gated_signal(nats.server().server_pid());
    let config = nats.control_config();
    let runtime = nats
        .start_control_with_reload(&config, reload.clone())
        .await;
    let api = nats.api();

    let accepted = machine_add(&api, "op_machine", "idem_machine", "machine_2").await;

    // The handler returned while the (gated) reload had not run: accepting
    // is fast and never includes render/reload/verify work.
    assert_eq!(reload.outcomes().len(), 0);
    reload.release();

    let redeemed = redeem_when_ready(&api, &accepted.join_token).await;
    assert!(
        redeemed
            .secret_delivery
            .nats_credentials
            .secret()
            .starts_with("SU"),
        "minted material is an NKey user seed"
    );
    assert!(!reload.outcomes().is_empty(), "reload runner was invoked");

    let retry_error = api
        .machine_add(&MachineAddRequest {
            operation_id: operation_id("op_machine_retry"),
            idempotency_key: idempotency_key("idem_machine"),
            machine_id: machine_id("machine_2"),
            name: ployz_sdk_types::MachineName::try_new("machine_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
        })
        .await
        .expect_err("duplicate machine add idempotency key is rejected");
    assert_eq!(
        retry_error,
        OperationApiClientError::Domain {
            endpoint: OperationApiEndpoint::MachineAdd,
            error: MachineAddError::DuplicateIdempotencyKey {
                operation_id: operation_id("op_machine_retry"),
            },
        }
    );
    let replayed = redeem_when_ready(&api, &accepted.join_token).await;
    assert_eq!(
        replayed.secret_delivery.nats_credentials.secret(),
        redeemed.secret_delivery.nats_credentials.secret(),
        "replay returns the original minted material"
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

/// Two machine-adds mint distinct credentials.
#[tokio::test]
async fn machine_add_mints_unique_credentials_per_machine() {
    let _guard = lock_machine_add_mint_test().await;
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();

    let first = machine_add(&api, "op_machine_a", "idem_machine_a", "machine_a").await;
    let second = machine_add(&api, "op_machine_b", "idem_machine_b", "machine_b").await;
    let first_redeemed = redeem_when_ready(&api, &first.join_token).await;
    let second_redeemed = redeem_when_ready(&api, &second.join_token).await;

    assert_ne!(
        first_redeemed.secret_delivery.nats_credentials.secret(),
        second_redeemed.secret_delivery.nats_credentials.secret(),
        "each machine gets its own minted seed"
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

/// ADR-0015 fence: two concurrent machine-adds queue their renders through
/// the single writer; both complete and both public keys are present in
/// the rendered `authorized-users.conf`.
#[tokio::test]
async fn concurrent_machine_adds_both_render_their_keys() {
    let _guard = lock_machine_add_mint_test().await;
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();

    let (left, right) = tokio::join!(
        machine_add(&api, "op_fence_a", "idem_fence_a", "machine_fa"),
        machine_add(&api, "op_fence_b", "idem_fence_b", "machine_fb"),
    );
    let left_redeemed = redeem_when_ready(&api, &left.join_token).await;
    let right_redeemed = redeem_when_ready(&api, &right.join_token).await;

    let rendered = std::fs::read_to_string(nats.server().authorized_users_path())
        .expect("authorized-users file is readable");
    let users = parse_authorized_users(&rendered).expect("rendered authority file parses");
    let left_key = public_key_of(left_redeemed.secret_delivery.nats_credentials.secret());
    let right_key = public_key_of(right_redeemed.secret_delivery.nats_credentials.secret());
    assert!(
        rendered_principal_key(&users, "machine_fa") == Some(left_key),
        "machine_fa's minted public key is rendered"
    );
    assert!(
        rendered_principal_key(&users, "machine_fb") == Some(right_key),
        "machine_fb's minted public key is rendered"
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

/// File authority durability: with a pre-existing file containing an
/// unknown user, startup and a subsequent render preserve the entry.
#[tokio::test]
async fn startup_preserves_existing_authorized_users_when_rendering() {
    let _guard = lock_machine_add_mint_test().await;
    let nats = TestNats::start().await;
    // Plant an unknown principal in the authority file before control runs.
    let ghost = nkeys::KeyPair::new_user();
    let ghost_public =
        NatsUserPublicKey::try_new(ghost.public_key()).expect("generated public key is valid");
    let cloud = nkeys::KeyPair::new_user();
    let cloud_public =
        NatsUserPublicKey::try_new(cloud.public_key()).expect("generated public key is valid");
    let existing = std::fs::read_to_string(nats.server().authorized_users_path())
        .expect("fixture authority file is readable");
    let mut users = parse_authorized_users(&existing).expect("fixture authority file parses");
    users.push(ployz_core::nats_config::NatsAuthorizedUser {
        principal: NatsPrincipal::Machine {
            machine_id: machine_id("machine_ghost"),
        },
        nkey_public: ghost_public.clone(),
    });
    users.push(ployz_core::nats_config::NatsAuthorizedUser {
        principal: NatsPrincipal::User,
        nkey_public: cloud_public.clone(),
    });
    std::fs::write(
        nats.server().authorized_users_path(),
        render_authorized_users(&users),
    )
    .expect("authority file with unknown user writes");

    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();

    // Trigger a render through a real machine-add.
    let accepted = machine_add(&api, "op_preserve", "idem_preserve", "machine_new").await;
    redeem_when_ready(&api, &accepted.join_token).await;

    let rendered = std::fs::read_to_string(nats.server().authorized_users_path())
        .expect("authorized-users file is readable");
    let users = parse_authorized_users(&rendered).expect("rendered authority file parses");
    assert_eq!(
        rendered_principal_key(&users, "machine_ghost"),
        Some(ghost_public),
        "unknown user survives the render"
    );
    assert!(
        rendered_principal_key(&users, "machine_new").is_some(),
        "minted user is rendered alongside the existing one"
    );
    assert!(
        users.iter().any(|user| {
            user.principal == NatsPrincipal::User && user.nkey_public == cloud_public
        }),
        "extra User credential survives the render"
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

/// Startup render repairs authorization files whose principal set is still
/// current but whose permission profile was rendered by an older binary.
#[tokio::test]
async fn startup_renders_current_authorization_permissions() {
    let _guard = lock_machine_add_mint_test().await;
    let nats = TestNats::start().await;
    let progress_scope = OPS_STREAM_SUBJECT;
    let existing = std::fs::read_to_string(nats.server().authorized_users_path())
        .expect("fixture authority file is readable");
    assert!(
        existing.contains(progress_scope),
        "fixture starts with current controller permissions"
    );
    let stale = existing.replace(progress_scope, "plz.v1.oldprogress");
    assert!(
        !stale.contains(progress_scope),
        "stale fixture removes the operation progress publish scope"
    );
    std::fs::write(nats.server().authorized_users_path(), stale)
        .expect("stale authority file writes");

    let reload = nats.reload_runner();
    let config = nats.control_config();
    let runtime = nats
        .start_control_with_reload(&config, reload.clone())
        .await;

    let repaired = std::fs::read_to_string(nats.server().authorized_users_path())
        .expect("repaired authorized-users file is readable");
    assert!(
        repaired.contains(progress_scope),
        "startup render restores the current controller permission profile"
    );
    assert_eq!(
        reload.outcomes().len(),
        1,
        "startup render reloads NATS after repairing stale permissions"
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

/// Reload failure is a typed terminal failure with command evidence —
/// not a retry loop.
#[tokio::test]
async fn machine_add_reload_failure_is_a_typed_terminal_failure() {
    let _guard = lock_machine_add_mint_test().await;
    let nats = TestNats::start().await;
    let reload = RecordingReload::failing();
    let config = nats.control_config();
    let runtime = nats
        .start_control_with_reload(&config, reload.clone())
        .await;
    let api = nats.api();

    machine_add(&api, "op_reload_fail", "idem_reload_fail", "machine_rf").await;
    let status = wait_for_terminal_status(
        &api,
        &operation_id("op_reload_fail"),
        Duration::from_secs(10),
    )
    .await;

    let OperationStatus::MachineAdd {
        state: MachineAddOperationState::Failed { failure },
        ..
    } = status
    else {
        panic!("expected failed machine add, got {status:?}");
    };
    let MachineAddFailure::NatsReloadFailed { message } = failure else {
        panic!("expected typed reload failure, got {failure:?}");
    };
    assert!(
        message.as_str().contains("reload refused by test"),
        "failure carries the reload command evidence: {message:?}"
    );
    assert!(!reload.outcomes().is_empty(), "reload runner was invoked");

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

/// A control crash between machine-add acceptance and material-ready must
/// not strand the operation: the next control start runs one bounded
/// reconciliation pass that resumes the mint, and the per-key mint claim
/// makes the resumed run converge on the partially minted material.
#[tokio::test]
async fn control_restart_resumes_stranded_mint_to_material_ready() {
    let _guard = lock_machine_add_mint_test().await;
    let nats = TestNats::start().await;
    // Gate the reload so the mint cannot reach material-ready while the
    // first control process is alive.
    let reload = RecordingReload::gated_signal(nats.server().server_pid());
    let config = nats.control_config();
    let runtime = nats
        .start_control_with_reload(&config, reload.clone())
        .await;
    let api = nats.api();

    let accepted = machine_add(&api, "op_stranded", "idem_stranded", "machine_st").await;
    let not_ready = api
        .machine_join_redeem(&MachineJoinRedeemRequest {
            join_token: accepted.join_token.clone(),
        })
        .await
        .expect_err("redeem before material-ready is refused");
    assert!(
        matches!(
            not_ready,
            OperationApiClientError::Domain {
                error: MachineJoinRedeemError::MaterialNotReady { .. },
                ..
            }
        ),
        "mint had not reached material-ready before the crash: {not_ready:?}"
    );

    // The control process "crashes": the mint worker dies with it, leaving
    // the accepted operation without material.
    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
    reload.release();

    // A fresh control start reconciles: the stranded mint resumes and
    // reaches material-ready without a new machine-add request.
    let runtime = nats.start_control(&config).await;
    let redeemed = redeem_when_ready(&api, &accepted.join_token).await;
    assert!(
        redeemed
            .secret_delivery
            .nats_credentials
            .secret()
            .starts_with("SU"),
        "resumed mint produced an NKey user seed"
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

/// Redeem before the mint worker reaches material-ready is the typed
/// not-ready response, and the keeper-style bounded retry succeeds later.
#[tokio::test]
async fn machine_join_redeem_waits_for_material_ready() {
    let _guard = lock_machine_add_mint_test().await;
    let nats = TestNats::start().await;
    let reload = RecordingReload::gated_signal(nats.server().server_pid());
    let config = nats.control_config();
    let runtime = nats
        .start_control_with_reload(&config, reload.clone())
        .await;
    let api = nats.api();

    let accepted = machine_add(&api, "op_wait", "idem_wait", "machine_w").await;
    let not_ready = api
        .machine_join_redeem(&MachineJoinRedeemRequest {
            join_token: accepted.join_token.clone(),
        })
        .await
        .expect_err("redeem before material-ready is refused");
    assert!(
        matches!(
            not_ready,
            OperationApiClientError::Domain {
                error: MachineJoinRedeemError::MaterialNotReady { ref operation_id },
                ..
            } if operation_id == &accepted.accepted.operation_id
        ),
        "expected typed not-ready, got {not_ready:?}"
    );

    reload.release();
    redeem_when_ready(&api, &accepted.join_token).await;

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

async fn machine_add(
    api: &OperationApiClient,
    operation: &str,
    idempotency: &str,
    machine: &str,
) -> MachineAddAccepted {
    api.machine_add(&MachineAddRequest {
        operation_id: operation_id(operation),
        idempotency_key: idempotency_key(idempotency),
        machine_id: machine_id(machine),
        name: ployz_sdk_types::MachineName::try_new(machine).expect("valid machine name"),
        roles: InstallRolePolicy::install_all().without_gateway(),
    })
    .await
    .expect("machine add accepts")
}

fn public_key_of(seed: &str) -> NatsUserPublicKey {
    let pair = nkeys::KeyPair::from_seed(seed).expect("minted seed parses");
    NatsUserPublicKey::try_new(pair.public_key()).expect("derived public key is valid")
}

fn rendered_principal_key(
    users: &[ployz_core::nats_config::NatsAuthorizedUser],
    machine: &str,
) -> Option<NatsUserPublicKey> {
    users
        .iter()
        .find(|user| {
            user.principal
                == NatsPrincipal::Machine {
                    machine_id: machine_id(machine),
                }
        })
        .map(|user| user.nkey_public.clone())
}
