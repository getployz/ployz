use ployz_core::{
    AdvertisedEndpoint, MachineName, MachineRpc, MachineRpcClient, RecoverSealingKeyRequest,
    RecoveryCode, RecoveryRefusal, RecoverySeed, RpcErrorCode, RpcResponseBody, Sealing,
    SealingKeyBackup, SealingPublicKey, WrapSealingKeyRequest, op,
};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Code, Request, transport::Server};

use super::super::{
    LocalMachine, LocalMachineError, LocalMachineStore, RecordOwner, RecoveryError,
};
use super::harness::{machine_service, open_store, participating};
use crate::corrosion::{AdminClient, ReplicatedStore, fake_cluster};
use crate::machine_api::MachineApi;
use crate::sealing_key::SealingKey;
use crate::sealing_recovery::back_up;

async fn awaiting(
    expected: SealingPublicKey,
    backup: Option<SealingKeyBackup>,
) -> (
    LocalMachine,
    ReplicatedStore,
    std::path::PathBuf,
    tokio::task::JoinHandle<()>,
) {
    let (local, replicated, data_dir, server) = seeded(expected, backup).await;
    replicated
        .publish_sealing_awaits_recovery(expected)
        .await
        .unwrap();
    (local, replicated, data_dir, server)
}

/// A founder seeded for recovery whose publisher has not replicated the awaits marker.
async fn seeded(
    expected: SealingPublicKey,
    backup: Option<SealingKeyBackup>,
) -> (
    LocalMachine,
    ReplicatedStore,
    std::path::PathBuf,
    tokio::task::JoinHandle<()>,
) {
    let (replicated, server) = fake_cluster::store_with_subscriptions().await;
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-recover-{}",
        ployz_core::MachineId::random()
    ));
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    store
        .enroll_for_recovery(
            ployz_core::EnrollRequest {
                role: ployz_core::EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: MachineName::parse("edge").unwrap(),
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: None,
                    advertised_endpoints: vec![AdvertisedEndpoint(
                        "192.0.2.1:51820".parse().unwrap(),
                    )],
                },
                wireguard_mtu: None,
                pinned: Default::default(),
            },
            RecoverySeed {
                expected_public_key: expected,
                backup,
            },
        )
        .unwrap();
    replicated
        .publish_local_machine(store.record().machine().unwrap())
        .await
        .unwrap();
    let local = LocalMachine::new(RecordOwner::spawn(store).unwrap()).with_cluster(Some((
        replicated.clone(),
        AdminClient::new("/no/such/ployz-admin.sock"),
    )));
    (local, replicated, data_dir, server)
}

fn wrap(local: &LocalMachine, code: &RecoveryCode) -> ployz_core::SealingKeyWrapped {
    local
        .wrap_sealing_key(WrapSealingKeyRequest { code: code.clone() }, false)
        .unwrap()
}

async fn recover(
    local: &LocalMachine,
    code: &RecoveryCode,
    backup: Option<SealingKeyBackup>,
) -> Result<SealingPublicKey, LocalMachineError> {
    local
        .recover_sealing_key(
            RecoverSealingKeyRequest {
                code: code.clone(),
                backup,
            },
            false,
        )
        .await
        .map(|recovered| recovered.public_key)
}

fn held(data_dir: &std::path::Path) -> Option<SealingPublicKey> {
    SealingKey::load(data_dir)
        .unwrap()
        .map(|key| key.public_key())
}

