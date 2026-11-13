use ployz_core::{
    AdvertisedEndpoint, Machine, MachineId, MachineName, MachineRpc, MachineRuntime,
    RegisterRequest, Registered, RpcError, RpcResponseBody, WireGuardPublicKey, op,
};
use tonic::Request;

use super::super::{LocalMachine, LocalMachineError, LocalMachineStore, RecordOwner};
use crate::{
    corrosion::{AdminClient, ReplicatedStore, fake_cluster},
    machine_api::MachineService,
};

pub(super) async fn participating() -> (
    LocalMachine,
    ReplicatedStore,
    Machine,
    std::path::PathBuf,
    tokio::task::JoinHandle<()>,
) {
    let (data_dir, store, founder) = open_store("ployzd-register");
    over_cluster(data_dir, store, founder).await
}

/// [`participating`], on a record a daemon from before enforcement was the only mode
/// wrote, so it awaits its Cluster's activation.
pub(super) async fn participating_awaiting() -> (
    LocalMachine,
    ReplicatedStore,
    Machine,
    std::path::PathBuf,
    tokio::task::JoinHandle<()>,
) {
    let (data_dir, store, founder) = founded_store("ployzd-register-awaiting");
    drop(store);
    crate::machine::test_support::written_before_f7(&data_dir, false);
    let store = RecordOwner::spawn(LocalMachineStore::open(&data_dir).unwrap()).unwrap();
    over_cluster(data_dir, store, founder).await
}

async fn over_cluster(
    data_dir: std::path::PathBuf,
    store: RecordOwner,
    founder: Machine,
) -> (
    LocalMachine,
    ReplicatedStore,
    Machine,
    std::path::PathBuf,
    tokio::task::JoinHandle<()>,
) {
    let (replicated, server) = fake_cluster::store().await;
    replicated.publish_local_machine(&founder).await.unwrap();
    let local = LocalMachine::new(store).with_cluster(Some((
        replicated.clone(),
        AdminClient::new("/no/such/ployz-admin.sock"),
    )));
    (local, replicated, founder, data_dir, server)
}

pub(super) fn open_store(prefix: &str) -> (std::path::PathBuf, RecordOwner, Machine) {
    let (data_dir, store, founder) = founded_store(prefix);
    (data_dir, RecordOwner::spawn(store).unwrap(), founder)
}

