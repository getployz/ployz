//! Every coverage case, sent unsigned to an enforcing Machine through its Machine API.
//!
//! A call that reaches the API over TCP carries no management connection and no local
//! socket, so an enforcing Machine treats it as an Operator's, as it treats Cloud's. Each
//! case gets a fresh Machine, so "nothing changed" compares one case's effects only.

use std::collections::BTreeSet;
use std::path::PathBuf;

use ployz_core::authority::coverage::{CASES, Case, Expected};
use ployz_core::authority::{Signed, Signer};
use ployz_core::{
    DeviceKey, Machine, MachineId, MachineRpc, MachineRpcClient, OPERATOR_SCOPE_HEADER,
    OpaquePayload, Rpc, RpcRequest, RpcResponse, RpcResponseBody, op, rpc::status_details,
};
use serde_json::{Value, json};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Code, Request, Status, transport::Server};

use super::MachineService;
use crate::corrosion::ReplicatedStore;
use crate::docker::test_support::FakeDocker;
use crate::machine::{LocalMachineStore, RecordOwner};
use crate::sealing_key::SealingKey;

/// Cases only a live lane answers, with the lane. A test daemon refuses an upgrade from a
/// non-system install path before either check runs.
const LIVE_ONLY: &[(&str, &str, &str)] = &[
    (
        ployz_core::REQUEST_MACHINE_UPGRADE_CAPABILITY,
        "an exact version no channel resolves to",
        "F1 lane 9: a test daemon refuses non-system paths before it resolves a release",
    ),
    (
        ployz_core::REQUEST_MACHINE_UPGRADE_CAPABILITY,
        "a downgrade",
        "F1 lane 9: a test daemon refuses non-system paths before it reads the installed release",
    ),
];

/// Cases this harness can't reach over TCP, with the unit test that proves each. Cloud's
/// own connection is an iroh connection, and a rule grant needs GitHub's signed token.
const UNIT_ONLY: &[(&str, &str, &str)] = &[
    (
        ployz_core::SET_MANAGEMENT_CLIENT_CAPABILITY,
        "from Cloud's own connection",
        "management_client::tests::no_caller_writes_a_device_slot_through_set_management_client",
    ),
    (
        ployz_core::MINT_BUILD_GRANT_CAPABILITY,
        "with a rule and its proof",
        "rule_grant_tests::a_valid_token_mints_and_its_secrets_open_only_for_the_build",
    ),
    (
        ployz_core::SET_VOLUME_COPY_CAPABILITY,
        "a pull",
        "volume_approval_tests::a_pull_needs_an_approval_that_names_this_machine_and_its_source",
    ),
    (
        ployz_core::SET_VOLUME_COPY_CAPABILITY,
        "a cut",
        "volume_approval_tests::a_cut_a_promote_and_a_forget_of_a_copy_need_an_approval",
    ),
    (
        ployz_core::SET_VOLUME_COPY_CAPABILITY,
        "a promote with no receipt",
        "volume_approval_tests::a_promote_without_a_receipt_runs_only_under_a_restore_of_this_copy_at_its_snapshot",
    ),
    (
        ployz_core::SET_VOLUME_COPY_CAPABILITY,
        "a forget of a copy",
        "volume_approval_tests::a_forget_of_a_copy_needs_a_removal_of_its_role",
    ),
];

struct Enforcing {
    data_dir: PathBuf,
    peer_dir: PathBuf,
    store: RecordOwner,
    service: MachineService,
    fake: FakeDocker,
    replicated: ReplicatedStore,
    machine: Machine,
    peer: Machine,
    peer_store: LocalMachineStore,
    address: String,
    _cluster: tokio::task::JoinHandle<()>,
    _server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

fn founder(dir: &std::path::Path, name: &str) -> LocalMachineStore {
    let mut store = LocalMachineStore::open(dir).unwrap();
    store
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Founder {
                initial_policy: Default::default(),
                name: ployz_core::MachineName::parse(name).unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                    "192.0.2.1:51820".parse().unwrap(),
                )],
                grant: None,
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    store
}

impl Enforcing {
    async fn new(enforced: bool) -> Self {
        use crate::docker::test_support::fake_runtime_with;
        let data_dir =
            std::env::temp_dir().join(format!("ployzd-authorization-{}", MachineId::random()));
        let peer_dir = data_dir.with_extension("peer");
        let store = founder(&data_dir, "local");
        let machine = store.record().machine().unwrap().clone();
        let peer_store = founder(&peer_dir, "peer");
        let peer = peer_store.record().machine().unwrap().clone();
        let (replicated, cluster) = crate::corrosion::fake_cluster::store().await;
        replicated.publish_local_machine(&machine).await.unwrap();
        replicated.publish_local_machine(&peer).await.unwrap();
        let held = SealingKey::load(&data_dir).unwrap().unwrap();
        replicated
            .publish_sealing_public_key(held.public_key())
            .await
            .unwrap();
        let store = RecordOwner::spawn(store).unwrap();
        let device = DeviceKey::from(Signer::from_seed(&[7; 32]).public());
        let starting = vec![ployz_core::authority::Member::from(&peer)];
        store
            .mutate(move |store| {
                store.approve_device(device).unwrap();
                store.set_device_enforcement(enforced, starting).unwrap();
            })
            .await
            .unwrap();
        let (runtime, fake) = fake_runtime_with(FakeDocker {
            named_containers: Some(std::sync::Arc::new(std::sync::Mutex::new(
                Default::default(),
            ))),
            list_named: true,
            ..Default::default()
        })
        .await;
        let mut service = MachineService::with_cluster(
            store.clone(),
            Some((
                replicated.clone(),
                crate::corrosion::AdminClient::new("/no/admin"),
            )),
        )
        .with_optional_containers(Some(runtime));
        service.local = service.local.with_zpool("true");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let api = crate::machine_api::MachineApi::from_local(service.clone());
        let server = tokio::spawn(
            Server::builder().serve_with_incoming(api, TcpListenerStream::new(listener)),
        );
        Self {
            data_dir,
            peer_dir,
            store,
            service,
            fake,
            replicated,
            machine,
            peer,
            peer_store,
            address,
            _cluster: cluster,
            _server: server,
        }
    }

