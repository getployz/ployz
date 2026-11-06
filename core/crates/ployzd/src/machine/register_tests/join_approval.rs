use super::*;
use ployz_core::{
    ApproveJoinRequest, DeviceKey, EnrollAs, EnrollRequest, SetDeviceEnforcementRequest,
    SetDeviceRequest, ShareSealingKeyRequest,
    authority::{AuthorityRefusal, Signed, Signer},
};

async fn enforce(entry: &LocalMachine, signer: &Signer) {
    entry
        .set_device(SetDeviceRequest::Approve {
            key: signer.public().into(),
        })
        .await
        .unwrap();
    entry
        .set_device_enforcement(SetDeviceEnforcementRequest { enforced: true })
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
    assert!(matches!(entry.register(request.clone()).await, Ok(_)));
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
    joiner.set_device_enforcement(true, &[]).unwrap();
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
                    enforced: false
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
    use ployz_core::authority::Timestamp;
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
    use ployz_core::authority::Timestamp;
    let (entry, replicated, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    std::fs::write(
        directory.join("restart-approvals.json"),
        serde_json::to_vec(&serde_json::json!({
            "expiry_time_floor": Timestamp::now().plus(18 * 60 * 1000),
        }))
        .unwrap(),
    )
    .unwrap();
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

#[tokio::test]
async fn stripped_capsule_cannot_authorize_a_second_machine_on_an_honest_member() {
    let (entry, raw, _, directory, server) = participating().await;
    let signer = Signer::from_seed(&[1; 32]);
    enforce(&entry, &signer).await;
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let (_, mut registration) = approved_registration(&entry, &joiner, &signer).await;
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
    assert!(
        entry
            .record()
            .accepts_machine(joiner.record().machine().unwrap())
    );
    assert!(
        crate::sealing_key::SealingKey::load(join_dir.path())
            .unwrap()
            .is_none()
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
    let (_, mut registration) = approved_registration(&entry, &joiner, &signer).await;
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
    let (_, mut registration) = approved_registration(&entry, &joiner, &signer).await;
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
    use ployz_core::authority::Timestamp;
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