#[tokio::test]
async fn a_wrapped_key_recovers_onto_a_founder_awaiting_it_and_is_published() {
    let (holder, _, _, holder_dir, holder_server) = participating().await;
    let code = RecoveryCode::generate();
    let wrapped = wrap(&holder, &code);
    assert_eq!(Some(wrapped.public_key), held(&holder_dir));

    let (recoverer, replicated, dir, server) = awaiting(wrapped.public_key, None).await;
    assert_eq!(held(&dir), None);

    let recovered = recover(&recoverer, &code, Some(wrapped.backup))
        .await
        .unwrap();

    assert_eq!(recovered, wrapped.public_key);
    assert_eq!(held(&dir), Some(wrapped.public_key));
    assert_eq!(
        replicated.sealing_public_key().await.unwrap(),
        Some(wrapped.public_key)
    );
    holder_server.abort();
    server.abort();
    let _ = std::fs::remove_dir_all(holder_dir);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn the_published_row_follows_what_the_machine_holds_of_the_sealing_key() {
    use std::time::Duration;

    let key = SealingKey::generate();
    let code = RecoveryCode::generate();
    let (local, replicated, dir, server) =
        awaiting(key.public_key(), Some(back_up(&key, &code))).await;
    let id = local.owner().record().id();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let view = crate::corrosion::MachineView::start(replicated.clone(), shutdown.clone());
    let publisher = tokio::spawn(crate::corrosion::run_machine_publisher(
        Some((replicated.clone(), view.clone())),
        local.owner().clone(),
        crate::corrosion::LiveRuntime::unobserved(),
        shutdown.clone(),
    ));
    let published = |expected: Option<Sealing>| {
        let mut machines = view.watch();
        async move {
            tokio::time::timeout(
                Duration::from_secs(5),
                machines.wait_for(|snapshot| {
                    snapshot.as_ref().is_some_and(|snapshot| {
                        snapshot
                            .observations
                            .iter()
                            .any(|machine| machine.id == id && machine.runtime.sealing == expected)
                    })
                }),
            )
            .await
            .unwrap_or_else(|_| panic!("{expected:?} was never published"))
            .unwrap();
        }
    };

    published(Some(Sealing::AwaitsRecovery {
        public_key: key.public_key(),
    }))
    .await;
    recover(&local, &code, None).await.unwrap();
    published(Some(Sealing::Held {
        public_key: key.public_key(),
    }))
    .await;

    shutdown.cancel();
    publisher.await.unwrap().unwrap();
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_founder_recovers_from_the_backup_it_was_founded_with() {
    let key = SealingKey::generate();
    let code = RecoveryCode::generate();
    let (local, _, dir, server) = awaiting(key.public_key(), Some(back_up(&key, &code))).await;

    assert_eq!(
        recover(&local, &code, None).await.unwrap(),
        key.public_key()
    );
    assert_eq!(held(&dir), Some(key.public_key()));
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_founder_recovers_before_its_awaits_marker_replicates() {
    let key = SealingKey::generate();
    let code = RecoveryCode::generate();
    let (local, replicated, dir, server) =
        seeded(key.public_key(), Some(back_up(&key, &code))).await;
    assert_eq!(replicated.sealing_awaits_recovery().await.unwrap(), None);

    assert_eq!(
        recover(&local, &code, None).await.unwrap(),
        key.public_key()
    );
    assert_eq!(held(&dir), Some(key.public_key()));
    assert_eq!(
        replicated.sealing_public_key().await.unwrap(),
        Some(key.public_key())
    );
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn refused_recoveries_keep_no_key_and_publish_nothing() {
    let key = SealingKey::generate();
    let code = RecoveryCode::generate();
    let (local, replicated, dir, server) = awaiting(key.public_key(), None).await;

    let wrong_code = recover(
        &local,
        &RecoveryCode::generate(),
        Some(back_up(&key, &code)),
    )
    .await;
    assert!(matches!(
        wrong_code,
        Err(LocalMachineError::Recovery(RecoveryError::WrongCode))
    ));
    let other_key = recover(&local, &code, Some(back_up(&SealingKey::generate(), &code))).await;
    assert!(matches!(
        other_key,
        Err(LocalMachineError::Recovery(RecoveryError::OtherKey(expected))) if expected == key.public_key()
    ));
    let malformed = recover(
        &local,
        &code,
        Some(SealingKeyBackup::from_bytes(vec![0; 8])),
    )
    .await;
    assert!(matches!(
        malformed,
        Err(LocalMachineError::Recovery(RecoveryError::Malformed))
    ));
    assert!(matches!(
        recover(&local, &code, None).await,
        Err(LocalMachineError::Recovery(RecoveryError::NoBackup))
    ));
    assert_eq!(held(&dir), None);
    assert_eq!(replicated.sealing_public_key().await.unwrap(), None);
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_holder_still_checks_the_code_against_its_own_key() {
    let (holder, replicated, _, dir, server) = participating().await;
    let code = RecoveryCode::generate();
    let wrapped = wrap(&holder, &code);
    replicated
        .publish_sealing_public_key(wrapped.public_key)
        .await
        .unwrap();

    let wrong = recover(
        &holder,
        &RecoveryCode::generate(),
        Some(wrapped.backup.clone()),
    )
    .await;
    assert!(matches!(
        wrong,
        Err(LocalMachineError::Recovery(RecoveryError::WrongCode))
    ));
    assert_eq!(
        recover(&holder, &code, Some(wrapped.backup)).await.unwrap(),
        wrapped.public_key
    );
    assert_eq!(held(&dir), Some(wrapped.public_key));
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_machine_holding_another_key_refuses_to_replace_it() {
    let (holder, replicated, _, dir, server) = participating().await;
    let own = held(&dir).unwrap();
    let named = SealingKey::generate();
    let code = RecoveryCode::generate();
    replicated
        .publish_sealing_public_key(named.public_key())
        .await
        .unwrap();

    let refused = recover(&holder, &code, Some(back_up(&named, &code))).await;

    assert!(matches!(
        refused,
        Err(LocalMachineError::Recovery(RecoveryError::HeldAnother { held, recovered }))
            if held == own && recovered == named.public_key()
    ));
    assert_eq!(held(&dir), Some(own));
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_cluster_that_names_no_key_has_nothing_to_recover() {
    let (holder, _, _, dir, server) = participating().await;
    let code = RecoveryCode::generate();
    let wrapped = wrap(&holder, &code);

    assert!(matches!(
        recover(&holder, &code, Some(wrapped.backup)).await,
        Err(LocalMachineError::Recovery(RecoveryError::NothingAwaited))
    ));
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

fn routed<T>(body: T, cloud_routed: bool) -> Request<T> {
    let mut request = Request::new(body);
    if cloud_routed {
        request
            .metadata_mut()
            .insert(ployz_core::CLOUD_ROUTED_HEADER, "1".parse().unwrap());
    }
    request
}

#[tokio::test]
async fn the_management_tunnel_never_carries_a_recovery_code() {
    let (replicated, server) = fake_cluster::store().await;
    let (dir, store, founder) = open_store("ployzd-recover-routed");
    replicated.publish_local_machine(&founder).await.unwrap();
    let service = machine_service(store, replicated, None);
    let code = RecoveryCode::generate();
    let wrap = || {
        op::WrapSealingKey::into_request(WrapSealingKeyRequest { code: code.clone() })
            .encode()
            .unwrap()
    };
    let recover = || {
        op::RecoverSealingKey::into_request(RecoverSealingKeyRequest {
            code: code.clone(),
            backup: None,
        })
        .encode()
        .unwrap()
    };

    for response in [
        service.wrap_sealing_key(routed(wrap(), true)).await,
        service.recover_sealing_key(routed(recover(), true)).await,
    ] {
        let response = response.unwrap().into_inner().decode_response().unwrap();
        let RpcResponseBody::Error(error) = response.body else {
            panic!("a Cloud-routed recovery code was accepted");
        };
        assert_eq!(error.code, RpcErrorCode::Unsupported, "{error:?}");
        assert_eq!(error.recovery_refusal(), Some(RecoveryRefusal::CloudRouted));
    }
    let direct = service
        .wrap_sealing_key(routed(wrap(), false))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap()
        .decode::<op::WrapSealingKey>()
        .unwrap();
    assert_eq!(Some(direct.public_key), held(&dir));
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn the_cloud_slot_needs_full_access_to_wrap_or_recover() {
    let (replicated, cluster) = fake_cluster::store().await;
    let (dir, store, founder) = open_store("ployzd-recover-scope");
    replicated.publish_local_machine(&founder).await.unwrap();
    let api = MachineApi::from_local(machine_service(store, replicated, None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server =
        tokio::spawn(Server::builder().serve_with_incoming(api, TcpListenerStream::new(listener)));
    let mut client = MachineRpcClient::connect(format!("http://{address}"))
        .await
        .unwrap();
    let code = RecoveryCode::generate();
    let operator = |body| {
        let mut request = Request::new(body);
        request
            .metadata_mut()
            .insert(ployz_core::OPERATOR_SCOPE_HEADER, "1".parse().unwrap());
        request
    };

    let wrap = client
        .wrap_sealing_key(operator(
            op::WrapSealingKey::into_request(WrapSealingKeyRequest { code: code.clone() })
                .encode()
                .unwrap(),
        ))
        .await
        .expect_err("the cloud slot must not wrap the Sealing Key");
    let recover = client
        .recover_sealing_key(operator(
            op::RecoverSealingKey::into_request(RecoverSealingKeyRequest { code, backup: None })
                .encode()
                .unwrap(),
        ))
        .await
        .expect_err("the cloud slot must not recover the Sealing Key");

    for (status, capability) in [
        (wrap, "ployz.sealing.wrap.v1"),
        (recover, "ployz.sealing.recover.v1"),
    ] {
        assert_eq!(status.code(), Code::PermissionDenied, "{status:?}");
        assert!(
            status
                .message()
                .contains(&format!("{capability} needs Full access")),
            "{status:?}"
        );
    }
    server.abort();
    cluster.abort();
    let _ = std::fs::remove_dir_all(dir);
}