    /// What a refusal must leave as it was: Docker's calls, this Machine's record and
    /// Sealing Key, and the Cluster's Machine rows.
    async fn snapshot(&self) -> String {
        let docker = self.fake.requests.lock().unwrap().len();
        let record = format!("{:?}", self.store.record());
        let key = SealingKey::load(&self.data_dir)
            .unwrap()
            .map(|key| key.public_key());
        let rows: BTreeSet<String> = self
            .replicated
            .machines()
            .await
            .unwrap()
            .observations
            .iter()
            .map(|machine| format!("{machine:?}"))
            .collect();
        format!("{docker}\n{record}\n{key:?}\n{rows:?}")
    }

    async fn unary(&self, path: &'static str, request: &RpcRequest) -> Answer {
        let channel = tonic::transport::Endpoint::from_shared(self.address.clone())
            .unwrap()
            .connect()
            .await
            .unwrap();
        let mut grpc = tonic::client::Grpc::new(channel);
        grpc.ready().await.unwrap();
        let result = grpc
            .unary(
                Request::new(request.encode().unwrap()),
                tonic::codegen::http::uri::PathAndQuery::from_static(path),
                tonic::codec::ProstCodec::<OpaquePayload, OpaquePayload>::default(),
            )
            .await;
        Answer::of(result.map(|response| response.into_inner().decode_response().unwrap()))
    }

    async fn stream(&self, exec: bool) -> Answer {
        let mut client = MachineRpcClient::connect(self.address.clone())
            .await
            .unwrap();
        let input = tokio_stream::iter([OpaquePayload::new(b"{}".to_vec())]);
        let opened = if exec {
            client.exec(input).await.map(|_| ())
        } else {
            match client.build(input).await {
                Ok(response) => {
                    let mut stream = response.into_inner();
                    stream.message().await.map(|_| ())
                }
                Err(status) => Err(status),
            }
        };
        match opened {
            Ok(()) => Answer::Served,
            Err(status) => Answer::of_status(&status),
        }
    }

    fn remove(self) {
        let _ = std::fs::remove_dir_all(&self.data_dir);
        let _ = std::fs::remove_dir_all(&self.peer_dir);
    }
}

/// What the Machine answered a case, read for its authority.
#[derive(Debug)]
enum Answer {
    Served,
    Refused {
        code: String,
        capability: Option<String>,
        #[expect(dead_code, reason = "read through Debug, in a failure's text")]
        message: String,
    },
}

impl Answer {
    fn of(result: Result<RpcResponse, Status>) -> Self {
        let response = match result {
            Ok(response) => response,
            Err(status) => return Self::of_status(&status),
        };
        if let RpcResponseBody::Error(error) = &response.body {
            return Self::Refused {
                code: format!("{:?}", error.code),
                capability: capability(&error.details),
                message: error.message.clone(),
            };
        }
        let payload = serde_json::to_value(&response).unwrap();
        match refusal_in(&payload) {
            Some(refusal) => Self::Refused {
                code: "refused".into(),
                capability: capability(refusal)
                    .or_else(|| refusal.get("details").and_then(capability)),
                message: refusal
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            },
            None => Self::Served,
        }
    }

    fn of_status(status: &Status) -> Self {
        Self::Refused {
            code: format!("{:?}", status.code()),
            capability: status_details(status.details()).and_then(|details| capability(&details)),
            message: status.message().to_owned(),
        }
    }

    /// Refused for authority: denied outright, or answered with a refusal that names a
    /// capability.
    fn for_authority(&self) -> bool {
        match self {
            Self::Served => false,
            Self::Refused {
                code, capability, ..
            } => code == "PermissionDenied" || code == "permission_denied" || capability.is_some(),
        }
    }
}

