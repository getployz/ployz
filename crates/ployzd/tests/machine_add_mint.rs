//! Machine-add credential minting against a secured (TLS +
//! NKey-authorized) NATS server.
//!
//! Minting runs as bounded operation work: these tests drive the full
//! mint → render → reload → verify → material-ready sequence against the
//! fixture's real `nats-server`, including the ADR-0015 single-writer
//! fence and the ADR-0001 authority-file durability rules.

use ployz_core::machine::{JoinTokenRedeemedAt, MachineAddFailure, RawJoinToken};
use ployz_core::nats_config::{
    CredentialGrant, CredentialName, CredentialRole, NatsAuthorizationGrant, NatsInternalAuthority,
    NatsUserPublicKey, parse_authorized_users, render_authorized_users,
};
use ployz_core::ops::MachineAddOperationState;
use ployz_core::ops::OperationStatus;
use ployz_core::roles::InstallRolePolicy;
use ployz_core::state::{ControlPlaneEpoch, IntentSnapshot, PendingMachineJoinRecoverySnapshot};
use ployz_core::subjects::{OPERATION_PROGRESS_SCOPE, OperationApiEndpoint};
use ployz_nats::operation_api_client::{OperationApiClient, OperationApiClientError};
use ployz_sdk_types::{
    MachineAddAccepted, MachineAddError, MachineAddRequest, MachineJoinRedeemError,
    MachineJoinRedeemRequest, MachineJoinRedeemResult, MachineJoinReportOutcome,
    MachineJoinReportRequest, MachineListRequest, OpsStatusError, OpsStatusRequest,
};
use ployzd::intent::service::NatsIntentReader;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{Mutex, OwnedMutexGuard};

#[path = "machine_add_mint/dataplane.rs"]
mod dataplane;
mod support;

use ployz_test_support::ids::{idempotency_key, machine_id, operation_id};
use ployz_test_support::ops::wait_for_terminal_status;
use support::control::{RecordingReload, TestNats, redeem_when_ready};

