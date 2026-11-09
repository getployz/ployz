use super::*;
use ployz_core::{
    ApproveJoinRequest, DeviceKey, EnrollAs, EnrollRequest, SetDeviceEnforcementRequest,
    SetDeviceRequest, ShareSealingKeyRequest,
    authority::{AuthorityRefusal, Member, Signed, Signer, StartingSet, Timestamp},
};

async fn enforce(entry: &LocalMachine, signer: &Signer) {
    entry
        .set_device(SetDeviceRequest::Approve {
            key: signer.public().into(),
        })
        .await
        .unwrap();
    let members = entry
        .replicated()
        .unwrap()
        .machines()
        .await
        .unwrap()
        .observations
        .iter()
        .map(Into::into)
        .collect();
    entry
        .set_device_enforcement(enforce_request(signer, members))
        .await
        .unwrap();
}

fn enforce_request(signer: &Signer, members: Vec<Member>) -> SetDeviceEnforcementRequest {
    let issued_at = Timestamp::now();
    let set = StartingSet {
        operation: ployz_core::OperationId::random(),
        members,
        issued_at,
        expires_at: issued_at.plus(60_000),
        signer: signer.public().into(),
    };
    SetDeviceEnforcementRequest {
        enforced: true,
        starting_set: Some(Signed::sign(&set, signer).unwrap()),
    }
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
                role: EnrollAs::Member { registration },
                wireguard_mtu: None,
                pinned: Default::default()
            })
            .unwrap(),
        EnrollPlan::Fresh
    );
    assert_eq!(
        joiner.record().devices().collect::<Vec<_>>(),
        vec![signer.public().into()]
    );
    assert!(joiner.record().device_enforcement());
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
            field: "an admitting Machine"
        }))
    ));
    body.admitting_machines = vec![entry.record().id()];
    body.devices.push(DeviceKey::from_bytes([9; 32]));
    let approval = Signed::sign(&body, &signer).unwrap();
    assert!(matches!(
        entry.approve_join(ApproveJoinRequest { approval }).await,
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Mismatch {
            field: "approved Devices"
        }))
    ));
    body.devices.pop();
    body.admitting_machines = vec![entry.record().id()];
    body.sealing_recipient = WireGuardPublicKey([3; 32]);
    let approval = Signed::sign(&body, &signer).unwrap();
    assert!(matches!(
        entry.approve_join(ApproveJoinRequest { approval }).await,
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Mismatch {
            field: "a sealing recipient"
        }))
    ));
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn unsigned_enroll_and_share_are_refused_under_enforcement() {
    let (entry, _, _, directory, server) = participating().await;
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let mut request = request("joiner", joiner.record().private_key().public_key());
    request.machine_id = joiner.record().id();
    let registration = entry.register(request.clone()).await.unwrap();
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    joiner.approve_device(signer.public().into()).unwrap();
    joiner.set_device_enforcement(true, Vec::new()).unwrap();
    assert!(matches!(
        joiner.enroll(EnrollRequest {
            role: EnrollAs::Member { registration },
            wireguard_mtu: None,
            pinned: Default::default()
        }),
        Err(super::super::StoreError::JoinRefused(
            AuthorityRefusal::Unsigned { .. }
        ))
    ));
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
            .set_device_enforcement(Request::new(
                op::SetDeviceEnforcement::into_request(SetDeviceEnforcementRequest {
                    enforced: false,
                    starting_set: None,
                })
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
    entry
        .approve_join(ApproveJoinRequest {
            approval: Signed::sign(&body, &signer).unwrap(),
        })
        .await
        .unwrap();
    assert!(matches!(
        entry.register(request.clone()).await,
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Expired))
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
            field: "a join confirmation"
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
    assert_eq!(confirmed, registration);
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
    joiner
        .enroll(EnrollRequest {
            role: EnrollAs::Member { registration },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    assert!(!joiner.record().device_enforcement());
    let stripped = joiner.record().machine().unwrap().id;
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
            field: "a join confirmation"
        }))
    ));
    assert!(
        raw.machine(stripped.as_str()).await.unwrap().is_none(),
        "a joiner cut off from its capsule is never published, so no Machine peers with it"
    );
    assert_eq!(
        joiner.record().devices().collect::<Vec<_>>(),
        vec![DeviceKey::from_bytes([9; 32])]
    );
    let mut attacker = joiner.record().machine().unwrap().clone();
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
            .seal_join(&body.machine.public_key, &approval)
            .unwrap(),
    );
    joiner
        .enroll(EnrollRequest {
            role: EnrollAs::Member { registration },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    let forged = joiner.record().machine().unwrap();
    assert!(matches!(
        entry
            .register(ployz_core::RegisterRequest {
                join_confirmation: Some(fake_key.confirm_join(&approval)),
                ..request
            })
            .await,
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Mismatch {
            field: "a join confirmation"
        }))
    ));
    raw.publish_local_machine(forged).await.unwrap();
    assert!(joiner.record().device_enforcement());
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
            role: EnrollAs::Member { registration },
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
        role: EnrollAs::Member { registration },
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
            field: "an unused subnet"
        }))
    ));
    assert!(entry.pending_joins().unwrap().joins.is_empty());
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn mesh_keeps_full_scope_when_enforcement_is_off() {
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
    client
        .set_device(Request::new(
            op::SetDevice::into_request(SetDeviceRequest::Approve { key })
                .encode()
                .unwrap(),
        ))
        .await
        .unwrap();
    assert!(owner.record().devices().any(|device| device == key));
    server.abort();
    drop(client);
    drop(owner);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn enforcement_pins_existing_id_key_and_subnet_durably() {
    let (entry, raw, mut founder, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    let mut existing = founder.clone();
    existing.id = MachineId::random();
    existing.subnet = "10.210.2.0/24".parse().unwrap();
    existing.public_key = WireGuardPublicKey([2; 32]);
    raw.publish_local_machine(&existing).await.unwrap();
    enforce(&entry, &signer).await;
    assert!(entry.record().accepts_machine(&existing));
    drop(entry);
    let store = LocalMachineStore::open(&directory).unwrap();
    assert!(store.record().accepts_machine(&existing));
    existing.public_key = WireGuardPublicKey([3; 32]);
    assert!(!store.record().accepts_machine(&existing));
    founder.public_key = WireGuardPublicKey([4; 32]);
    assert!(!store.record().accepts_machine(&founder));
    server.abort();
    drop(store);
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
            role: EnrollAs::Member { registration },
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
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn share_refuses_an_approval_for_another_allocation_of_the_recipient() {
    let (entry, _, _, directory, server) = participating().await;
    let existing = entry
        .register(request("existing", WireGuardPublicKey([4; 32])))
        .await
        .unwrap()
        .assigned_machine;
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
async fn resending_the_original_enroll_while_resetting_conflicts() {
    let (entry, _, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let (_, registration) = approved_registration(&entry, &joiner, &signer).await;
    let enrollment = EnrollRequest {
        role: EnrollAs::Member { registration },
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
async fn enforcement_trusts_only_the_signed_starting_set_not_rows_registered_before_it() {
    let (entry, raw, founder, directory, server) = participating().await;
    let late = entry
        .register(request("late", WireGuardPublicKey([4; 32])))
        .await
        .unwrap()
        .assigned_machine;
    let signer = Signer::from_seed(&[1; 32]);
    entry
        .set_device(SetDeviceRequest::Approve {
            key: signer.public().into(),
        })
        .await
        .unwrap();
    entry
        .set_device_enforcement(enforce_request(&signer, vec![(&founder).into()]))
        .await
        .unwrap();
    assert!(!entry.record().accepts_machine(&late));
    assert!(
        !raw.machines()
            .await
            .unwrap()
            .observations
            .iter()
            .all(|row| entry.record().accepts_machine(row))
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
            .any(|row| row.id == late.id)
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn enforcement_refuses_a_missing_foreign_misscoped_or_reused_starting_set() {
    let (entry, _, founder, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    entry
        .set_device(SetDeviceRequest::Approve {
            key: signer.public().into(),
        })
        .await
        .unwrap();
    let refused = |result| matches!(result, Err(LocalMachineError::EnforcementRefused(_)));
    let unsigned = SetDeviceEnforcementRequest {
        enforced: true,
        starting_set: None,
    };
    assert!(refused(entry.set_device_enforcement(unsigned).await));
    let foreign = enforce_request(&Signer::from_seed(&[2; 32]), vec![(&founder).into()]);
    assert!(refused(entry.set_device_enforcement(foreign).await));
    let mut other = founder.clone();
    other.id = MachineId::random();
    let misscoped = enforce_request(&signer, vec![(&other).into()]);
    assert!(refused(entry.set_device_enforcement(misscoped).await));
    assert!(!entry.record().device_enforcement());
    let approved = enforce_request(&signer, vec![(&founder).into()]);
    entry
        .set_device_enforcement(approved.clone())
        .await
        .unwrap();
    entry
        .set_device_enforcement(SetDeviceEnforcementRequest {
            enforced: false,
            starting_set: None,
        })
        .await
        .unwrap();
    assert!(refused(entry.set_device_enforcement(approved).await));
    assert!(!entry.record().device_enforcement());
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_saved_bootstrap_peer_outside_the_starting_set_is_not_offered_after_restart() {
    let (entry, _, founder, directory, server) = participating().await;
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let mut joining = request("joiner", joiner.record().private_key().public_key());
    joining.machine_id = joiner.record().id();
    let mut registration = entry.register(joining).await.unwrap();
    let mut unapproved = founder.clone();
    unapproved.id = MachineId::random();
    unapproved.public_key = WireGuardPublicKey([9; 32]);
    unapproved.subnet = "10.210.77.0/24".parse().unwrap();
    registration.visible_peers.push(unapproved.clone());
    joiner
        .enroll(EnrollRequest {
            role: EnrollAs::Member { registration },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    joiner.complete_catch_up().unwrap();
    let own = joiner.record().machine().unwrap().into();
    joiner
        .set_device_enforcement(true, vec![(&founder).into(), own])
        .unwrap();
    drop(joiner);
    let joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    assert_eq!(
        joiner
            .record()
            .bootstrap()
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        vec![founder.id]
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}