/// A founder's store, not yet owned by a running record owner.
pub(super) fn founded_store(prefix: &str) -> (std::path::PathBuf, LocalMachineStore, Machine) {
    let data_dir = std::env::temp_dir().join(format!("{prefix}-{}", MachineId::random()));
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    store
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Founder {
                initial_policy: Default::default(),
                name: MachineName::parse("edge").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
                grant: None,
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    let founder = store.record().machine().unwrap().clone();
    (data_dir, store, founder)
}

pub(super) fn machine_service(
    store: RecordOwner,
    replicated: ReplicatedStore,
    port: Option<u16>,
) -> MachineService {
    let service = MachineService::with_cluster(
        store,
        Some((replicated, AdminClient::new("/no/such/ployz-admin.sock"))),
    );
    match port {
        Some(port) => service.with_machine_api_port(port),
        None => service,
    }
}

pub(super) fn request(name: &str, public_key: WireGuardPublicKey) -> RegisterRequest {
    RegisterRequest {
        join_approval: None,
        join_confirmation: None,
        machine_id: MachineId::random(),
        assigned_subnet: Some("10.210.1.0/24".parse().unwrap()),
        initial_policy: Default::default(),
        name: MachineName::parse(name).unwrap(),
        storage: ployz_core::StorageChoice::None,
        public_key,
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.9:51820".parse().unwrap())],
        runtime: MachineRuntime::default(),
        capabilities: Vec::new(),
    }
}

pub(super) async fn rpc_register(
    service: &MachineService,
    body: RegisterRequest,
) -> Result<Registered, RpcError> {
    let request = Request::new(op::Register::into_request(body).encode().unwrap());
    let response = service
        .register(request)
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap();
    if let RpcResponseBody::Error(error) = &response.body {
        return Err(error.clone());
    }
    Ok(response.decode::<op::Register>().unwrap())
}

pub(super) async fn rpc_list_machines(
    service: &MachineService,
) -> Result<ployz_core::MachineList, RpcError> {
    let request = Request::new(
        op::ListMachines::into_request(ployz_core::ListMachinesRequest {})
            .encode()
            .unwrap(),
    );
    let response = service
        .list_machines(request)
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap();
    if let RpcResponseBody::Error(error) = &response.body {
        return Err(error.clone());
    }
    Ok(response.decode::<op::ListMachines>().unwrap())
}

/// The Device an entry trusts to sign the joins it proposes.
pub(super) fn join_signer() -> ployz_core::authority::Signer {
    ployz_core::authority::Signer::from_seed(&[1; 32])
}

/// Approve `signer` at `entry`'s console, unless it already trusts it.
async fn trust_device(entry: &LocalMachine, signer: &ployz_core::authority::Signer) {
    let key = signer.public().into();
    if entry.record().devices().any(|device| device == key) {
        return;
    }
    entry
        .clone()
        .on_local_socket()
        .set_device(ployz_core::SetDeviceRequest::Approve { key })
        .await
        .unwrap();
}

/// Register `request` at `entry` as a person's driver does: ask, sign the join the entry
/// proposes with [`join_signer`], which the entry then trusts, and ask again under that
/// approval. A request the entry already answered is answered from what it kept.
pub(super) async fn register_approved(
    entry: &LocalMachine,
    request: &RegisterRequest,
) -> Result<Registered, LocalMachineError> {
    use ployz_core::authority::{AuthorityRefusal, Signed};
    let signer = join_signer();
    trust_device(entry, &signer).await;
    match entry.register(request.clone()).await {
        Err(LocalMachineError::JoinRefused(AuthorityRefusal::Unsigned { .. })) => {}
        answered => return answered,
    }
    let mut body = entry
        .pending_joins()?
        .joins
        .into_iter()
        .find(|join| join.machine.id == request.machine_id)
        .expect("the entry proposed this join");
    body.signer = signer.public().into();
    body.devices = entry.record().devices().collect();
    entry
        .register(RegisterRequest {
            join_approval: Some(Signed::sign(&body, &signer).unwrap()),
            ..request.clone()
        })
        .await
}

/// [`register_approved`], then the joiner's confirmation that it opened its capsule, made
/// from the entry's own Sealing Key, so the entry publishes the row.
pub(super) async fn register_published(
    entry: &LocalMachine,
    request: &RegisterRequest,
) -> Result<Registered, LocalMachineError> {
    let registered = register_approved(entry, request).await?;
    let approval = registered
        .assigned_machine
        .join_approval
        .clone()
        .expect("an approved registration carries its approval");
    let key = crate::sealing_key::SealingKey::load(entry.owner().data_dir())
        .unwrap()
        .expect("the entry holds the Sealing Key");
    entry
        .register(RegisterRequest {
            join_confirmation: Some(key.confirm_join(&approval)),
            join_approval: Some(approval),
            ..request.clone()
        })
        .await
}

/// A join approval of `recipient`'s published row, admitted by `admitting` and signed by
/// [`join_signer`], which `admitting` then trusts: what a person signs to share the Sealing
/// Key with a Member that lacks it.
pub(super) async fn approve_recipient(
    admitting: &LocalMachine,
    recipient: &Machine,
) -> ployz_core::authority::Signed<ployz_core::authority::JoinApproval> {
    use ployz_core::authority::{JoinApproval, Signed, Timestamp};
    let signer = join_signer();
    trust_device(admitting, &signer).await;
    let cluster = crate::sealing_key::SealingKey::load(admitting.owner().data_dir())
        .unwrap()
        .expect("the admitting Machine holds the Sealing Key")
        .public_key();
    let network = admitting
        .replicated()
        .unwrap()
        .cluster_network()
        .await
        .unwrap()
        .expect("the Cluster network is published");
    let issued_at = Timestamp::now();
    let body = JoinApproval {
        cluster,
        machine: recipient.into(),
        network,
        sealing_recipient: recipient.public_key,
        devices: admitting.record().devices().collect(),
        admitting_machines: vec![admitting.record().id()],
        bootstrap: Vec::new(),
        issued_at,
        expires_at: issued_at.plus(60_000),
        signer: signer.public().into(),
    };
    Signed::sign(&body, &signer).unwrap()
}

/// The row a Member that joined before enforcement was the only mode left in the store:
/// no join approval, so only an activation's starting set could make it trusted.
pub(super) async fn joined_before_activation(
    replicated: &ReplicatedStore,
    founder: &Machine,
) -> Machine {
    let late = Machine {
        id: MachineId::random(),
        name: MachineName::parse("late").unwrap(),
        subnet: "10.210.4.0/24".parse().unwrap(),
        public_key: WireGuardPublicKey([4; 32]),
        ..founder.clone()
    };
    replicated.publish_local_machine(&late).await.unwrap();
    late
}