use ployzd::core_store::CoreStore;
use ployzd::operations::log::{OperationRepository, OperationStatusWrite};
use ployzd::roles::machine::intent_mirror::{MachineIntentMirror, MachinePendingJoinMirror};

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
    let join_api = nats.join_api();

    let accepted = machine_add(&api, "op_machine", "idem_machine", "machine_2").await;

    // The handler returned while the (gated) reload had not run: accepting
    // is fast and never includes render/reload/verify work.
    assert_eq!(reload.outcomes().len(), 0);
    reload.release();

    let redeemed = redeem_when_ready(&join_api, &accepted.join_token).await;
    assert!(
        redeemed
            .secret_delivery
            .nats_credentials
            .secret()
            .starts_with("SU"),
        "minted material is an NKey user seed"
    );
    assert!(!reload.outcomes().is_empty(), "reload runner was invoked");

    let retry_error = machine_add_result(
        &api,
        &MachineAddRequest {
            operation_id: operation_id("op_machine_retry"),
            idempotency_key: idempotency_key("idem_machine"),
            machine_id: machine_id("machine_2"),
            name: ployz_sdk_types::MachineName::try_new("machine_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
        },
    )
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
    let replayed = redeem_when_ready(&join_api, &accepted.join_token).await;
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
    let join_api = OperationApiClient::new(nats.connected.join.clone())
        .with_request_timeout(Duration::from_secs(5));

    let first = machine_add(&api, "op_machine_a", "idem_machine_a", "machine_a").await;
    let second = machine_add(&api, "op_machine_b", "idem_machine_b", "machine_b").await;
    let first_redeemed = redeem_when_ready(&join_api, &first.join_token).await;
    let second_redeemed = redeem_when_ready(&join_api, &second.join_token).await;

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

#[tokio::test]
async fn completed_machine_join_records_machine_derived_endpoint_subnet() {
    let _guard = lock_machine_add_mint_test().await;
    let nats = TestNats::start().await;
    let config = nats.control_config().with_dataplane_endpoint_supernet(
        ployz_core::dataplane::MachineEndpointSupernet::try_new("10.199.0.0/16")
            .expect("valid endpoint supernet"),
    );
    let runtime = nats.start_control(&config).await;
    let api = nats.api();
    let join_api = nats.join_api();

    let accepted = machine_add(&api, "op_endpoint", "idem_endpoint", "machine_endpoint").await;
    let redeemed = redeem_when_ready(&join_api, &accepted.join_token).await;
    dataplane::report_join_completed_with_retry(
        &nats,
        machine_id("machine_endpoint"),
        redeemed.secret_delivery.nats_credentials,
        accepted.join_token,
    )
    .await;

    let listed = api
        .machine_list(&MachineListRequest {})
        .await
        .expect("machine list succeeds");
    let [machine] = listed.machines.as_slice() else {
        panic!("expected one active machine");
    };
    // The roster records the subnet the machine daemon derives for itself,
    // so intent matches the machine's dataplane without a delivery channel.
    assert_eq!(
        machine.active.endpoint_subnet.as_string(),
        ployz_core::dataplane::default_endpoint_subnet(&machine.active.machine_id)
    );
    let expected_mesh_endpoint = "192.0.2.10:51820".parse().expect("mesh endpoint");
    assert_eq!(machine.active.mesh_endpoints, vec![expected_mesh_endpoint]);
    assert_eq!(
        machine.active.wireguard_public_key,
        dataplane::test_wireguard_public_key(&machine.active.machine_id)
    );

    let projection = NatsIntentReader::new(nats.connected.controller.clone())
        .intent()
        .await
        .expect("intent reads")
        .dataplane_projection;
    let [declared] = projection.declared_members() else {
        panic!("expected one declared dataplane member");
    };
    assert_eq!(declared.mesh_endpoints, vec![expected_mesh_endpoint]);
    assert_eq!(
        declared.wireguard_public_key,
        dataplane::test_wireguard_public_key(&declared.machine_id)
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
    let join_api = nats.join_api();

    let (left, right) = tokio::join!(
        machine_add(&api, "op_fence_a", "idem_fence_a", "machine_fa"),
        machine_add(&api, "op_fence_b", "idem_fence_b", "machine_fb"),
    );
    let left_redeemed = redeem_when_ready(&join_api, &left.join_token).await;
    let right_redeemed = redeem_when_ready(&join_api, &right.join_token).await;

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

#[tokio::test]
async fn concurrent_colliding_machine_adds_reserve_distinct_endpoint_subnets() {
    let _guard = lock_machine_add_mint_test().await;
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();
    let join_api = nats.join_api();

    let (first, colliding) = tokio::join!(
        machine_add(&api, "op_subnet_1", "idem_subnet_1", "machine_1"),
        machine_add(&api, "op_subnet_255", "idem_subnet_255", "machine_255"),
    );
    let first = redeem_when_ready(&join_api, &first.join_token).await;
    let colliding = redeem_when_ready(&join_api, &colliding.join_token).await;

    assert_ne!(first.endpoint_subnet, colliding.endpoint_subnet);
    assert!(
        [
            first.endpoint_subnet.as_string(),
            colliding.endpoint_subnet.as_string()
        ]
        .contains(&"10.198.0.0/24".to_owned())
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
    users.push(NatsAuthorizationGrant::Internal {
        authority: NatsInternalAuthority::Machine {
            machine_id: machine_id("machine_ghost"),
        },
        public_key: ghost_public.clone(),
    });
    users.push(NatsAuthorizationGrant::Credential(CredentialGrant {
        public_key: cloud_public.clone(),
        name: CredentialName::try_new("Ployz Cloud").expect("credential name"),
        role: CredentialRole::Operator,
    }));
    std::fs::write(
        nats.server().authorized_users_path(),
        render_authorized_users(&users),
    )
    .expect("authority file with unknown user writes");

    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();
    let join_api = nats.join_api();

    // Trigger a render through a real machine-add.
    let accepted = machine_add(&api, "op_preserve", "idem_preserve", "machine_new").await;
    redeem_when_ready(&join_api, &accepted.join_token).await;

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
        users.iter().any(|grant| {
            matches!(
                grant,
                NatsAuthorizationGrant::Credential(CredentialGrant { public_key, .. })
                    if public_key == &cloud_public
            )
        }),
        "extra Operator credential survives the render"
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
    let progress_scope = OPERATION_PROGRESS_SCOPE;
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
    let runtime = ployzd::roles::control::start_control_process_with_client_and_reload(
        nats.connected.controller.clone(),
        &config,
        reload.clone(),
    )
    .await
    .expect("control runtime starts");

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
    let join_api = nats.join_api();

    let accepted = machine_add(&api, "op_stranded", "idem_stranded", "machine_st").await;
    let not_ready = join_api
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
    let redeemed = redeem_when_ready(&join_api, &accepted.join_token).await;
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

#[tokio::test]
async fn promoted_core_recovers_pending_join_without_old_operation_log() {
    let _guard = lock_machine_add_mint_test().await;
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();

    machine_add(
        &api,
        "op_recover_join_1",
        "idem_recover_join_1",
        "machine_1",
    )
    .await;
    let accepted = machine_add_with_assurance(
        &api,
        "op_recover_join_255",
        "idem_recover_join_255",
        "machine_255",
        ployz_core::install::HostPortAssurance::External,
    )
    .await;
    runtime
        .shutdown()
        .await
        .expect("original control runtime shuts down");

    let source_store = CoreStore::open(config.core_db_path.clone())
        .await
        .expect("source store opens");
    let source_repository =
        OperationRepository::open(source_store, nats.connected.controller.clone());
    let pending = source_repository
        .pending_machine_adds_for_mirror()
        .await
        .expect("pending joins mirror");

    let mirror_dir = tempfile::tempdir().expect("mirror dir");
    let intent_mirror = mirror_dir.path().join("intent-mirror.json");
    MachineIntentMirror::new(intent_mirror.clone())
        .store(&IntentSnapshot {
            epoch: ControlPlaneEpoch::initial().next(),
            core_machine_id: ployz_test_support::ids::machine_id("machine_a"),
            active_machines: Vec::new(),
            dataplane_projection: ployz_core::dataplane::DataplaneProjection::try_new(
                Vec::new(),
                None,
            )
            .expect("empty projection"),
            route_bindings: Vec::new(),
            serving_target_entries: Vec::new(),
            volume_pins: Vec::new(),
            nats_authorizations: Vec::new(),
            managed_lease: ployz_core::state::ManagedLeaseProjection::Unacquired,
            custom_certificates: Vec::new(),
            acme_http01_challenges: Vec::new(),
        })
        .expect("intent mirror stores");
    MachinePendingJoinMirror::new(mirror_dir.path().join("pending-machine-joins.json"))
        .store(&PendingMachineJoinRecoverySnapshot {
            epoch: ControlPlaneEpoch::initial().next(),
            pending,
        })
        .expect("pending join mirror stores");

    let promoted_db = tempfile::tempdir().expect("promoted db dir");
    let promoted_config = nats
        .control_config()
        .with_core_db_path(promoted_db.path().join("ployz-core.db"))
        .with_seed_from_mirror(Some(intent_mirror));
    let promoted_runtime = nats.start_control(&promoted_config).await;
    let join_api = nats.join_api();

    let redeemed = redeem_when_ready(&join_api, &accepted.join_token).await;
    assert_eq!(redeemed.result, MachineJoinRedeemResult::Joined);
    assert_eq!(redeemed.machine_id, machine_id("machine_255"));
    assert_eq!(
        redeemed.host_port_assurance,
        ployz_core::install::HostPortAssurance::External
    );
    assert_eq!(redeemed.endpoint_subnet.as_string(), "10.198.0.0/24");
    assert!(
        redeemed
            .secret_delivery
            .nats_credentials
            .secret()
            .starts_with("SU"),
        "promoted core minted fresh machine credentials"
    );

    dataplane::report_join_completed_with_retry(
        &nats,
        machine_id("machine_255"),
        redeemed.secret_delivery.nats_credentials.clone(),
        accepted.join_token,
    )
    .await;

    let machines = api
        .machine_list(&MachineListRequest {})
        .await
        .expect("operator can list after recovered join");
    assert!(
        machines
            .machines
            .iter()
            .any(|machine| { machine.active.machine_id == machine_id("machine_255") })
    );

    promoted_runtime
        .shutdown()
        .await
        .expect("promoted runtime shuts down");
}

/// Redeem before the mint worker reaches material-ready is the typed
/// not-ready response, and the Host Runner-style bounded retry succeeds later.
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
    let join_api = nats.join_api();

    let accepted = machine_add(&api, "op_wait", "idem_wait", "machine_w").await;
    let not_ready = join_api
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
    redeem_when_ready(&join_api, &accepted.join_token).await;

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn rejected_duplicate_machine_add_operation_id_does_not_poison_recovery() {
    let _guard = lock_machine_add_mint_test().await;
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();
    let join_api = nats.join_api();

    let accepted = machine_add(&api, "op_dup", "idem_dup_a", "machine_dup").await;
    let duplicate = machine_add_result(
        &api,
        &MachineAddRequest {
            operation_id: operation_id("op_dup"),
            idempotency_key: idempotency_key("idem_dup_b"),
            machine_id: machine_id("machine_dup_b"),
            name: ployz_sdk_types::MachineName::try_new("machine_dup_b")
                .expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
        },
    )
    .await
    .expect_err("duplicate operation id with new idempotency is rejected");
    assert_eq!(
        duplicate,
        OperationApiClientError::Domain {
            endpoint: OperationApiEndpoint::MachineAdd,
            error: MachineAddError::DuplicateIdempotencyKey {
                operation_id: operation_id("op_dup"),
            },
        }
    );

    let redeemed = redeem_when_ready(&join_api, &accepted.join_token).await;
    assert!(
        redeemed.last_event_sequence.get() > accepted.accepted.start_sequence.get(),
        "redeem response reports the joined event sequence"
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
    let runtime = nats.start_control(&config).await;
    runtime
        .shutdown()
        .await
        .expect("control runtime restarts after rejected duplicate");
}

#[tokio::test]
async fn terminal_machine_add_scrubs_working_secrets_and_status_corruption_is_unavailable() {
    let _guard = lock_machine_add_mint_test().await;
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();
    let join_api = nats.join_api();
    let scrub_operation_id = operation_id("op_scrub");

    let accepted = machine_add(
        &api,
        scrub_operation_id.as_str(),
        "idem_scrub",
        "machine_scrub",
    )
    .await;
    redeem_when_ready(&join_api, &accepted.join_token).await;
    join_api
        .machine_join_report(&MachineJoinReportRequest {
            join_token: accepted.join_token.clone(),
            outcome: MachineJoinReportOutcome::Completed,
        })
        .await
        .expect("machine join completion reports");
    let duplicate = machine_add_result(
        &api,
        &MachineAddRequest {
            operation_id: operation_id("op_scrub_retry"),
            idempotency_key: idempotency_key("idem_scrub"),
            machine_id: machine_id("machine_scrub_retry"),
            name: ployz_sdk_types::MachineName::try_new("machine_scrub_retry")
                .expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
        },
    )
    .await
    .expect_err("terminal scrub keeps the non-secret idempotency tombstone");
    assert_eq!(
        duplicate,
        OperationApiClientError::Domain {
            endpoint: OperationApiEndpoint::MachineAdd,
            error: MachineAddError::DuplicateIdempotencyKey {
                operation_id: operation_id("op_scrub_retry"),
            },
        }
    );

    let conn = rusqlite::Connection::open(nats.work_dir.path().join("ployz-core.db"))
        .expect("core db opens");
    for table in [
        "machine_add_claims",
        "machine_add_submissions",
        "machine_add_join_tokens",
        "machine_add_secret_deliveries",
        "machine_add_mint_claims",
    ] {
        let count: u64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE operation_id = ?1"),
                [scrub_operation_id.as_str()],
                |row| row.get(0),
            )
            .expect("working rows count");
        assert_eq!(count, 0, "{table} keeps terminal secret material");
    }

    conn.execute(
        "UPDATE operations SET status_json = 'not json' WHERE operation_id = ?1",
        [scrub_operation_id.as_str()],
    )
    .expect("status row corrupts");
    drop(conn);

    let error = api
        .ops_status(&OpsStatusRequest {
            operation_id: scrub_operation_id.clone(),
        })
        .await
        .expect_err("corrupt status is not reported as missing");
    assert!(
        matches!(
            error,
            OperationApiClientError::Domain {
                endpoint: OperationApiEndpoint::OpsStatus,
                error: OpsStatusError::Unavailable {
                    operation_id: ref unavailable_id,
                    ..
                },
            } if unavailable_id == &scrub_operation_id
        ),
        "expected unavailable status error, got {error:?}"
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn duplicate_machine_add_joined_event_adopts_existing_join() {
    let _guard = lock_machine_add_mint_test().await;
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();
    let join_api = nats.join_api();

    let accepted = machine_add(&api, "op_join_race", "idem_join_race", "machine_jr").await;
    redeem_when_ready(&join_api, &accepted.join_token).await;
    let repository = operation_repository_for_test(&nats).await;

    let outcome = repository
        .record_machine_add_joined(
            &operation_id("op_join_race"),
            &machine_id("machine_jr"),
            JoinTokenRedeemedAt::try_new(999).expect("valid join time"),
        )
        .await
        .expect("duplicate joined event adopts stored join");
    assert!(matches!(
        outcome,
        OperationStatusWrite::AlreadySatisfied { .. }
    ));

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn machine_add_reusing_another_operations_idempotency_key_is_rejected() {
    let _guard = lock_machine_add_mint_test().await;
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();

    machine_add(&api, "op_a", "idem_a", "machine_a").await;
    machine_add(&api, "op_b", "idem_b", "machine_b").await;

    // op_b already exists; reusing op_a's idempotency key must not return
    // op_a's join material — it is a duplicate key, not an adopt of op_b.
    let mismatch = machine_add_result(
        &api,
        &MachineAddRequest {
            operation_id: operation_id("op_b"),
            idempotency_key: idempotency_key("idem_a"),
            machine_id: machine_id("machine_b"),
            name: ployz_sdk_types::MachineName::try_new("machine_b").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
        },
    )
    .await
    .expect_err("reusing another operation's idempotency key is rejected");
    assert_eq!(
        mismatch,
        OperationApiClientError::Domain {
            endpoint: OperationApiEndpoint::MachineAdd,
            error: MachineAddError::DuplicateIdempotencyKey {
                operation_id: operation_id("op_b"),
            },
        }
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

async fn operation_repository_for_test(nats: &TestNats) -> OperationRepository {
    OperationRepository::open(
        CoreStore::open(nats.work_dir.path().join("ployz-core.db"))
            .await
            .expect("core store opens"),
        nats.connected.controller.clone(),
    )
}

fn secret_row_count(nats: &TestNats, table: &str, operation: &str) -> u64 {
    let conn = rusqlite::Connection::open(nats.work_dir.path().join("ployz-core.db"))
        .expect("core db opens");
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE operation_id = ?1"),
        [operation],
        |row| row.get(0),
    )
    .expect("secret rows count")
}

async fn machine_add(
    api: &OperationApiClient,
    operation: &str,
    idempotency: &str,
    machine: &str,
) -> MachineAddAccepted {
    machine_add_with_assurance(
        api,
        operation,
        idempotency,
        machine,
        ployz_core::install::HostPortAssurance::Keeper,
    )
    .await
}

async fn machine_add_with_assurance(
    api: &OperationApiClient,
    operation: &str,
    idempotency: &str,
    machine: &str,
    host_port_assurance: ployz_core::install::HostPortAssurance,
) -> MachineAddAccepted {
    machine_add_result(
        api,
        &MachineAddRequest {
            operation_id: operation_id(operation),
            idempotency_key: idempotency_key(idempotency),
            machine_id: machine_id(machine),
            name: ployz_sdk_types::MachineName::try_new(machine).expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            host_port_assurance,
        },
    )
    .await
    .expect("machine add accepts")
}

/// Send a machine-add, retrying only transient transport (`Request`) timeouts.
/// machine-add is idempotent — the operation id and idempotency key dedupe the
/// re-send server-side — so a request that times out under a starved CI runner
/// is safely repeated. A settled result (`Ok`, or a typed `MachineAddError`
/// domain rejection) is surfaced as soon as it arrives; the retry is bounded
/// (~20s) so a genuinely wedged runtime still fails the test rather than hanging.
async fn machine_add_result(
    api: &OperationApiClient,
    request: &MachineAddRequest,
) -> Result<MachineAddAccepted, OperationApiClientError<MachineAddError>> {
    for _ in 0..200 {
        match api.machine_add(request).await {
            Err(OperationApiClientError::Request { .. }) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            settled => return settled,
        }
    }
    api.machine_add(request).await
}

fn public_key_of(seed: &str) -> NatsUserPublicKey {
    let pair = nkeys::KeyPair::from_seed(seed).expect("minted seed parses");
    NatsUserPublicKey::try_new(pair.public_key()).expect("derived public key is valid")
}

fn rendered_principal_key(
    users: &[NatsAuthorizationGrant],
    machine: &str,
) -> Option<NatsUserPublicKey> {
    users.iter().find_map(|grant| {
        let NatsAuthorizationGrant::Internal {
            authority:
                NatsInternalAuthority::Machine {
                    machine_id: granted,
                },
            public_key,
        } = grant
        else {
            return None;
        };
        (granted == &machine_id(machine)).then(|| public_key.clone())
    })
}
