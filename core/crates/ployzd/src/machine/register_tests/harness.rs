use ployz_core::{
    AdvertisedEndpoint, Machine, MachineId, MachineName, MachineRpc, MachineRuntime,
    RegisterRequest, Registered, RpcError, RpcResponseBody, WireGuardPublicKey, op,
};
use tonic::Request;

use super::super::{LocalMachine, LocalMachineStore, RecordOwner};
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
    let (replicated, server) = fake_cluster::store().await;
    let (data_dir, store, founder) = open_store("ployzd-register");
    replicated.publish_local_machine(&founder).await.unwrap();
    let local = LocalMachine::new(store).with_cluster(Some((
        replicated.clone(),
        AdminClient::new("/no/such/ployz-admin.sock"),
    )));
    (local, replicated, founder, data_dir, server)
}

pub(super) fn open_store(prefix: &str) -> (std::path::PathBuf, RecordOwner, Machine) {
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
            },
            wireguard_mtu: None,
        })
        .unwrap();
    let founder = store.record().machine().unwrap().clone();
    (data_dir, RecordOwner::spawn(store).unwrap(), founder)
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
        machine_id: MachineId::random(),
        assigned_subnet: Some("10.210.1.0/24".parse().unwrap()),
        initial_policy: Default::default(),
        name: MachineName::parse(name).unwrap(),
        storage: ployz_core::StorageChoice::None,
        public_key,
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.9:51820".parse().unwrap())],
        runtime: MachineRuntime::default(),
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
