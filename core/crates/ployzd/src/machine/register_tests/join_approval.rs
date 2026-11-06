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
    assert!(matches!(
        entry.register(request.clone()).await,
        Err(LocalMachineError::JoinConsumed(_))
    ));
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
        reopened.consume_join(request.machine_id),
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
    joiner.set_device_enforcement(true).unwrap();
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
async fn mesh_tcp_never_grants_full_scope_or_slot_administration() {
    use ployz_core::{MachineRpcClient, SetManagementClientRequest, op};
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::{Request, transport::Server};
    let (directory, owner, _) = open_store("ployzd-mesh-authority");
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
    std::fs::remove_dir_all(directory).unwrap();
}
