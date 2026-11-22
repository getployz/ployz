use super::*;
use ployz_core::authority::MismatchField;
use ployz_core::{
    ApproveJoinRequest, DeviceKey, EnrollAs, EnrollRequest, SetDeviceRequest,
    ShareSealingKeyRequest,
    authority::{AuthorityRefusal, Signed, Signer, Timestamp},
};

use crate::machine::StoreError;

/// The pin a CLI holding `signer`'s Device sends.
fn pinned(signer: &Signer) -> ployz_core::JoinPin {
    ployz_core::JoinPin {
        devices: vec![signer.public().into()],
    }
}

/// Approve `signer` at `entry`'s console, as a person does on a Server born enforcing.
async fn enforce(entry: &LocalMachine, signer: &Signer) {
    entry
        .clone()
        .on_local_socket()
        .set_device(SetDeviceRequest::Approve {
            key: signer.public().into(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn enforced_register_refuses_unsigned_and_unknown_signers_without_publication_or_wrapping() {
    let (entry, replicated, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let mut request = request("attacker", WireGuardPublicKey([2; 32]));
    assert!(matches!(
        entry.register(request.clone()).await,
        Err(LocalMachineError::JoinRefused(
            AuthorityRefusal::Unsigned { .. }
        ))
    ));
    assert!(
        replicated
            .machine(request.machine_id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    let mut body = entry.pending_joins().unwrap().joins.pop().unwrap();
    let attacker = Signer::from_seed(&[2; 32]);
    body.signer = attacker.public().into();
    request.join_approval = Some(Signed::sign(&body, &attacker).unwrap());
    assert!(matches!(
        entry.register(request.clone()).await,
        Err(LocalMachineError::JoinRefused(
            AuthorityRefusal::UnknownKey { .. }
        ))
    ));
    assert!(
        replicated
            .machine(request.machine_id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_pending_join_lists_the_passkeys_the_admitting_server_trusts() {
    let (entry, _, _, directory, server) = participating().await;
    enforce(&entry, &Signer::from_seed(&[1; 32])).await;
    let passkey = crate::machine::test_support::passkey(5);
    let trusted = ployz_core::authority::TrustedPasskey {
        passkey: passkey.clone(),
        approved_by: ployz_core::authority::PasskeyApprover::Server,
    };
    entry
        .owner()
        .mutate(move |store| store.approve_passkey(trusted))
        .await
        .unwrap()
        .unwrap();
    let joiner = request("joiner", WireGuardPublicKey([3; 32]));
    assert!(entry.register(joiner).await.is_err());

    let body = entry.pending_joins().unwrap().joins.pop().unwrap();

    assert_eq!(body.passkeys, [passkey]);
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn approved_join_uses_only_encrypted_approved_devices_and_cannot_be_redeemed_twice() {
    let (entry, _, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let mut request = request("joiner", joiner.record().private_key().public_key());
    request.machine_id = joiner.record().id();
    assert!(entry.register(request.clone()).await.is_err());
    let mut body = entry.pending_joins().unwrap().joins.pop().unwrap();
    body.signer = signer.public().into();
    let approval = Signed::sign(&body, &signer).unwrap();
    entry
        .approve_join(ApproveJoinRequest {
            approval: approval.clone(),
        })
        .await
        .unwrap();
    let mut registration = entry.register(request.clone()).await.unwrap();
    registration.devices = vec![DeviceKey::from_bytes([9; 32])];
    assert_eq!(
        joiner
            .enroll(EnrollRequest {
                role: EnrollAs::Member {
                    pin: Some(pinned(&signer)),
                    registration,
                },
                wireguard_mtu: None,
                pinned: Default::default()
            })
            .unwrap(),
        EnrollPlan::Fresh(())
    );
    assert_eq!(
        joiner.record().devices().collect::<Vec<_>>(),
        vec![signer.public().into()]
    );
    assert!(joiner.record().filters_peers());
    assert!(entry.register(request.clone()).await.is_ok());
    confirm(&entry, &request, &approval, join_dir.path()).await;
    assert!(matches!(
        entry
            .approve_join(ApproveJoinRequest {
                approval: approval.clone()
            })
            .await,
        Err(LocalMachineError::JoinConsumed(_))
    ));
    assert!(matches!(
        entry
            .share_sealing_key(ShareSealingKeyRequest {
                join_approval: Some(approval),
                machine: request.machine_id,
                public_key: request.public_key
            })
            .await,
        Err(LocalMachineError::JoinConsumed(_))
    ));
    drop(entry);
    let owner = RecordOwner::spawn(LocalMachineStore::open(&directory).unwrap()).unwrap();
    let reopened = LocalMachine::new(owner);
    assert!(matches!(
        reopened.require_unused_join(request.machine_id),
        Err(LocalMachineError::JoinConsumed(_))
    ));
    server.abort();
    drop(reopened);
    std::fs::remove_dir_all(directory).unwrap();
}

/// A joiner takes only the key its install command named. A join answered under another
/// trusted key spends nothing for good: the join is offered again, another approval under
/// the wrong key is refused, and a fresh one under the key the joiner waits for replaces
/// the spent answer. Once the joiner confirms, the join is closed.
#[tokio::test]
async fn a_join_answered_under_another_key_heals_with_the_key_the_joiner_waits_for() {
    let (entry, _, _, directory, server) = participating().await;
    let wrong = Signer::from_seed(&[1; 32]);
    let right = Signer::from_seed(&[2; 32]);
    enforce(&entry, &wrong).await;
    enforce(&entry, &right).await;
    let joiner = LocalMachineStore::open(tempfile::tempdir().unwrap().path()).unwrap();
    let mut request = request("joiner", joiner.record().private_key().public_key());
    request.machine_id = joiner.record().id();
    let waits_for = ployz_core::first_key::FirstKeyHint::Device {
        code: DeviceKey::from(right.public()).code(),
    };
    request.first_key = Some(waits_for);
    assert!(entry.register(request.clone()).await.is_err());
    assert_eq!(
        entry
            .pending_joins()
            .unwrap()
            .first_keys
            .get(&request.machine_id),
        Some(&waits_for)
    );
    let approve = async |signer: &Signer| {
        let mut body = entry.pending_joins().unwrap().joins.pop().unwrap();
        body.signer = signer.public().into();
        let approval = Signed::sign(&body, signer).unwrap();
        entry
            .approve_join(ApproveJoinRequest {
                approval: approval.clone(),
            })
            .await
            .map(|_| approval)
    };

    let spent = approve(&wrong).await.unwrap();
    let answered = entry.register(request.clone()).await.unwrap();
    assert_eq!(answered.assigned_machine.join_approval, Some(spent));
    assert!(matches!(
        approve(&wrong).await,
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Mismatch {
            field: MismatchField::FirstKey
        }))
    ));
    let fresh = approve(&right).await.unwrap();
    let healed = entry.register(request.clone()).await.unwrap();
    assert_eq!(healed.assigned_machine.join_approval, Some(fresh.clone()));
    assert!(healed.sealing_key.is_some());

    let key = crate::sealing_key::SealingKey::load(&directory)
        .unwrap()
        .unwrap();
    entry
        .register(ployz_core::RegisterRequest {
            join_confirmation: Some(key.confirm_join(&fresh)),
            ..request.clone()
        })
        .await
        .unwrap();
    assert!(entry.pending_joins().unwrap().joins.is_empty());
    let mut body = fresh.open().unwrap();
    body.issued_at = body.issued_at.plus(1);
    assert!(matches!(
        entry
            .approve_join(ApproveJoinRequest {
                approval: Signed::sign(&body, &right).unwrap(),
            })
            .await,
        Err(LocalMachineError::JoinConsumed(_))
    ));
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

/// The draft of a join answered under the wrong key can go before anyone approves it
/// again: its window closes and it is pruned, or the answer predates kept drafts. The
/// joiner's next Register offers the join again with a fresh window and the key it waits
/// for, and the old answer still stands until an approval under that key replaces it.
#[tokio::test]
async fn a_wrong_key_join_is_offered_again_after_its_draft_is_gone() {
    let (entry, _, _, directory, server) = participating().await;
    let wrong = Signer::from_seed(&[1; 32]);
    let right = Signer::from_seed(&[2; 32]);
    enforce(&entry, &wrong).await;
    enforce(&entry, &right).await;
    let joiner = LocalMachineStore::open(tempfile::tempdir().unwrap().path()).unwrap();
    let mut request = request("joiner", joiner.record().private_key().public_key());
    request.machine_id = joiner.record().id();
    let waits_for = ployz_core::first_key::FirstKeyHint::Device {
        code: DeviceKey::from(right.public()).code(),
    };
    request.first_key = Some(waits_for);
    assert!(entry.register(request.clone()).await.is_err());
    let approve = async |signer: &Signer| {
        let mut body = entry.pending_joins().unwrap().joins.pop().expect("offered");
        body.signer = signer.public().into();
        let approval = Signed::sign(&body, signer).unwrap();
        entry
            .approve_join(ApproveJoinRequest {
                approval: approval.clone(),
            })
            .await
            .map(|_| approval)
    };
    let spent = approve(&wrong).await.unwrap();
    entry.register(request.clone()).await.unwrap();
    std::fs::remove_file(directory.join("join-approvals.json")).unwrap();

    let replayed = entry.register(request.clone()).await.unwrap();

    assert_eq!(replayed.assigned_machine.join_approval, Some(spent));
    assert_eq!(
        entry
            .pending_joins()
            .unwrap()
            .first_keys
            .get(&request.machine_id),
        Some(&waits_for)
    );
    let fresh = approve(&right).await.unwrap();
    let healed = entry.register(request.clone()).await.unwrap();
    assert_eq!(healed.assigned_machine.join_approval, Some(fresh));
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

/// Once a Forget answers, this Machine holds the forgotten Server removed. Every join route
/// for it is refused after that, a resend carrying the approval it was admitted under
/// included, and none lists it again.
#[tokio::test]
async fn a_forgotten_server_is_refused_on_every_join_route_even_with_its_old_approval() {
    let (entry, replicated, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let request = request("joiner", WireGuardPublicKey([2; 32]));
    assert!(entry.register(request.clone()).await.is_err());
    let mut body = entry.pending_joins().unwrap().joins.pop().unwrap();
    body.signer = signer.public().into();
    let approval = Signed::sign(&body, &signer).unwrap();
    entry
        .approve_join(ApproveJoinRequest {
            approval: approval.clone(),
        })
        .await
        .unwrap();
    entry.register(request.clone()).await.unwrap();

    assert!(matches!(
        entry
            .forget(ployz_core::ForgetRequest {
                machine_id: request.machine_id,
            })
            .await
            .unwrap(),
        ployz_core::Forgotten::Forgotten
    ));
    assert!(
        entry
            .record()
            .removes_member(request.machine_id, Some(request.public_key))
    );

    let gone = request.machine_id;
    let mut replayed = request.clone();
    replayed.join_approval = Some(approval.clone());
    let mut rekeyed = request.clone();
    rekeyed.public_key = WireGuardPublicKey([4; 32]);
    for register in [request.clone(), replayed, rekeyed] {
        assert!(matches!(
            entry.register(register).await,
            Err(LocalMachineError::MemberRemoved(id)) if id == gone
        ));
    }
    assert!(matches!(
        entry
            .approve_join(ApproveJoinRequest {
                approval: approval.clone(),
            })
            .await,
        Err(LocalMachineError::MemberRemoved(id)) if id == gone
    ));
    assert!(matches!(
        entry
            .share_sealing_key(ShareSealingKeyRequest {
                join_approval: Some(approval),
                machine: gone,
                public_key: request.public_key,
            })
            .await,
        Err(LocalMachineError::MemberRemoved(id)) if id == gone
    ));
    assert!(replicated.machine(gone.as_str()).await.unwrap().is_none());
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

/// An expired approval is refused, and the time this Machine saw is kept, so a clock set
/// back later never reopens the window.
#[tokio::test]
async fn an_expired_join_still_raises_the_persisted_clock_floor() {
    let (entry, _, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let request = request("joiner", WireGuardPublicKey([2; 32]));
    assert!(entry.register(request.clone()).await.is_err());
    let mut body = entry.pending_joins().unwrap().joins.pop().unwrap();
    body.signer = signer.public().into();
    body.issued_at = Timestamp::from_millis(1_000).unwrap();
    body.expires_at = body.issued_at.plus(60_000);
    let before = Timestamp::now();
    assert!(matches!(
        entry
            .approve_join(ApproveJoinRequest {
                approval: Signed::sign(&body, &signer).unwrap(),
            })
            .await,
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Expired))
    ));
    let log: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("restart-approvals.json")).unwrap())
            .unwrap();
    let floor: Timestamp =
        serde_json::from_value(log.get("expiry_time_floor").unwrap().clone()).unwrap();
    assert!(floor.as_millis() >= before.as_millis());
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn another_admitting_machine_or_changed_recipient_cannot_use_the_approval() {
    let (entry, _, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let request = request("joiner", WireGuardPublicKey([2; 32]));
    assert!(entry.register(request.clone()).await.is_err());
    let mut body = entry.pending_joins().unwrap().joins.pop().unwrap();
    body.signer = signer.public().into();
    body.admitting_machines = vec![MachineId::random()];
    let approval = Signed::sign(&body, &signer).unwrap();
    assert!(matches!(
        entry.approve_join(ApproveJoinRequest { approval }).await,
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Mismatch {
            field: MismatchField::AdmittingMachine
        }))
    ));
    body.admitting_machines = vec![entry.record().id()];
    body.devices.push(DeviceKey::from_bytes([9; 32]));
    let approval = Signed::sign(&body, &signer).unwrap();
    assert!(matches!(
        entry.approve_join(ApproveJoinRequest { approval }).await,
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Mismatch {
            field: MismatchField::ApprovedDevices
        }))
    ));
    body.devices.pop();
    body.passkeys.push(crate::machine::test_support::passkey(6));
    let approval = Signed::sign(&body, &signer).unwrap();
    assert!(matches!(
        entry.approve_join(ApproveJoinRequest { approval }).await,
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Mismatch {
            field: MismatchField::ApprovedPasskeys
        }))
    ));
    body.passkeys.pop();
    body.sealing_recipient = WireGuardPublicKey([3; 32]);
    let approval = Signed::sign(&body, &signer).unwrap();
    assert!(matches!(
        entry.approve_join(ApproveJoinRequest { approval }).await,
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Mismatch {
            field: MismatchField::SealingRecipient
        }))
    ));
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn an_unsigned_share_is_refused() {
    let (entry, _, _, directory, server) = participating().await;
    let request = request("joiner", WireGuardPublicKey([2; 32]));
    register_published(&entry, &request).await.unwrap();
    assert!(matches!(
        entry
            .share_sealing_key(ShareSealingKeyRequest {
                join_approval: None,
                machine: request.machine_id,
                public_key: request.public_key
            })
            .await,
        Err(LocalMachineError::JoinRefused(
            AuthorityRefusal::Unsigned { .. }
        ))
    ));
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn enforcing_mesh_tcp_never_grants_full_scope_or_slot_administration() {
    use ployz_core::{MachineRpcClient, SetManagementClientRequest, op};
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::{Request, transport::Server};
    let (entry, _, _, directory, cluster_server) = participating().await;
    enforce(&entry, &Signer::from_seed(&[1; 32])).await;
    let owner = entry.owner().clone();
    let api = crate::machine_api::MachineApi::builder(owner.clone()).build();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server =
        tokio::spawn(Server::builder().serve_with_incoming(api, TcpListenerStream::new(listener)));
    let mut client = MachineRpcClient::connect(format!("http://{address}"))
        .await
        .unwrap();
    let device = DeviceKey::from_bytes([9; 32]);
    assert_eq!(
        client
            .set_device(Request::new(
                op::SetDevice::into_request(SetDeviceRequest::Approve { key: device })
                    .encode()
                    .unwrap()
            ))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::PermissionDenied
    );
    assert_eq!(
        client
            .wrap_sealing_key(Request::new(ployz_core::OpaquePayload {
                json: "{}".into()
            }))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::PermissionDenied
    );
    assert_eq!(
        client
            .set_management_client(Request::new(
                op::SetManagementClient::into_request(SetManagementClientRequest::Set {
                    label: ployz_core::ManagementClientLabel::parse("cloud").unwrap()
                })
                .encode()
                .unwrap()
            ))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::PermissionDenied
    );
    assert!(!owner.record().devices().any(|key| key == device));
    server.abort();
    drop(client);
    drop(owner);
    drop(entry);
    cluster_server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn join_approval_lifetime_and_issue_time_are_bounded_before_admission() {
    let (entry, replicated, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let request = request("joiner", WireGuardPublicKey([2; 32]));
    assert!(entry.register(request.clone()).await.is_err());
    let body = entry.pending_joins().unwrap().joins.pop().unwrap();
    let now = Timestamp::now();
    let intervals = [
        (now, now.plus(16 * 60 * 1000)),
        (now.plus(3 * 60 * 1000), now.plus(18 * 60 * 1000)),
        (now.plus(1000), now),
        (
            Timestamp::from_millis(now.as_millis() - 18 * 60 * 1000).unwrap(),
            Timestamp::from_millis(now.as_millis() - 3 * 60 * 1000).unwrap(),
        ),
    ];
    for (issued_at, expires_at) in intervals {
        let mut body = body.clone();
        body.signer = signer.public().into();
        body.issued_at = issued_at;
        body.expires_at = expires_at;
        let approval = Signed::sign(&body, &signer).unwrap();
        assert!(matches!(
            entry.approve_join(ApproveJoinRequest { approval }).await,
            Err(LocalMachineError::JoinRefused(_))
        ));
        assert!(
            replicated
                .machine(request.machine_id.as_str())
                .await
                .unwrap()
                .is_none()
        );
    }
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn join_consumption_uses_the_shared_persisted_approval_clock_floor() {
    let (entry, replicated, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    drop(entry);
    std::fs::write(
        directory.join("restart-approvals.json"),
        serde_json::to_vec(&serde_json::json!({
            "expiry_time_floor": Timestamp::now().plus(18 * 60 * 1000),
        }))
        .unwrap(),
    )
    .unwrap();
    let owner = RecordOwner::spawn(LocalMachineStore::open(&directory).unwrap()).unwrap();
    let entry = LocalMachine::new(owner).with_cluster(Some((
        replicated.clone(),
        crate::corrosion::AdminClient::new("/no/admin"),
    )));
    let request = request("joiner", WireGuardPublicKey([2; 32]));
    assert!(entry.register(request.clone()).await.is_err());
    let mut body = entry.pending_joins().unwrap().joins.pop().unwrap();
    body.signer = signer.public().into();
    assert!(matches!(
        entry
            .approve_join(ApproveJoinRequest {
                approval: Signed::sign(&body, &signer).unwrap(),
            })
            .await,
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Expired))
    ));
    assert!(entry.register(request.clone()).await.is_err());
    assert!(
        replicated
            .machine(request.machine_id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

/// Cloud relays the pending body to the person, so it can add a Machine of its own to the
/// bootstrap peers they sign. The admitter seals no capsule for it.
#[tokio::test]
async fn an_approval_naming_a_bootstrap_peer_this_machine_never_listed_is_refused() {
    let (entry, _, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let request = request("joiner", WireGuardPublicKey([2; 32]));
    assert!(entry.register(request.clone()).await.is_err());
    let mut body = entry.pending_joins().unwrap().joins.pop().unwrap();
    body.signer = signer.public().into();
    let mut clouds = body.bootstrap.first().unwrap().clone();
    clouds.id = MachineId::random();
    clouds.public_key = WireGuardPublicKey([7; 32]);
    body.bootstrap.push(clouds);
    entry
        .approve_join(ApproveJoinRequest {
            approval: Signed::sign(&body, &signer).unwrap(),
        })
        .await
        .unwrap();

    assert!(matches!(
        entry.register(request).await,
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Mismatch {
            field: MismatchField::BootstrapPeers
        }))
    ));
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

async fn approved_registration(
    entry: &LocalMachine,
    joiner: &LocalMachineStore,
    signer: &Signer,
) -> (ployz_core::RegisterRequest, ployz_core::Registered) {
    let mut request = request("approved", joiner.record().private_key().public_key());
    request.machine_id = joiner.record().id();
    assert!(entry.register(request.clone()).await.is_err());
    let mut body = entry.pending_joins().unwrap().joins.pop().unwrap();
    body.signer = signer.public().into();
    entry
        .approve_join(ApproveJoinRequest {
            approval: Signed::sign(&body, signer).unwrap(),
        })
        .await
        .unwrap();
    let registration = entry.register(request.clone()).await.unwrap();
    (request, registration)
}

/// Relay the joiner's confirmation, read from the key its capsule gave it, as a driver does.
async fn confirm(
    entry: &LocalMachine,
    request: &ployz_core::RegisterRequest,
    approval: &Signed<ployz_core::authority::JoinApproval>,
    joiner: &std::path::Path,
) {
    let key = crate::sealing_key::SealingKey::load(joiner)
        .unwrap()
        .unwrap();
    entry
        .register(ployz_core::RegisterRequest {
            join_confirmation: Some(key.confirm_join(approval)),
            ..request.clone()
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn an_approved_row_is_published_only_once_the_joiner_confirms_its_capsule() {
    let (entry, raw, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let join_dir = tempfile::tempdir().unwrap();
    let joiner = LocalMachine::new(
        RecordOwner::spawn(LocalMachineStore::open(join_dir.path()).unwrap()).unwrap(),
    );
    let mut request = request("approved", joiner.record().private_key().public_key());
    request.machine_id = joiner.record().id();
    assert!(entry.register(request.clone()).await.is_err());
    let mut body = entry.pending_joins().unwrap().joins.pop().unwrap();
    body.signer = signer.public().into();
    entry
        .approve_join(ApproveJoinRequest {
            approval: Signed::sign(&body, &signer).unwrap(),
        })
        .await
        .unwrap();
    let registration = entry.register(request.clone()).await.unwrap();
    assert!(registration.sealing_key.is_some());
    assert!(
        raw.machine(request.machine_id.as_str())
            .await
            .unwrap()
            .is_none(),
        "Register wraps the key but publishes nothing before the joiner confirms"
    );

    let ployz_core::Enrolled::Enrolled {
        join_confirmation: Some(confirmation),
        ..
    } = Box::pin(joiner.enroll(EnrollRequest {
        role: EnrollAs::Member {
            pin: crate::machine::test_support::pin_of(&registration),
            registration: registration.clone(),
        },
        wireguard_mtu: None,
        pinned: Default::default(),
    }))
    .await
    .unwrap()
    else {
        panic!("an approved join returns its confirmation");
    };
    assert_eq!(
        joiner.record().devices().collect::<Vec<_>>(),
        vec![signer.public().into()]
    );
    assert_eq!(
        crate::sealing_key::SealingKey::load(join_dir.path()).unwrap(),
        crate::sealing_key::SealingKey::load(&directory).unwrap(),
    );

    let mut wrong = confirmation;
    wrong.0[0] ^= 1;
    assert!(matches!(
        entry
            .register(ployz_core::RegisterRequest {
                join_confirmation: Some(wrong),
                ..request.clone()
            })
            .await,
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Mismatch {
            field: MismatchField::JoinConfirmation
        }))
    ));
    assert!(
        raw.machine(request.machine_id.as_str())
            .await
            .unwrap()
            .is_none()
    );

    let confirmed = entry
        .register(ployz_core::RegisterRequest {
            join_confirmation: Some(confirmation),
            ..request.clone()
        })
        .await
        .unwrap();
    assert_eq!(
        ployz_core::Registered {
            restrictions: None,
            ..confirmed.clone()
        },
        registration
    );
    assert!(confirmed.restrictions.is_some());
    assert!(
        entry
            .replicated()
            .unwrap()
            .machines()
            .await
            .unwrap()
            .observations
            .iter()
            .any(|row| row.id == request.machine_id),
        "a confirmed join is published and accepted"
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_joiner_cut_off_from_its_capsule_is_never_published_nor_authorizes_another() {
    let (entry, raw, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let (request, mut registration) = approved_registration(&entry, &joiner, &signer).await;
    let approval = registration.assigned_machine.join_approval.clone().unwrap();
    registration.sealing_key = None;
    registration.devices = vec![DeviceKey::from_bytes([9; 32])];
    let assigned = registration.assigned_machine.clone();
    assert!(matches!(
        joiner.enroll(EnrollRequest {
            role: EnrollAs::Member {
                pin: crate::machine::test_support::pin_of(&registration),
                registration,
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        }),
        Err(super::super::StoreError::JoinRefused(
            AuthorityRefusal::Unsigned { .. }
        ))
    ));
    assert_eq!(joiner.record().phase(), LocalMachinePhase::Uninitialized);
    assert_eq!(joiner.record().devices().count(), 0);
    assert!(
        crate::sealing_key::SealingKey::load(join_dir.path())
            .unwrap()
            .is_none()
    );
    let guessed = crate::sealing_key::SealingKey::generate().confirm_join(&approval);
    assert!(matches!(
        entry
            .register(ployz_core::RegisterRequest {
                join_confirmation: Some(guessed),
                ..request
            })
            .await,
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Mismatch {
            field: MismatchField::JoinConfirmation
        }))
    ));
    assert!(
        raw.machine(assigned.id.as_str()).await.unwrap().is_none(),
        "a joiner cut off from its capsule is never published, so no Machine peers with it"
    );
    let mut attacker = assigned;
    attacker.id = MachineId::random();
    attacker.public_key = WireGuardPublicKey([9; 32]);
    attacker.join_approval = None;
    raw.publish_local_machine(&attacker).await.unwrap();
    assert!(!entry.record().accepts_machine(&attacker));
    assert!(
        entry
            .replicated()
            .unwrap()
            .machine(attacker.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !entry
            .replicated()
            .unwrap()
            .machines()
            .await
            .unwrap()
            .observations
            .iter()
            .any(|row| row.id == attacker.id)
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn forged_capsule_only_establishes_the_attackers_own_cluster() {
    let (entry, raw, founder, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let (request, mut registration) = approved_registration(&entry, &joiner, &signer).await;
    let attacker = Signer::from_seed(&[9; 32]);
    let fake_key = crate::sealing_key::SealingKey::generate();
    let mut body = registration
        .assigned_machine
        .join_approval
        .as_ref()
        .unwrap()
        .open()
        .unwrap();
    body.cluster = fake_key.public_key();
    body.signer = attacker.public().into();
    body.devices = vec![attacker.public().into()];
    body.bootstrap = vec![(&founder).into()];
    let approval = Signed::sign(&body, &attacker).unwrap();
    registration.assigned_machine.join_approval = Some(approval.clone());
    registration.sealing_key = Some(
        fake_key
            .seal_join(
                &body.machine.public_key,
                &approval,
                None,
                Some(&Default::default()),
            )
            .unwrap(),
    );
    let forged_join = |pin| EnrollRequest {
        role: EnrollAs::Member {
            pin,
            registration: registration.clone(),
        },
        wireguard_mtu: None,
        pinned: Default::default(),
    };
    assert!(matches!(
        joiner.enroll(forged_join(Some(pinned(&signer)))),
        Err(StoreError::JoinRefused(AuthorityRefusal::UnknownKey { .. }))
    ));
    assert!(matches!(
        joiner.enroll(forged_join(None)),
        Err(StoreError::JoinUnanchored)
    ));
    // A joiner that pinned the attacker's own Device joins the attacker's Cluster, and the
    // real one still never publishes it.
    joiner.enroll(forged_join(Some(pinned(&attacker)))).unwrap();
    let forged = joiner.record().machine().unwrap();
    assert!(matches!(
        entry
            .register(ployz_core::RegisterRequest {
                join_confirmation: Some(fake_key.confirm_join(&approval)),
                ..request
            })
            .await,
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Mismatch {
            field: MismatchField::JoinConfirmation
        }))
    ));
    raw.publish_local_machine(forged).await.unwrap();
    assert!(joiner.record().filters_peers());
    assert!(!entry.record().accepts_machine(forged));
    assert!(
        entry
            .replicated()
            .unwrap()
            .machine(forged.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn unsigned_visible_peers_never_seed_an_approved_join_or_its_membership() {
    let (entry, raw, founder, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let (request, mut registration) = approved_registration(&entry, &joiner, &signer).await;
    let approval = registration.assigned_machine.join_approval.clone().unwrap();
    let mut attacker = founder.clone();
    attacker.id = MachineId::random();
    attacker.public_key = WireGuardPublicKey([9; 32]);
    registration.visible_peers.push(attacker.clone());
    registration.devices = vec![DeviceKey::from_bytes([9; 32])];
    joiner
        .enroll(EnrollRequest {
            role: EnrollAs::Member {
                pin: Some(pinned(&signer)),
                registration,
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    confirm(&entry, &request, &approval, join_dir.path()).await;
    assert_eq!(
        joiner
            .record()
            .bootstrap()
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        vec![founder.id]
    );
    assert!(joiner.record().accepts_machine(&founder));
    assert!(!joiner.record().accepts_machine(&attacker));
    raw.publish_local_machine(&attacker).await.unwrap();
    assert!(!entry.record().accepts_machine(&attacker));
    assert_eq!(
        entry
            .replicated()
            .unwrap()
            .machines()
            .await
            .unwrap()
            .observations
            .len(),
        2
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn lost_register_reply_is_recovered_after_restart_with_the_same_capsule() {
    let (entry, raw, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let join_dir = tempfile::tempdir().unwrap();
    let joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let (mut request, first) = approved_registration(&entry, &joiner, &signer).await;
    drop(entry);
    let owner = RecordOwner::spawn(LocalMachineStore::open(&directory).unwrap()).unwrap();
    let entry = LocalMachine::new(owner)
        .with_cluster(Some((raw, crate::corrosion::AdminClient::new("/no/admin"))));
    assert_eq!(entry.register(request.clone()).await.unwrap(), first);
    request.public_key = WireGuardPublicKey([8; 32]);
    assert!(matches!(
        entry.register(request).await,
        Err(LocalMachineError::JoinConsumed(_))
    ));
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn enrollment_replay_does_not_revalidate_an_expired_approval_or_change_state() {
    let (entry, _, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let mut request = request("joiner", joiner.record().private_key().public_key());
    request.machine_id = joiner.record().id();
    assert!(entry.register(request.clone()).await.is_err());
    let mut body = entry.pending_joins().unwrap().joins.pop().unwrap();
    body.signer = signer.public().into();
    body.expires_at = Timestamp::now().plus(500);
    request.join_approval = Some(Signed::sign(&body, &signer).unwrap());
    let registration = entry.register(request.clone()).await.unwrap();
    let enrollment = EnrollRequest {
        role: EnrollAs::Member {
            pin: crate::machine::test_support::pin_of(&registration),
            registration,
        },
        wireguard_mtu: None,
        pinned: Default::default(),
    };
    joiner.enroll(enrollment.clone()).unwrap();
    let before = std::fs::read(join_dir.path().join("machine.json")).unwrap();
    // real-wait: approval expiry reads the wall clock, which paused time does not move.
    tokio::time::sleep(std::time::Duration::from_millis(510)).await;
    assert!(entry.register(request).await.is_ok());
    assert!(matches!(
        joiner.enroll(enrollment),
        Ok(EnrollPlan::Replay(_))
    ));
    assert_eq!(
        std::fs::read(join_dir.path().join("machine.json")).unwrap(),
        before
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn enforcing_register_refuses_an_overlapping_allocation_before_proposing() {
    let (entry, _, founder, directory, server) = participating().await;
    enforce(&entry, &Signer::from_seed(&[1; 32])).await;
    let mut request = request("overlap", WireGuardPublicKey([2; 32]));
    request.assigned_subnet = Some(founder.subnet);
    assert!(matches!(
        entry.register(request).await,
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Mismatch {
            field: MismatchField::UnusedSubnet
        }))
    ));
    assert!(entry.pending_joins().unwrap().joins.is_empty());
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

/// A mesh caller is a Member's own Machine or anything holding its WireGuard key, and the
/// local socket stays the console's, so a daemon that says `enforced.v1` never gives it Full
/// scope, before activation or after.
#[tokio::test]
async fn a_mesh_caller_is_never_full_on_an_enforced_v1_daemon() {
    use ployz_core::{MachineRpcClient, op};
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::{Request, transport::Server};
    let (directory, owner, _) = open_store("ployzd-mesh-off");
    let api = crate::machine_api::MachineApi::builder(owner.clone()).build();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server =
        tokio::spawn(Server::builder().serve_with_incoming(api, TcpListenerStream::new(listener)));
    let mut client = MachineRpcClient::connect(format!("http://{address}"))
        .await
        .unwrap();
    let key = DeviceKey::from_bytes([9; 32]);
    let refused = client
        .set_device(Request::new(
            op::SetDevice::into_request(SetDeviceRequest::Approve { key })
                .encode()
                .unwrap(),
        ))
        .await
        .unwrap_err();
    assert_eq!(refused.code(), tonic::Code::PermissionDenied);
    assert!(!owner.record().devices().any(|device| device == key));
    server.abort();
    drop(client);
    drop(owner);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn pending_join_flood_is_bounded_and_keeps_existing_proposals() {
    let (entry, _, _, directory, server) = participating().await;
    enforce(&entry, &Signer::from_seed(&[1; 32])).await;
    for _ in 0..130 {
        assert!(
            entry
                .register(request("pending", WireGuardPublicKey([2; 32])))
                .await
                .is_err()
        );
    }
    assert_eq!(entry.pending_joins().unwrap().joins.len(), 128);
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn removing_the_signing_device_keeps_an_admitted_machine_peered() {
    let (entry, _, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let (request, registration) = approved_registration(&entry, &joiner, &signer).await;
    let approval = registration.assigned_machine.join_approval.clone().unwrap();
    joiner
        .enroll(EnrollRequest {
            role: EnrollAs::Member {
                pin: crate::machine::test_support::pin_of(&registration),
                registration,
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    confirm(&entry, &request, &approval, join_dir.path()).await;
    let members = || async {
        entry
            .replicated()
            .unwrap()
            .machines()
            .await
            .unwrap()
            .observations
            .iter()
            .any(|row| row.id == request.machine_id)
    };
    assert!(members().await);
    entry
        .set_device(SetDeviceRequest::Approve {
            key: DeviceKey::from_bytes([7; 32]),
        })
        .await
        .unwrap();
    entry
        .set_device(SetDeviceRequest::Remove {
            code: DeviceKey::from(signer.public()).code(),
        })
        .await
        .unwrap();
    assert!(
        !entry
            .record()
            .devices()
            .any(|key| key == signer.public().into())
    );
    assert!(members().await);
    // A resend after the row is out publishes nothing new, so it still answers.
    confirm(&entry, &request, &approval, join_dir.path()).await;
    assert!(members().await);
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn share_refuses_an_approval_for_another_allocation_of_the_recipient() {
    let (entry, raw, founder, directory, server) = participating().await;
    let existing = joined_before_activation(&raw, &founder).await;
    let member = (&existing).into();
    entry
        .owner()
        .mutate(move |store| store.trust_members(vec![member]).unwrap())
        .await
        .unwrap();
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let mut other = request("other", WireGuardPublicKey([5; 32]));
    other.assigned_subnet = Some("10.210.5.0/24".parse().unwrap());
    assert!(entry.register(other).await.is_err());
    let mut body = entry.pending_joins().unwrap().joins.pop().unwrap();
    body.signer = signer.public().into();
    body.sealing_recipient = existing.public_key;
    body.machine = (&existing).into();
    body.machine.subnet = "10.210.99.0/24".parse().unwrap();
    let share = |body: &ployz_core::authority::JoinApproval| ShareSealingKeyRequest {
        join_approval: Some(Signed::sign(body, &signer).unwrap()),
        machine: existing.id,
        public_key: existing.public_key,
    };
    assert!(matches!(
        entry.share_sealing_key(share(&body)).await,
        Err(LocalMachineError::JoinRefused(
            AuthorityRefusal::Mismatch { .. }
        ))
    ));
    body.machine = (&existing).into();
    assert!(entry.share_sealing_key(share(&body)).await.is_ok());
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_shared_key_is_replayed_for_its_own_approval_and_consumed_for_any_other_entry() {
    let (entry, raw, founder, directory, server) = participating().await;
    let existing = joined_before_activation(&raw, &founder).await;
    let member = (&existing).into();
    entry
        .owner()
        .mutate(move |store| store.trust_members(vec![member]).unwrap())
        .await
        .unwrap();
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let mut other = request("other", WireGuardPublicKey([5; 32]));
    other.assigned_subnet = Some("10.210.5.0/24".parse().unwrap());
    assert!(entry.register(other).await.is_err());
    let mut body = entry.pending_joins().unwrap().joins.pop().unwrap();
    body.signer = signer.public().into();
    body.sealing_recipient = existing.public_key;
    body.machine = (&existing).into();
    let share = |body: &ployz_core::authority::JoinApproval| ShareSealingKeyRequest {
        join_approval: Some(Signed::sign(body, &signer).unwrap()),
        machine: existing.id,
        public_key: existing.public_key,
    };

    let shared = entry.share_sealing_key(share(&body)).await.unwrap();

    let replayed = entry.share_sealing_key(share(&body)).await.unwrap();
    assert_eq!(
        replayed.sealed, shared.sealed,
        "the same approval is answered again"
    );
    let mut longer = body.clone();
    longer.expires_at = longer.expires_at.plus(1);
    assert!(matches!(
        entry.share_sealing_key(share(&longer)).await,
        Err(LocalMachineError::JoinConsumed(id)) if id == existing.id
    ));
    let mut register = request("again", existing.public_key);
    register.machine_id = existing.id;
    register.assigned_subnet = Some(existing.subnet);
    assert!(matches!(
        entry.register(register).await,
        Err(LocalMachineError::JoinConsumed(id)) if id == existing.id
    ));
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn resending_the_original_enroll_while_resetting_conflicts() {
    let (entry, _, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let (_, registration) = approved_registration(&entry, &joiner, &signer).await;
    let enrollment = EnrollRequest {
        role: EnrollAs::Member {
            pin: crate::machine::test_support::pin_of(&registration),
            registration,
        },
        wireguard_mtu: None,
        pinned: Default::default(),
    };
    joiner.enroll(enrollment.clone()).unwrap();
    joiner.begin_reset().unwrap();
    assert!(matches!(
        joiner.enroll(enrollment),
        Ok(EnrollPlan::Conflict(_))
    ));
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_saved_bootstrap_peer_it_does_not_trust_is_not_offered_after_restart() {
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let cluster = crate::machine::test_support::ApprovingCluster::new(1);
    joiner.enroll(cluster.join(joiner.record())).unwrap();
    drop(joiner);
    let path = join_dir.path().join("machine.json");
    let mut row: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let unapproved = ployz_core::Machine {
        id: MachineId::random(),
        public_key: WireGuardPublicKey([9; 32]),
        subnet: "10.210.77.0/24".parse().unwrap(),
        ..cluster.admitting.clone()
    };
    row.pointer_mut("/body/bootstrap")
        .and_then(serde_json::Value::as_array_mut)
        .unwrap()
        .push(serde_json::to_value(&unapproved).unwrap());
    std::fs::write(&path, serde_json::to_vec(&row).unwrap()).unwrap();

    let joiner = LocalMachineStore::open(join_dir.path()).unwrap();

    assert_eq!(
        joiner
            .record()
            .bootstrap()
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        vec![cluster.admitting.id]
    );
}

/// The confirmation a joiner relays for its own approval.
fn confirmation(
    joiner: &std::path::Path,
    request: &ployz_core::RegisterRequest,
    approval: &Signed<ployz_core::authority::JoinApproval>,
) -> ployz_core::RegisterRequest {
    let key = crate::sealing_key::SealingKey::load(joiner)
        .unwrap()
        .unwrap();
    ployz_core::RegisterRequest {
        join_confirmation: Some(key.confirm_join(approval)),
        ..request.clone()
    }
}

fn member(registration: ployz_core::Registered) -> EnrollRequest {
    EnrollRequest {
        role: EnrollAs::Member {
            pin: crate::machine::test_support::pin_of(&registration),
            registration,
        },
        wireguard_mtu: None,
        pinned: Default::default(),
    }
}

/// Another Member forgets a joined Server while a resend of its confirmed Register waits
/// to publish here. The forget wins: the resend is refused, the row stays deleted, and this
/// Machine records the Server removed.
#[tokio::test]
async fn a_resent_register_is_refused_when_another_members_forget_lands_mid_call() {
    let (entry, raw, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let (request, registration) = approved_registration(&entry, &joiner, &signer).await;
    let approval = registration.assigned_machine.join_approval.clone().unwrap();
    joiner.enroll(member(registration)).unwrap();
    confirm(&entry, &request, &approval, join_dir.path()).await;
    let gone = request.machine_id;
    assert!(raw.machine(gone.as_str()).await.unwrap().is_some());

    let guard = raw.machine_publication().await;
    let registering = tokio::spawn({
        let entry = entry.clone();
        let resent = confirmation(join_dir.path(), &request, &approval);
        async move { entry.register(resent).await }
    });
    // real-wait: the resend passes admission and reaches the publication guard.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    raw.receive_forget(&gone).await.unwrap();
    drop(guard);

    assert!(matches!(
        registering.await.unwrap(),
        Err(LocalMachineError::MemberRemoved(id)) if id == gone
    ));
    assert!(raw.machine(gone.as_str()).await.unwrap().is_none());
    assert!(entry.record().removes_member(gone, None));
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

/// A Register naming a Server whose row this store shows deleted is refused. The removal is
/// recorded under what this Machine verified for that Server, never the WireGuard key the
/// request names, so a live Server holding that key is not shut out.
#[tokio::test]
async fn a_removal_a_register_finds_never_takes_the_requests_wireguard_key() {
    let (entry, raw, founder, directory, server) = participating().await;
    enforce(&entry, &Signer::from_seed(&[1; 32])).await;
    let live = WireGuardPublicKey([6; 32]);
    let request = request("gone", live);
    raw.publish_local_machine(&ployz_core::Machine {
        id: request.machine_id,
        public_key: WireGuardPublicKey([5; 32]),
        ..founder
    })
    .await
    .unwrap();
    raw.receive_forget(&request.machine_id).await.unwrap();

    assert!(matches!(
        entry.register(request.clone()).await,
        Err(LocalMachineError::MemberRemoved(id)) if id == request.machine_id
    ));
    assert!(entry.record().removes_member(request.machine_id, None));
    assert!(
        !entry
            .record()
            .removes_member(MachineId::random(), Some(live)),
        "a live Server with the request's WireGuard key stays admitted"
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

/// A joiner is published only while its approval still holds here. Once the Device that
/// signed it is removed, its first confirmed Register publishes nothing.
#[tokio::test]
async fn a_confirmed_register_publishes_nothing_once_its_signer_is_removed() {
    let (entry, raw, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let (request, registration) = approved_registration(&entry, &joiner, &signer).await;
    let approval = registration.assigned_machine.join_approval.clone().unwrap();
    joiner.enroll(member(registration)).unwrap();
    entry
        .set_device(SetDeviceRequest::Approve {
            key: DeviceKey::from_bytes([7; 32]),
        })
        .await
        .unwrap();
    entry
        .set_device(SetDeviceRequest::Remove {
            code: DeviceKey::from(signer.public()).code(),
        })
        .await
        .unwrap();

    assert!(matches!(
        entry
            .register(confirmation(join_dir.path(), &request, &approval))
            .await,
        Err(LocalMachineError::JoinRefused(_))
    ));
    assert!(
        raw.machine(request.machine_id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

/// A joiner checks its approval on the clock it keeps, which never runs back, so a clock
/// turned back can't reopen an approval it already saw expire.
#[tokio::test]
async fn a_joiner_refuses_an_approval_its_kept_clock_has_passed() {
    let (entry, _, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let join_dir = tempfile::tempdir().unwrap();
    let store = LocalMachineStore::open(join_dir.path()).unwrap();
    let (_, registration) = approved_registration(&entry, &store, &signer).await;
    let later = Timestamp::now().plus(24 * 60 * 60 * 1000);
    std::fs::write(
        join_dir.path().join("restart-approvals.json"),
        serde_json::to_vec(&serde_json::json!({ "expiry_time_floor": later })).unwrap(),
    )
    .unwrap();
    let joiner = LocalMachine::new(RecordOwner::spawn(store).unwrap());

    assert!(matches!(
        Box::pin(joiner.enroll(member(registration))).await,
        Err(LocalMachineError::Store(StoreError::JoinRefused(
            AuthorityRefusal::Expired
        )))
    ));
    assert_eq!(joiner.record().phase(), LocalMachinePhase::Uninitialized);
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

/// An approval open when the enrollment was planned but closed by the time it commits
/// installs nothing.
#[tokio::test]
async fn an_approval_that_closes_before_the_install_commits_installs_nothing() {
    let (entry, _, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let (_, registration) = approved_registration(&entry, &joiner, &signer).await;
    let now = Timestamp::now();
    let EnrollPlan::Fresh(installation) = joiner
        .plan_enrollment(member(registration), None, None, now)
        .unwrap()
    else {
        panic!("an approved registration enrolls fresh");
    };

    let closed = now.plus(24 * 60 * 60 * 1000);
    assert!(matches!(
        joiner.install(installation, closed),
        Err(StoreError::JoinRefused(AuthorityRefusal::Expired))
    ));
    assert_eq!(joiner.record().phase(), LocalMachinePhase::Uninitialized);
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}
