use std::{
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use ployz_core::{
    AdvertisedEndpoint, CORROSION_GOSSIP_PORT, Machine, MachineId, MachineName, MachineRpc,
    MachineRuntime, RegisterRequest, Registered, RpcError, RpcResponseBody, WireGuardPublicKey, op,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tonic::Request;

use super::super::{LocalMachine, LocalMachineStore};
use crate::{
    corrosion::{AdminClient, ReplicatedStore, fake_cluster},
    machine_api::{MachineService, REGISTER_FORWARDED_METADATA},
};

pub(super) async fn participating() -> (
    LocalMachine,
    ReplicatedStore,
    Machine,
    std::path::PathBuf,
    tokio::task::JoinHandle<()>,
) {
    let setup = participating_without_allocator().await;
    setup
        .1
        .publish_founder_allocator(&setup.2.id)
        .await
        .unwrap();
    setup
}

pub(super) async fn participating_without_allocator() -> (
    LocalMachine,
    ReplicatedStore,
    Machine,
    std::path::PathBuf,
    tokio::task::JoinHandle<()>,
) {
    let (replicated, server) = fake_cluster::store().await;
    let (data_dir, store, founder) = open_store("ployzd-register");
    replicated.publish_local_machine(&founder).await.unwrap();
    let local = LocalMachine::new(store, watch::channel(false).0).with_cluster(Some((
        replicated.clone(),
        AdminClient::new("/no/such/ployz-admin.sock"),
    )));
    (local, replicated, founder, data_dir, server)
}

pub(super) fn open_store(
    prefix: &str,
) -> (std::path::PathBuf, Arc<Mutex<LocalMachineStore>>, Machine) {
    let data_dir = std::env::temp_dir().join(format!("{prefix}-{}", MachineId::random()));
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    let founder = store
        .initialize(
            MachineName::parse("edge").unwrap(),
            crate::machine::FoundingCluster {
                network: "10.210.0.0/16".parse().unwrap(),
                ingress_proxy_backend: ployz_core::IngressProxyBackend::Caddy,
            },
            None,
            vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            None,
            None,
        )
        .unwrap();
    (data_dir, Arc::new(Mutex::new(store)), founder)
}

pub(super) fn machine_service(
    store: Arc<Mutex<LocalMachineStore>>,
    replicated: ReplicatedStore,
    port: Option<u16>,
) -> MachineService {
    let service = MachineService::with_cluster(
        store,
        watch::channel(false).0,
        Some((replicated, AdminClient::new("/no/such/ployz-admin.sock"))),
    );
    match port {
        Some(port) => service.with_machine_api_port(port),
        None => service,
    }
}

pub(super) async fn publish_peers(replicated: &ReplicatedStore, count: usize) -> Vec<Machine> {
    let mut peers = Vec::with_capacity(count);
    for index in 0..count {
        let seed = u8::try_from(index + 10).expect("peer seeds fit u8");
        let machine = Machine {
            id: MachineId::random(),
            name: MachineName::parse(format!("peer-{seed}")).unwrap(),
            subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
            public_key: WireGuardPublicKey([seed; 32]),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint(
                format!("192.0.2.{seed}:51820").parse().unwrap(),
            )],
            runtime: MachineRuntime::default(),
        };
        replicated.publish_local_machine(&machine).await.unwrap();
        peers.push(machine);
    }
    peers
}

pub(super) async fn serve_membership(
    states: &[(&Machine, &'static str)],
) -> (tokio::task::JoinHandle<()>, PathBuf, PathBuf) {
    let states: Vec<_> = states
        .iter()
        .map(|&(machine, state)| {
            (
                format!(
                    "[{}]:{CORROSION_GOSSIP_PORT}",
                    machine.management_address().0
                ),
                state,
            )
        })
        .collect();
    let root = std::env::temp_dir().join(format!("ployzd-register-admin-{}", MachineId::random()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("admin.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            if read_admin_frame(&mut stream).await.is_err() {
                continue;
            }
            for (addr, state) in &states {
                let payload = serde_json::json!({
                    "Json": {"id": {"addr": addr}, "state": state}
                });
                if write_admin_frame(&mut stream, &serde_json::to_vec(&payload).unwrap())
                    .await
                    .is_err()
                {
                    break;
                }
            }
            let _ = write_admin_frame(&mut stream, br#""Success""#).await;
        }
    });
    (server, path, root)
}

pub(super) async fn read_admin_frame(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let length = stream.read_u32().await?;
    let mut data = vec![0; length as usize];
    stream.read_exact(&mut data).await?;
    Ok(data)
}

pub(super) async fn write_admin_frame(stream: &mut UnixStream, data: &[u8]) -> io::Result<()> {
    stream.write_u32(data.len() as u32).await?;
    stream.write_all(data).await
}

pub(super) fn unreachable_allocator(id: MachineId) -> Machine {
    Machine {
        id,
        name: MachineName::parse("allocator").unwrap(),
        subnet: "10.210.0.0/24".parse().unwrap(),
        public_key: WireGuardPublicKey([3; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.3:51820".parse().unwrap())],
        runtime: MachineRuntime::default(),
    }
}

pub(super) fn request(name: &str, public_key: WireGuardPublicKey) -> RegisterRequest {
    RegisterRequest {
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
    forwarded: bool,
) -> Result<Registered, RpcError> {
    let mut request = Request::new(op::Register::into_request(body).encode().unwrap());
    if forwarded {
        request.metadata_mut().insert(
            REGISTER_FORWARDED_METADATA,
            "1".parse().expect("ASCII metadata"),
        );
    }
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