fn capability(details: &Value) -> Option<String> {
    details
        .get("capability")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// A refusal carried inside an answer that is not an error, such as `Left::Refused`
/// or `EnsuredContainer::Refused`.
fn refusal_in(value: &Value) -> Option<&Value> {
    match value {
        Value::Object(map) => {
            for key in ["refusal", "error"] {
                if let Some(found @ Value::Object(_)) = map.get(key) {
                    return Some(found);
                }
            }
            map.values().find_map(refusal_in)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            None
        }
    }
}

enum Sample {
    Unary(&'static str, Box<RpcRequest>),
    Exec,
    Build,
    /// On [`LIVE_ONLY`] or [`UNIT_ONLY`].
    Elsewhere,
    Unparsed(String),
}

fn unary<T: Rpc>(payload: Value) -> Sample
where
    T::Request: serde::de::DeserializeOwned,
{
    match serde_json::from_value(payload) {
        Ok(request) => Sample::Unary(T::PATH, Box::new(T::into_request(request))),
        Err(error) => Sample::Unparsed(format!("{} sample does not parse: {error}", T::PATH)),
    }
}

fn proven_elsewhere(case: &Case) -> bool {
    LIVE_ONLY
        .iter()
        .chain(UNIT_ONLY)
        .any(|(capability, branch, _)| *capability == case.capability && *branch == case.branch)
}

/// The request a case sends, after any state the case needs is set up on `machine`.
#[expect(clippy::too_many_lines, reason = "one arm per coverage case")]
async fn sample(case: &Case, machine: &Enforcing) -> Sample {
    if proven_elsewhere(case) {
        return Sample::Elsewhere;
    }
    let peer = machine.peer.id;
    match (case.capability, case.branch) {
        (
            ployz_core::ENROLL_MACHINE_CAPABILITY,
            "member, no capsule" | "member, approved capsule",
        ) => unary::<op::Enroll>(json!({
            "as": {"kind": "member", "registration": registration(machine)},
        })),
        (ployz_core::ENROLL_MACHINE_CAPABILITY, "founder") => {
            unary::<op::Enroll>(json!({"as": fresh_founder()}))
        }
        (ployz_core::ENROLL_FOR_RECOVERY_CAPABILITY, _) => unary::<op::EnrollForRecovery>(json!({
            "enroll": {"as": fresh_founder()},
            "recovery": {"expected_public_key": SealingKey::generate().public_key(), "backup": null},
        })),
        (ployz_core::WRAP_SEALING_KEY_CAPABILITY, _) => {
            unary::<op::WrapSealingKey>(json!({"code": recovery_code()}))
        }
        (ployz_core::RECOVER_SEALING_KEY_CAPABILITY, _) => {
            unary::<op::RecoverSealingKey>(json!({"code": recovery_code(), "backup": null}))
        }
        (ployz_core::REGISTER_MACHINE_CAPABILITY, _) => unary::<op::Register>(json!({
            "join_approval": null, "join_confirmation": null,
            "machine_id": MachineId::random(), "assigned_subnet": "10.210.9.0/24",
            "initial_policy": {
                "labels": {}, "accepts_builds": true, "accepts_services": true, "accepts_ingress": true,
            },
            "name": "joiner", "storage": "none",
            "public_key": machine.peer.public_key, "public_ip": null,
            "advertised_endpoints": ["192.0.2.7:51820"], "runtime": {},
        })),
        (ployz_core::SET_MANAGEMENT_CLIENT_CAPABILITY, _) => {
            let label = DeviceKey::from(Signer::from_seed(&[8; 32]).public()).label();
            unary::<op::SetManagementClient>(json!({"kind": "set", "label": label}))
        }
        (ployz_core::APPROVE_JOIN_CAPABILITY, _) => {
            unary::<op::ApproveJoin>(json!({"approval": stranger_join(machine)}))
        }
        (ployz_core::SET_DEVICE_CAPABILITY, _) => unary::<op::SetDevice>(json!({
            "kind": "approve", "key": DeviceKey::from(Signer::from_seed(&[9; 32]).public()),
        })),
        (ployz_core::SET_PUSH_TARGET_CAPABILITY, _) => {
            unary::<op::SetPushTarget>(json!({"url": "https://observe.invalid/push"}))
        }
        (ployz_core::SET_DEVICE_ENFORCEMENT_CAPABILITY, _) => {
            unary::<op::SetDeviceEnforcement>(json!({"enforced": false}))
        }
        (ployz_core::SET_PASSKEY_CAPABILITY, _) => {
            unary::<op::SetPasskey>(json!({"kind": "approve", "passkey": passkey()}))
        }
        (ployz_core::REMOVE_TRUSTED_KEY_CAPABILITY, _) => unary::<op::RemoveTrustedKey>(json!({
            "code": DeviceKey::from(Signer::from_seed(&[7; 32]).public()).code(),
        })),
        (ployz_core::SELECT_ENDPOINT_CAPABILITY, _) => unary::<op::SelectEndpoint>(json!({
            "peer": peer, "endpoint": "192.0.2.9:51820",
        })),
        (ployz_core::UPDATE_MACHINE_CAPABILITY, _) => {
            unary::<op::UpdateMachine>(json!({"update": {"name": "renamed"}}))
        }
        (ployz_core::REQUEST_MACHINE_UPGRADE_CAPABILITY, _) => {
            unary::<op::RequestMachineUpgrade>(json!({
                "attempt_id": "a".repeat(32), "release": "stable",
            }))
        }
        (ployz_core::GENERATE_SEALING_KEY_CAPABILITY, "the Cluster already publishes one") => {
            SealingKey::set(&machine.data_dir, None).unwrap();
            unary::<op::GenerateSealingKey>(json!({}))
        }
        (ployz_core::GENERATE_SEALING_KEY_CAPABILITY, _) => {
            unary::<op::GenerateSealingKey>(json!({}))
        }
        (ployz_core::SHARE_SEALING_KEY_CAPABILITY, _) => unary::<op::ShareSealingKey>(json!({
            "join_approval": null, "machine": peer, "public_key": machine.peer.public_key,
        })),
        (ployz_core::INSTALL_SEALING_KEY_CAPABILITY, "sealed by an accepted Machine's key") => {
            let held = SealingKey::load(&machine.data_dir).unwrap().unwrap();
            let sealed = held
                .seal_from(
                    machine.peer_store.record().private_key(),
                    &machine.machine.public_key,
                )
                .unwrap();
            unary::<op::InstallSealingKey>(json!({
                "from": peer, "sealed": sealed, "public_key": held.public_key(),
            }))
        }
        (ployz_core::INSTALL_SEALING_KEY_CAPABILITY, _) => {
            let other = SealingKey::generate();
            let stranger = crate::network::WireGuardPrivateKey::generate();
            let sealed = other
                .seal_from(&stranger, &machine.machine.public_key)
                .unwrap();
            unary::<op::InstallSealingKey>(json!({
                "from": peer, "sealed": sealed, "public_key": other.public_key(),
            }))
        }
        (ployz_core::LEAVE_MACHINE_CAPABILITY, _) => unary::<op::Leave>(json!({})),
        (ployz_core::FORGET_MACHINE_CAPABILITY, _) => {
            unary::<op::Forget>(json!({"machine_id": peer}))
        }
        (ployz_core::ENSURE_CONTAINER_CAPABILITY, "create") => {
            unary::<op::EnsureContainer>(json!({"want": new_want("api", "nginx:1.29-alpine")}))
        }
        (ployz_core::ENSURE_CONTAINER_CAPABILITY, branch) => {
            let id = running_container(machine).await;
            let want = match branch {
                "start" => {
                    json!({"state":"running", "rotation":"pending", "container":{"existing":{"id":id}}})
                }
                "stop" => json!({"state":"stopped", "container":{"id":id}, "drain":0}),
                "held" => {
                    json!({"state":"held", "container":{"id":id}, "run": "b".repeat(32), "expect": null})
                }
                "out" => {
                    json!({"state":"out", "container":{"id":id}, "allow_empty": true, "expect": null})
                }
                "remove" => json!({"state":"gone", "container":{"id":id}, "drain":0}),
                other => panic!("EnsureContainer has no sample for {other:?}"),
            };
            unary::<op::EnsureContainer>(json!({"want": want}))
        }
        (ployz_core::UPDATE_CONTAINER_LIMITS_CAPABILITY, _) => {
            let id = running_container(machine).await;
            unary::<op::UpdateContainerLimits>(
                json!({"container_id": id, "spec": spec("api", "nginx:1.29-alpine")}),
            )
        }
        (ployz_core::PREPARE_VOLUMES_CAPABILITY, _) => {
            unary::<op::PrepareVolumes>(json!({"specs": []}))
        }
        (ployz_core::REMOVE_VOLUME_CAPABILITY, _) => {
            unary::<op::RemoveVolume>(json!({"name": "app_data", "force": false}))
        }
        (ployz_core::SEED_VOLUME_CAPABILITY, _) => unary::<op::SeedVolume>(json!({
            "name": "app_data", "into": "app_restore", "guid": "7",
        })),
        (ployz_core::SET_VOLUME_COPY_CAPABILITY, branch) => {
            let want = match branch {
                "a Writer want with a start" => json!({
                    "kind": "writer", "at": "7", "epoch": 1,
                    "start": {
                        "kind": "create", "namespace": "app",
                        "resolved_spec": spec("db", "postgres:18"),
                        "creation_key": "attacker-0001", "gen": 1, "rotation": "pending",
                    },
                }),
                "a handoff" => json!({
                    "kind": "handoff", "to": "b".repeat(32), "at": "7", "epoch": 2,
                }),
                "a forget of nothing" => json!({"kind": "forget"}),
                "floor, snapshot" => json!({"kind": "floor"}),
                other => panic!("SetVolumeCopy has no sample for {other:?}"),
            };
            unary::<op::SetVolumeCopy>(json!({
                "name": "app_data", "expect": {"kind": "none"}, "want": want, "gen": 1,
            }))
        }
        (ployz_core::SET_VOLUME_SIZE_CAPABILITY, _) => unary::<op::SetVolumeSize>(json!({
            "name": "app_data", "maximum_bytes": 2_147_483_648_u64, "allow_shrink": false,
        })),
        (ployz_core::PRUNE_IMAGES_CAPABILITY, _) => {
            unary::<op::PruneImages>(json!({"repositories": []}))
        }
        (ployz_core::ENSURE_IMAGE_INGEST_CAPABILITY, _) => {
            unary::<op::EnsureImageIngest>(json!({}))
        }
        (ployz_core::PULL_IMAGE_CAPABILITY, _) => unary::<op::PullImage>(json!({
            "image": "nginx:1.29-alpine", "policy": "missing", "registry_auth": null,
        })),
        (ployz_core::PULL_IMAGE_FROM_MACHINE_CAPABILITY, _) => {
            unary::<op::PullImageFromMachine>(json!({
                "pull": {"mode": "reference", "image": "nginx:1.29-alpine"},
                "source": {"management_address": "fd00::1", "port": 7570},
                "platform": "linux/amd64",
            }))
        }
        (ployz_core::MINT_BUILD_GRANT_CAPABILITY, _) => unary::<op::MintBuildGrant>(json!({
            "repository": "ployz-build/web", "holder": null, "sealed": null, "rule": null,
        })),
        (ployz_core::PUBLISH_CERTIFICATE_MATERIAL_CAPABILITY, _) => {
            unary::<op::PublishCertificateMaterial>(json!({
                "hostname": "app.example.com", "change": {"action": "clear"},
            }))
        }
        (ployz_core::SET_INGRESS_DOORS_CAPABILITY, _) => unary::<op::SetIngressDoors>(json!({
            "doors": ["public"], "proxy_service": "c".repeat(32), "image": "attacker/caddy:latest",
        })),
        (ployz_core::SET_PRIVATE_ACCESS_CAPABILITY, branch) => unary::<op::SetPrivateAccess>(
            json!({"enabled": branch == "on", "sealed_auth_key": null}),
        ),
        (ployz_core::SET_CLUSTER_NAME_CAPABILITY, _) => {
            unary::<op::SetClusterName>(json!({"name": "renamed"}))
        }
        (ployz_core::SET_CLUSTER_DOORS_CAPABILITY, _) => {
            unary::<op::SetClusterDoors>(json!({"doors": ["public"]}))
        }
        (ployz_core::EXEC_CONTAINER_CAPABILITY, _) => Sample::Exec,
        (ployz_core::BUILD_CAPABILITY, _) => Sample::Build,
        (capability, branch) => panic!("{capability} has no sample for branch {branch:?}"),
    }
}

fn recovery_code() -> String {
    "ABCDE-FGHIJ-KLMNO-PQRST-UVWXYZ".into()
}

fn passkey() -> ployz_core::authority::Passkey {
    use ployz_core::authority::trusted_key::{PasskeyAlgorithm, RelyingParty};
    ployz_core::authority::Passkey {
        credential_id: serde_json::from_value(json!("AQID")).unwrap(),
        public_key: ployz_core::authority::PasskeyPublicKey::parse(
            PasskeyAlgorithm::Eddsa,
            iroh::SecretKey::generate().public().as_bytes(),
        )
        .unwrap(),
        account: ployz_core::authority::PasskeyAccount::parse("ada@example.com").unwrap(),
        relying_party: RelyingParty::of(&"https://ployz.cloud".parse().unwrap()).unwrap(),
    }
}

fn fresh_founder() -> Value {
    json!({
        "kind": "founder", "name": "fresh", "cluster_network": "10.211.0.0/16",
        "initial_policy": {
            "labels": {}, "accepts_builds": true, "accepts_services": true, "accepts_ingress": true,
        },
        "public_ip": null, "advertised_endpoints": [], "grant": null,
    })
}

fn registration(machine: &Enforcing) -> Value {
    json!({
        "assigned_machine": machine.peer,
        "visible_peers": [machine.machine],
        "target_versions": {},
        "sealing_key": null,
        "devices": [],
    })
}

/// A join approval of the peer, signed by a key this Machine does not trust.
fn stranger_join(machine: &Enforcing) -> Value {
    use ployz_core::authority::{JoinApproval, Timestamp, join::JoinAllocation};
    let stranger = Signer::from_seed(&[42; 32]);
    let held = SealingKey::load(&machine.data_dir).unwrap().unwrap();
    let approval = JoinApproval {
        cluster: held.public_key(),
        machine: JoinAllocation::from(&machine.peer),
        network: "10.210.0.0/16".parse().unwrap(),
        sealing_recipient: machine.peer.public_key,
        devices: vec![DeviceKey::from(stranger.public())],
        admitting_machines: vec![machine.machine.id],
        bootstrap: Vec::new(),
        issued_at: Timestamp::now(),
        expires_at: Timestamp::now().plus(60_000),
        signer: DeviceKey::from(stranger.public()),
    };
    serde_json::to_value(Signed::sign(&approval, &stranger).unwrap()).unwrap()
}

fn spec(name: &str, image: &str) -> Value {
    json!({
        "service_id": ployz_core::ServiceId::random(),
        "name": name,
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": image, "pull_policy": "missing"},
    })
}

fn new_want(name: &str, image: &str) -> Value {
    json!({"state":"running", "rotation":"pending", "container":{"new":{
        "slot": {"namespace": "app", "service": name, "kind": "service_container",
                 "creation_key": "attacker-0001"},
        "resolved_spec": spec(name, image), "gen": 1}}})
}

async fn running_container(machine: &Enforcing) -> ployz_core::ContainerId {
    let spec: ployz_core::ResolvedServiceSpec =
        serde_json::from_value(spec("api", "nginx:1.29-alpine")).unwrap();
    machine
        .service
        .local
        .create_container(
            ployz_core::ContainerKind::ServiceContainer,
            &ployz_core::Namespace::parse("app").unwrap(),
            &spec,
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .container_id
}

async fn send(machine: &Enforcing, sample: &Sample) -> Answer {
    match sample {
        Sample::Unary(path, request) => machine.unary(path, request).await,
        Sample::Exec => machine.stream(true).await,
        Sample::Build => machine.stream(false).await,
        Sample::Elsewhere | Sample::Unparsed(_) => unreachable!("never sent"),
    }
}

/// The check a `Bounded` case names, run after its request was served.
async fn bounded(case: &Case, check: &str, machine: &Enforcing) {
    match check {
        "a following unsigned EnsureContainer create that uses it is refused" => {
            let create = unary::<op::EnsureContainer>(json!({
                "want": new_want("api", "nginx:1.29-alpine"),
            }));
            let answer = send(machine, &create).await;
            assert!(
                matches!(&answer, Answer::Refused { capability: Some(name), .. }
                    if name == ployz_core::DEPLOY_APPROVAL_CAPABILITY),
                "{case:?}: the create after it was not refused: {answer:?}"
            );
        }
        "the key stays on the Machine" | "the key installed is the one the Cluster publishes" => {
            let held = SealingKey::load(&machine.data_dir)
                .unwrap()
                .expect("the Machine holds a key");
            assert_eq!(
                machine.replicated.sealing_public_key().await.unwrap(),
                Some(held.public_key()),
                "{case:?}"
            );
        }
        "only pinned images run" => {
            let created: Vec<String> = machine
                .fake
                .request_bodies
                .lock()
                .unwrap()
                .iter()
                .filter(|(path, _)| path.ends_with("/containers/create"))
                .map(|(_, body)| body["Image"].as_str().unwrap_or_default().to_owned())
                .collect();
            let pinned = [crate::images::CADDY, crate::images::PAUSE];
            assert!(
                !created.is_empty() && created.iter().all(|image| pinned.contains(&image.as_str())),
                "{case:?}: the doors ran images other than the pins: {created:?}"
            );
        }
        "Device slots are untouched" => {
            assert_eq!(
                machine.store.record().devices().count(),
                1,
                "{case:?}: the approved Device is still the only one"
            );
        }
        other => panic!("{case:?} names a check this test does not run: {other}"),
    }
}

enum Outcome {
    Answered,
    Elsewhere,
    Failed(String),
}

/// Each case on its own fresh Machine; cases share nothing, so they run at once.
#[tokio::test(flavor = "multi_thread")]
async fn every_case_gets_its_expected_answer_from_an_enforcing_machine() {
    let runs: Vec<_> = CASES.iter().map(|case| tokio::spawn(check(case))).collect();
    let mut failures = Vec::new();
    let mut elsewhere = BTreeSet::new();
    for (case, run) in CASES.iter().zip(runs) {
        match run.await.unwrap() {
            Outcome::Answered => {}
            Outcome::Elsewhere => {
                elsewhere.insert((case.capability, case.branch));
            }
            Outcome::Failed(failure) => failures.push(format!("{case:?}: {failure}")),
        }
    }
    let frozen: BTreeSet<_> = LIVE_ONLY
        .iter()
        .chain(UNIT_ONLY)
        .map(|(capability, branch, _)| (*capability, *branch))
        .collect();
    assert_eq!(elsewhere, frozen, "the cases proven elsewhere changed");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

async fn check(case: &'static Case) -> Outcome {
    let machine = Enforcing::new(true).await;
    let outcome = answer(case, &machine).await;
    machine.remove();
    outcome
}

async fn answer(case: &'static Case, machine: &Enforcing) -> Outcome {
    let sample = sample(case, machine).await;
    match &sample {
        Sample::Elsewhere => return Outcome::Elsewhere,
        Sample::Unparsed(error) => return Outcome::Failed(error.clone()),
        Sample::Unary(..) | Sample::Exec | Sample::Build => {}
    }
    let before = machine.snapshot().await;
    let answer = send(machine, &sample).await;
    match case.expected {
        Expected::Refused(names) => match &answer {
            Answer::Refused {
                capability: Some(name),
                ..
            } if names.contains(&name.as_str()) => {
                if machine.snapshot().await != before {
                    return Outcome::Failed("refused, but something changed".into());
                }
            }
            other @ (Answer::Served | Answer::Refused { .. }) => {
                return Outcome::Failed(format!(
                    "expected a refusal naming {names:?}, got {other:?}"
                ));
            }
        },
        Expected::Served | Expected::Bounded(_) if answer.for_authority() => {
            return Outcome::Failed(format!("refused for authority: {answer:?}"));
        }
        Expected::Bounded(check) => bounded(case, check, machine).await,
        Expected::Served => {}
    }
    Outcome::Answered
}

mod departure {
    //! `Leave` and `Forget` under Device enforcement: an Operator is refused before any
    //! Container stops, and a Full caller, or any caller with enforcement off, is not.

    use super::*;

    fn operator<T>(mut request: Request<T>) -> Request<T> {
        request
            .metadata_mut()
            .insert(OPERATOR_SCOPE_HEADER, "1".parse().unwrap());
        request
    }

    fn leave() -> Request<OpaquePayload> {
        Request::new(
            op::Leave::into_request(ployz_core::LeaveRequest { teardown: None })
                .encode()
                .unwrap(),
        )
    }

    fn forget(machine: MachineId) -> Request<OpaquePayload> {
        Request::new(
            op::Forget::into_request(ployz_core::ForgetRequest {
                teardown: None,
                machine_id: machine,
            })
            .encode()
            .unwrap(),
        )
    }

    fn refused_naming(result: Result<tonic::Response<OpaquePayload>, Status>, capability: &str) {
        let status = result.expect_err("an Operator's departure must be refused");
        assert_eq!(
            status.code(),
            Code::PermissionDenied,
            "{}",
            status.message()
        );
        let details = status_details(status.details()).unwrap();
        assert_eq!(
            details.get("capability"),
            Some(&json!(capability)),
            "{details}"
        );
    }

    /// Past the gate. Leave goes on to demote real storage, which a test Machine can't.
    #[track_caller]
    fn admitted(result: Result<tonic::Response<OpaquePayload>, Status>) {
        let answer =
            Answer::of(result.map(|response| response.into_inner().decode_response().unwrap()));
        assert!(!answer.for_authority(), "refused at the gate: {answer:?}");
    }

    async fn rows(machine: &Enforcing) -> BTreeSet<MachineId> {
        machine
            .replicated
            .machines()
            .await
            .unwrap()
            .observations
            .iter()
            .map(|row| row.id)
            .collect()
    }

    #[tokio::test]
    async fn an_operator_leave_under_enforcement_is_refused_before_any_container_stops() {
        let machine = Enforcing::new(true).await;
        let container = running_container(&machine).await;
        let before = machine.snapshot().await;

        refused_naming(
            machine.service.leave(operator(leave())).await,
            ployz_core::LEAVE_MACHINE_CAPABILITY,
        );

        assert_eq!(machine.snapshot().await, before, "nothing changed");
        assert_eq!(
            machine.store.record().phase(),
            ployz_core::LocalMachinePhase::Participating
        );
        let named = format!(
            "{:?}",
            machine
                .fake
                .named_containers
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
        );
        assert!(
            named.contains(container.as_str()),
            "the Container is still there"
        );
        machine.remove();
    }

    #[tokio::test]
    async fn an_operator_forget_under_enforcement_is_refused_and_the_row_stays() {
        let machine = Enforcing::new(true).await;
        let before = machine.snapshot().await;

        refused_naming(
            machine
                .service
                .forget(operator(forget(machine.peer.id)))
                .await,
            ployz_core::FORGET_MACHINE_CAPABILITY,
        );

        assert_eq!(machine.snapshot().await, before, "nothing changed");
        assert!(rows(&machine).await.contains(&machine.peer.id));
        machine.remove();
    }

    #[tokio::test]
    async fn a_full_caller_departs_under_enforcement() {
        let machine = Enforcing::new(true).await;
        machine
            .service
            .forget(forget(machine.peer.id))
            .await
            .unwrap();
        assert!(
            !rows(&machine).await.contains(&machine.peer.id),
            "the row is gone"
        );
        admitted(machine.service.leave(leave()).await);
        machine.remove();
    }

    #[tokio::test]
    async fn an_operator_departs_with_enforcement_off() {
        let machine = Enforcing::new(false).await;
        machine
            .service
            .forget(operator(forget(machine.peer.id)))
            .await
            .unwrap();
        assert!(
            !rows(&machine).await.contains(&machine.peer.id),
            "the row is gone"
        );
        admitted(machine.service.leave(operator(leave())).await);
        machine.remove();
    }

    /// A restarted daemon reads enforcement from the record on disk, so it refuses the
    /// same Leave.
    #[tokio::test]
    async fn a_restarted_daemon_still_refuses_an_operator_leave() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = founder(dir.path(), "local");
        store
            .approve_device(DeviceKey::from(Signer::from_seed(&[7; 32]).public()))
            .unwrap();
        store.set_device_enforcement(true, Vec::new()).unwrap();
        drop(store);

        let reopened = RecordOwner::spawn(LocalMachineStore::open(dir.path()).unwrap()).unwrap();
        let (replicated, _cluster) = crate::corrosion::fake_cluster::store().await;
        let restarted = MachineService::with_cluster(
            reopened,
            Some((replicated, crate::corrosion::AdminClient::new("/no/admin"))),
        );
        refused_naming(
            restarted.leave(operator(leave())).await,
            ployz_core::LEAVE_MACHINE_CAPABILITY,
        );
    }
}

mod private_access {
    //! `SetPrivateAccess` under Device enforcement: an Operator turns Private Access on only
    //! with an approval of the sealed key it sends, and turns it off with none.

    use ployz_core::authority::{
        KeyApproval, PrivateAccessApproval, PrivateAccessTerms, SignerKey, Timestamp,
    };
    use ployz_core::{Ciphertext, PlatformPurpose, SetPrivateAccessRequest, seal_platform};

    use super::*;

    fn device() -> Signer {
        Signer::from_seed(&[7; 32])
    }

    fn sealed(machine: &Enforcing, key: &str) -> Ciphertext {
        let cluster = SealingKey::load(&machine.data_dir)
            .unwrap()
            .unwrap()
            .public_key();
        seal_platform(&cluster, PlatformPurpose::PrivateAccess, key.as_bytes()).unwrap()
    }

    fn approval(machine: &Enforcing, signer: &Signer, sealed: &Ciphertext) -> KeyApproval {
        let cluster = SealingKey::load(&machine.data_dir)
            .unwrap()
            .unwrap()
            .public_key();
        KeyApproval::Signed(
            Signed::sign(
                &PrivateAccessApproval::drafted(&PrivateAccessTerms {
                    cluster,
                    sealed_auth_key: sealed.clone(),
                    issued_at: Timestamp::now(),
                    signer: SignerKey::Device(signer.public().into()),
                }),
                signer,
            )
            .unwrap(),
        )
    }

    async fn turn(machine: &Enforcing, request: SetPrivateAccessRequest) -> Answer {
        machine
            .unary(
                op::SetPrivateAccess::PATH,
                &op::SetPrivateAccess::into_request(request),
            )
            .await
    }

    fn on(sealed: Ciphertext, approval: Option<KeyApproval>) -> SetPrivateAccessRequest {
        SetPrivateAccessRequest {
            enabled: true,
            sealed_auth_key: Some(sealed),
            approval,
        }
    }

    #[track_caller]
    fn refused(answer: &Answer) {
        assert!(
            matches!(answer, Answer::Refused { capability: Some(name), .. }
                if name == ployz_core::PRIVATE_ACCESS_APPROVAL_CAPABILITY),
            "{answer:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_operator_turns_private_access_on_only_with_an_approval_of_its_key() {
        let machine = Enforcing::new(true).await;
        let key = sealed(&machine, "tskey-auth-approved");
        let before = machine.snapshot().await;
        refused(&turn(&machine, on(key.clone(), None)).await);
        let other = sealed(&machine, "tskey-auth-planted");
        let planted = on(other, Some(approval(&machine, &device(), &key)));
        refused(&turn(&machine, planted).await);
        let stranger = Signer::from_seed(&[8; 32]);
        refused(
            &turn(
                &machine,
                on(key.clone(), Some(approval(&machine, &stranger, &key))),
            )
            .await,
        );
        assert_eq!(
            machine.snapshot().await,
            before,
            "a refusal changed nothing"
        );
        let approved = turn(
            &machine,
            on(key.clone(), Some(approval(&machine, &device(), &key))),
        )
        .await;
        assert!(
            !approved.for_authority(),
            "refused at the gate: {approved:?}"
        );
        machine.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn turning_off_or_a_machine_not_enforcing_needs_no_approval() {
        let machine = Enforcing::new(true).await;
        let off = turn(
            &machine,
            SetPrivateAccessRequest {
                enabled: false,
                sealed_auth_key: None,
                approval: None,
            },
        )
        .await;
        assert!(!off.for_authority(), "{off:?}");
        machine.remove();
        let machine = Enforcing::new(false).await;
        let key = sealed(&machine, "tskey-auth-unenforced");
        let answer = turn(&machine, on(key, None)).await;
        assert!(!answer.for_authority(), "{answer:?}");
        machine.remove();
    }
}

mod teardown {
    //! A teardown under Device enforcement: an Operator's takedown, Volume removal, `Leave`
    //! or `Forget` runs only when a person's teardown names this Server and what goes.

    use ployz_core::authority::{
        SignerKey, Teardown, TeardownApproval, TeardownScope, TeardownTerms, Timestamp,
    };
    use ployz_core::{
        DockerVolumeId, DockerVolumeName, EnsureContainerRequest, ForgetRequest, LeaveRequest,
        Namespace, RemoveVolumeRequest,
    };

    use super::*;

    fn approval(machine: &Enforcing, signer: &Signer, scope: TeardownScope) -> TeardownApproval {
        let cluster = SealingKey::load(&machine.data_dir)
            .unwrap()
            .unwrap()
            .public_key();
        TeardownApproval::Signed(
            Signed::sign(
                &Teardown::drafted(TeardownTerms {
                    cluster,
                    scope,
                    issued_at: Timestamp::now(),
                    signer: SignerKey::Device(signer.public().into()),
                }),
                signer,
            )
            .unwrap(),
        )
    }

    fn device() -> Signer {
        Signer::from_seed(&[7; 32])
    }

    fn namespace(
        name: &str,
        machines: Vec<MachineId>,
        volumes: Vec<DockerVolumeId>,
    ) -> TeardownScope {
        TeardownScope::Namespace {
            namespaces: vec![Namespace::parse(name).unwrap()],
            machines,
            volumes,
        }
    }

    fn data(machine: &Enforcing) -> DockerVolumeId {
        DockerVolumeId {
            machine_id: machine.machine.id,
            name: DockerVolumeName::parse("app_data").unwrap(),
        }
    }

    async fn remove_volume(machine: &Enforcing, teardown: Option<TeardownApproval>) -> Answer {
        let request = RemoveVolumeRequest {
            name: DockerVolumeName::parse("app_data").unwrap(),
            force: false,
            teardown: teardown.map(Box::new),
        };
        machine
            .unary(
                op::RemoveVolume::PATH,
                &op::RemoveVolume::into_request(request),
            )
            .await
    }

    async fn take_down(machine: &Enforcing, state: &str, teardown: TeardownApproval) -> Answer {
        let id = running_container(machine).await;
        let want = match state {
            "start" => {
                json!({"state":"running", "rotation":"pending", "container":{"existing":{"id":id}}})
            }
            "remove" => json!({"state":"gone", "container":{"id":id}, "drain":0}),
            other => panic!("no takedown sample for {other:?}"),
        };
        let mut request: EnsureContainerRequest =
            serde_json::from_value(json!({"want": want})).unwrap();
        request.teardown = Some(Box::new(teardown));
        machine
            .unary(
                op::EnsureContainer::PATH,
                &op::EnsureContainer::into_request(request),
            )
            .await
    }

    #[track_caller]
    fn refused(answer: &Answer, capability: &str) {
        assert!(
            matches!(answer, Answer::Refused { capability: Some(name), .. } if name == capability),
            "{answer:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_teardown_removes_only_the_containers_it_names_and_starts_nothing() {
        let machine = Enforcing::new(true).await;
        let here = machine.machine.id;
        let other_namespace = approval(&machine, &device(), namespace("shop", vec![here], vec![]));
        refused(
            &take_down(&machine, "remove", other_namespace).await,
            ployz_core::TEARDOWN_CAPABILITY,
        );
        let elsewhere = approval(
            &machine,
            &device(),
            namespace("app", vec![machine.peer.id], vec![]),
        );
        refused(
            &take_down(&machine, "remove", elsewhere).await,
            ployz_core::TEARDOWN_CAPABILITY,
        );
        let stranger = approval(
            &machine,
            &Signer::from_seed(&[8; 32]),
            namespace("app", vec![here], vec![]),
        );
        refused(
            &take_down(&machine, "remove", stranger).await,
            ployz_core::TEARDOWN_CAPABILITY,
        );
        let app = approval(&machine, &device(), namespace("app", vec![here], vec![]));
        refused(
            &take_down(&machine, "start", app.clone()).await,
            ployz_core::TEARDOWN_CAPABILITY,
        );
        let removed = take_down(&machine, "remove", app).await;
        assert!(!removed.for_authority(), "{removed:?}");
        machine.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_volume_goes_only_under_a_teardown_that_names_it_here() {
        let machine = Enforcing::new(true).await;
        let before = machine.snapshot().await;
        refused(
            &remove_volume(&machine, None).await,
            ployz_core::TEARDOWN_CAPABILITY,
        );
        let containers_only = approval(
            &machine,
            &device(),
            namespace("app", vec![machine.machine.id], vec![]),
        );
        refused(
            &remove_volume(&machine, Some(containers_only)).await,
            ployz_core::TEARDOWN_CAPABILITY,
        );
        let mut peers = data(&machine);
        peers.machine_id = machine.peer.id;
        let on_the_peer = approval(&machine, &device(), namespace("app", vec![], vec![peers]));
        refused(
            &remove_volume(&machine, Some(on_the_peer)).await,
            ployz_core::TEARDOWN_CAPABILITY,
        );
        assert_eq!(
            machine.snapshot().await,
            before,
            "a refusal changed nothing"
        );
        let named = approval(
            &machine,
            &device(),
            namespace("app", vec![], vec![data(&machine)]),
        );
        let answer = remove_volume(&machine, Some(named)).await;
        assert!(!answer.for_authority(), "{answer:?}");
        machine.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_departure_runs_only_under_a_server_teardown_naming_this_server() {
        let machine = Enforcing::new(true).await;
        let here = machine.machine.id;
        let peer = machine.peer.id;
        let server = |leaving, forgetting| TeardownScope::Server {
            leaving,
            forgetting,
            volumes: vec![],
        };
        let forget = |teardown: TeardownApproval| {
            op::Forget::into_request(ForgetRequest {
                machine_id: peer,
                teardown: Some(Box::new(teardown)),
            })
        };
        let before = machine.snapshot().await;
        let not_this_one = approval(&machine, &device(), server(peer, vec![]));
        refused(
            &machine
                .unary(
                    op::Leave::PATH,
                    &op::Leave::into_request(LeaveRequest {
                        teardown: Some(Box::new(not_this_one)),
                    }),
                )
                .await,
            ployz_core::LEAVE_MACHINE_CAPABILITY,
        );
        let others_forget = approval(&machine, &device(), server(peer, vec![]));
        refused(
            &machine
                .unary(op::Forget::PATH, &forget(others_forget))
                .await,
            ployz_core::FORGET_MACHINE_CAPABILITY,
        );
        let namespace_teardown =
            approval(&machine, &device(), namespace("app", vec![here], vec![]));
        refused(
            &machine
                .unary(op::Forget::PATH, &forget(namespace_teardown))
                .await,
            ployz_core::FORGET_MACHINE_CAPABILITY,
        );
        assert_eq!(
            machine.snapshot().await,
            before,
            "a refusal changed nothing"
        );
        let forgets = approval(&machine, &device(), server(peer, vec![here]));
        let answer = machine.unary(op::Forget::PATH, &forget(forgets)).await;
        assert!(!answer.for_authority(), "{answer:?}");
        let leaves = approval(&machine, &device(), server(here, vec![]));
        let answer = machine
            .unary(
                op::Leave::PATH,
                &op::Leave::into_request(LeaveRequest {
                    teardown: Some(Box::new(leaves)),
                }),
            )
            .await;
        assert!(!answer.for_authority(), "{answer:?}");
        machine.remove();
    }
}
