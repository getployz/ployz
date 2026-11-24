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
    DeviceKey, Machine, MachineId, MachineRpcClient, OpaquePayload, Rpc, RpcRequest, RpcResponse,
    RpcResponseBody, WireGuardPublicKey, op, rpc::status_details,
};
use serde_json::{Value, json};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Status, transport::Server};

use super::MachineService;
use crate::corrosion::ReplicatedStore;
use crate::docker::test_support::FakeDocker;
use crate::machine::{LocalMachineStore, RecordOwner};
use crate::sealing_key::SealingKey;

const PINNED_NGINX: &str =
    "nginx@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Cases this harness can't reach over TCP, with the unit test that proves each. Cloud's
/// own connection is an iroh connection, a rule grant needs GitHub's signed token, and an
/// upgrade resolves releases only on an installed Machine.
const UNIT_ONLY: &[(&str, &str, &str)] = &[
    (
        ployz_core::REQUEST_MACHINE_UPGRADE_CAPABILITY,
        "an exact version no channel resolves to",
        "installer::upgrade::tests::an_operator_names_only_a_release_a_channel_resolves_to",
    ),
    (
        ployz_core::REQUEST_MACHINE_UPGRADE_CAPABILITY,
        "a downgrade",
        "installer::upgrade::tests::only_a_direct_connection_downgrades",
    ),
    (
        ployz_core::SET_INGRESS_DOORS_CAPABILITY,
        "tunnel door",
        "tests/network_namespace.rs::the_tunnel_connector_reaches_dns_and_the_cloudflare_edge_but_no_origin_a_route_names, with ingress_doors::the_tunnel_door_closes_the_ports_and_runs_the_connector_on_the_opened_token",
    ),
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
    async fn new() -> Self {
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
                store.trust_members(starting).unwrap();
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
        unary_at(self.address.clone(), path, request, false).await
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
    /// On [`UNIT_ONLY`].
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
    UNIT_ONLY
        .iter()
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
        (ployz_core::ENROLL_MACHINE_CAPABILITY, "member, no capsule") => {
            unary::<op::Enroll>(json!({
                "as": {"kind": "member", "registration": {
                    "assigned_machine": machine.machine,
                    "visible_peers": [machine.peer],
                    "target_versions": {},
                    "sealing_key": null,
                    "devices": [],
                }},
            }))
        }
        (ployz_core::ENROLL_MACHINE_CAPABILITY, "member, approved capsule") => {
            unary::<op::Enroll>(json!({
                "as": {"kind": "member", "registration": registration(machine)},
            }))
        }
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
        (ployz_core::SET_KEY_CAPABILITY, _) => unary::<op::SetKey>(json!({
            "key": ployz_core::first_key::LineKey::Device {
                key: DeviceKey::from(Signer::from_seed(&[9; 32]).public()),
            },
        })),
        (ployz_core::CONFIRM_JOIN_CAPABILITY, _) => unary::<op::ConfirmJoin>(json!({
            "restrictions": ployz_core::JoinRestrictions {
                set: ployz_core::JoinRestrictionSet {
                    removed_keys: Vec::new(),
                    removed_members: Vec::new(),
                    floors: std::collections::BTreeMap::new(),
                },
                tag: ployz_core::authority::join::RestrictionTag([7; 32]),
            },
        })),
        (ployz_core::SET_PUSH_TARGET_CAPABILITY, _) => {
            unary::<op::SetPushTarget>(json!({"url": "https://observe.invalid/push"}))
        }
        (ployz_core::KEY_STEP_CAPABILITY, _) => {
            unary::<op::KeyStep>(json!({"step": stranger_removal(machine)}))
        }
        (ployz_core::FORGET_VERSIONS_CAPABILITY, _) => {
            unary::<op::ForgetVersions>(json!({"versions": [stranger_forget(machine)]}))
        }
        (ployz_core::SELECT_ENDPOINT_CAPABILITY, _) => unary::<op::SelectEndpoint>(json!({
            "peer": peer, "endpoint": "192.0.2.9:51820",
        })),
        (ployz_core::UPDATE_MACHINE_CAPABILITY, _) => {
            unary::<op::UpdateMachine>(json!({"update": {"name": "renamed"}}))
        }
        (ployz_core::REQUEST_MACHINE_UPGRADE_CAPABILITY, branch) => {
            let release = match branch {
                "an exact version no channel resolves to" => "999.0.0",
                "a downgrade" => "0.0.1",
                _ => "stable",
            };
            unary::<op::RequestMachineUpgrade>(json!({
                "attempt_id": "a".repeat(32), "release": release,
            }))
        }
        (ployz_core::GENERATE_SEALING_KEY_CAPABILITY, "the Cluster already publishes one") => {
            SealingKey::set(&machine.data_dir, None).unwrap();
            unary::<op::GenerateSealingKey>(json!({}))
        }
        (ployz_core::GENERATE_SEALING_KEY_CAPABILITY, _) => {
            unary::<op::GenerateSealingKey>(json!({}))
        }
        (ployz_core::SHARE_SEALING_KEY_CAPABILITY, "no join approval, to a Member") => {
            unary::<op::ShareSealingKey>(json!({
                "join_approval": null, "machine": peer, "public_key": machine.peer.public_key,
            }))
        }
        (ployz_core::SHARE_SEALING_KEY_CAPABILITY, _) => {
            let stranger = Machine {
                id: MachineId::random(),
                public_key: WireGuardPublicKey([42; 32]),
                ..machine.peer.clone()
            };
            machine
                .replicated
                .publish_local_machine(&stranger)
                .await
                .unwrap();
            unary::<op::ShareSealingKey>(json!({
                "join_approval": null, "machine": stranger.id, "public_key": stranger.public_key,
            }))
        }
        (ployz_core::INSTALL_SEALING_KEY_CAPABILITY, "sealed by an accepted Machine's key") => {
            let held = SealingKey::load(&machine.data_dir).unwrap().unwrap();
            let sealed = held
                .seal_from(
                    machine.peer_store.record().private_key(),
                    &machine.machine.public_key,
                    None,
                    None,
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
                .seal_from(&stranger, &machine.machine.public_key, None, None)
                .unwrap();
            unary::<op::InstallSealingKey>(json!({
                "from": peer, "sealed": sealed, "public_key": other.public_key(),
            }))
        }
        (ployz_core::LEAVE_MACHINE_CAPABILITY, _) => unary::<op::Leave>(json!({})),
        (ployz_core::FORGET_MACHINE_CAPABILITY, _) => {
            unary::<op::Forget>(json!({"machine_id": peer}))
        }
        (ployz_core::ENSURE_CONTAINER_CAPABILITY, "create, reaches data") => {
            unary::<op::EnsureContainer>(json!({"want": mounting_want()}))
        }
        (ployz_core::ENSURE_CONTAINER_CAPABILITY, "create, reaches no data") => {
            unary::<op::EnsureContainer>(json!({"want": new_want("api", "nginx:1.29-alpine")}))
        }
        (ployz_core::ENSURE_PINNED_CONTAINER_CAPABILITY, branch) => {
            let want = match branch {
                "create, reaches data" => pinned(mounting_want()),
                "create, reaches no data" => pinned(new_want("api", "nginx:1.29-alpine")),
                "digest spec" => {
                    pinned(new_want("api", &format!("nginx@sha256:{}", "7".repeat(64))))
                }
                "no pin" => {
                    let mut want = pinned(new_want("api", "nginx:1.29-alpine"));
                    want.pointer_mut("/container/new")
                        .and_then(Value::as_object_mut)
                        .unwrap()
                        .remove("image_id");
                    want
                }
                other => panic!("EnsurePinnedContainer has no sample for {other:?}"),
            };
            unary::<op::EnsurePinnedContainer>(json!({"want": want}))
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
        (ployz_core::UPDATE_CONTAINER_PLACEMENT_CAPABILITY, _) => {
            let id = running_container(machine).await;
            unary::<op::UpdateContainerPlacement>(
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
        (ployz_core::SEED_COPY_CAPABILITY, _) => unary::<op::SeedCopy>(json!({
            "seed": {"name": "app_data", "into": "app_restore", "guid": "7"},
            "step": unsigned_copy(machine),
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
                "a pull" | "a cut" => {
                    json!({"kind": "mirror", "from": null, "target": "f-1", "drain": 0})
                }
                "a promote" => json!({"kind": "writer", "at": "7", "epoch": 2}),
                "a forget of a copy" | "a forget of nothing" => json!({"kind": "forget"}),
                "floor, snapshot" => json!({"kind": "floor"}),
                other => panic!("SetVolumeCopy has no sample for {other:?}"),
            };
            let expect = match branch {
                "a cut" => json!({"kind": "writer", "epoch": 1}),
                "a promote" | "a forget of a copy" => {
                    json!({"kind": "mirror", "newest": "7", "slot": "1"})
                }
                _ => json!({"kind": "none"}),
            };
            unary::<op::SetVolumeCopy>(json!({
                "name": "app_data", "expect": expect, "want": want, "gen": 1,
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
        (ployz_core::PULL_IMAGE_CAPABILITY, "tag") => unary::<op::PullImage>(json!({
            "image": "nginx:1.29-alpine", "policy": "missing", "registry_auth": null,
        })),
        (ployz_core::PULL_IMAGE_CAPABILITY, _) => unary::<op::PullImage>(json!({
            "image": PINNED_NGINX, "policy": "missing", "registry_auth": null,
        })),
        (ployz_core::PULL_FREE_IMAGE_CAPABILITY, _) => unary::<op::PullFreeImage>(json!({
            "image": "nginx:1.29-alpine", "policy": "missing",
        })),
        (ployz_core::PULL_IMAGE_FROM_MACHINE_CAPABILITY, branch) => {
            let image = if branch == "tag" {
                "nginx:1.29-alpine"
            } else {
                PINNED_NGINX
            };
            unary::<op::PullImageFromMachine>(json!({
                "pull": {"mode": "reference", "image": image},
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

/// A removal of a Device, signed by a key this Machine does not trust.
fn stranger_removal(machine: &Enforcing) -> Value {
    use ployz_core::authority::{KeyChange, KeyRef, KeyStep};
    let stranger = Signer::from_seed(&[42; 32]);
    let step = KeyStep {
        cluster: SealingKey::load(&machine.data_dir)
            .unwrap()
            .unwrap()
            .public_key(),
        change: KeyChange::Remove {
            key: KeyRef::Signing {
                key: DeviceKey::from(Signer::from_seed(&[7; 32]).public()),
            },
        },
        signer: DeviceKey::from(stranger.public()).into(),
        proposal: None,
    };
    serde_json::to_value(Signed::sign(&step, &stranger).unwrap()).unwrap()
}

/// A Copy of `app_data` into `app_restore` in a bundle nobody signed.
fn unsigned_copy(machine: &Enforcing) -> Value {
    use ployz_core::authority::{
        CopyStep, ProposalApproval, ProposalRef, ProposalStep, StepDigest, StepKind, Timestamp,
    };
    let stranger = Signer::from_seed(&[42; 32]);
    let cluster = SealingKey::load(&machine.data_dir)
        .unwrap()
        .unwrap()
        .public_key();
    let app = ployz_core::Namespace::parse("app").unwrap();
    let operation = ployz_core::OperationId::parse("0f8fad5bd9cb469fa16570867728950e").unwrap();
    let volume = |name: &str| ployz_core::DockerVolumeName::parse(name).unwrap();
    let step = CopyStep {
        cluster,
        namespace: app.clone(),
        proposal: ProposalRef {
            operation,
            index: 0,
        },
        signer: DeviceKey::from(stranger.public()).into(),
        machine: ployz_core::MachineId::parse("a".repeat(32)).unwrap(),
        source: volume("app_data"),
        origin: ployz_core::VolumeOrigin::legacy(&volume("app_data")),
        point: ployz_core::SnapshotGuid::new(7),
        taken_at: Timestamp::now(),
        into: volume("app_restore"),
        birth: ployz_core::VolumeBirth {
            namespace: app.clone(),
            name: "restore".to_owned(),
            made_at: Timestamp::now(),
            nonce: ployz_core::BirthNonce::from_bytes([3; 16]),
        },
    };
    let body = Signed::unsigned(&step).unwrap().body;
    let bundle = ProposalApproval {
        cluster,
        environment: app,
        operation,
        steps: vec![ProposalStep {
            index: 0,
            kind: StepKind::Copy,
            digest: StepDigest::of(&body),
        }],
        issued_at: Timestamp::now(),
        expires_at: Timestamp::now().plus(60_000),
        signer: step.signer,
    };
    let signed = Signed::proposed(&step, Signed::unsigned(&bundle).unwrap()).unwrap();
    serde_json::to_value(signed).unwrap()
}

/// A forget of `app/web`, signed by a key this Machine does not trust.
fn stranger_forget(machine: &Enforcing) -> Value {
    use ployz_core::authority::{PayloadDigest, ServiceVersion, Timestamp};
    let stranger = Signer::from_seed(&[42; 32]);
    let version = ServiceVersion {
        cluster: SealingKey::load(&machine.data_dir)
            .unwrap()
            .unwrap()
            .public_key(),
        namespace: ployz_core::Namespace::parse("app").unwrap(),
        service: ployz_core::ServiceName::parse("web").unwrap(),
        digest: PayloadDigest::from_bytes([1; 32]),
        signed_at: Timestamp::now(),
        signer: DeviceKey::from(stranger.public()).into(),
        proposal: None,
        forget_older: true,
        parent: None,
    };
    serde_json::to_value(Signed::sign(&version, &stranger).unwrap()).unwrap()
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
        passkeys: Vec::new(),
        admitting_machines: vec![machine.machine.id],
        bootstrap: Vec::new(),
        issued_at: Timestamp::now(),
        expires_at: Timestamp::now().plus(60_000),
        signer: DeviceKey::from(stranger.public()).into(),
        proposal: None,
        grants: Vec::new(),
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
    want_of(name, spec(name, image), false)
}

/// A new Container of `resolved_spec`, with `image_prepared` as Cloud's runner sets it
/// after its own pull.
fn want_of(name: &str, resolved_spec: Value, image_prepared: bool) -> Value {
    json!({"state":"running", "rotation":"pending", "container":{"new":{
        "slot": {"namespace": "app", "service": name, "kind": "service_container",
                 "creation_key": "attacker-0001"},
        "resolved_spec": resolved_spec, "gen": 1, "image_prepared": image_prepared}}})
}

/// `want` as a drain sends it on the pinned route: answered once settled, its image
/// prepared, and pinned to one image ID.
fn pinned(mut want: Value) -> Value {
    let new = want
        .pointer_mut("/container/new")
        .and_then(Value::as_object_mut)
        .unwrap();
    new.insert("image_prepared".into(), true.into());
    new.insert("answer".into(), "settled".into());
    new.insert(
        "image_id".into(),
        format!("sha256:{}", "7".repeat(64)).into(),
    );
    want
}

/// The image pulls Docker was asked for.
fn pulls(machine: &Enforcing) -> usize {
    machine
        .fake
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, path)| {
            *method == axum::http::Method::POST && path.ends_with("/images/create")
        })
        .count()
}

/// A new Container whose spec mounts a Volume.
fn mounting_want() -> Value {
    let mounting = crate::docker::test_support::spec_with_sources(vec![
        crate::docker::test_support::provisioned_source("data", 1_073_741_824),
    ]);
    let volumes = serde_json::to_value(mounting)
        .unwrap()
        .pointer("/volumes")
        .cloned()
        .unwrap();
    let mut resolved = spec("db", "postgres:18");
    resolved
        .as_object_mut()
        .unwrap()
        .insert("volumes".into(), volumes);
    want_of("db", resolved, false)
}

/// One call to the Machine at `address`, owned, so a test can leave it waiting. `operator`
/// marks it an Operator's whether or not the Machine enforces, as Cloud's slot does.
async fn unary_at(
    address: String,
    path: &'static str,
    request: &RpcRequest,
    operator: bool,
) -> Answer {
    let channel = tonic::transport::Endpoint::from_shared(address)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut grpc = tonic::client::Grpc::new(channel);
    grpc.ready().await.unwrap();
    let mut call = Request::new(request.encode().unwrap());
    if operator {
        call.metadata_mut().insert(
            ployz_core::OPERATOR_SCOPE_HEADER,
            tonic::metadata::MetadataValue::from_static("1"),
        );
    }
    let result = grpc
        .unary(
            call,
            tonic::codegen::http::uri::PathAndQuery::from_static(path),
            tonic::codec::ProstCodec::<OpaquePayload, OpaquePayload>::default(),
        )
        .await;
    Answer::of(result.map(|response| response.into_inner().decode_response().unwrap()))
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
        Sample::Elsewhere => unreachable!("never sent"),
        Sample::Unparsed(error) => panic!("{error}"),
    }
}

/// The check a `Bounded` case names, run after its request was served.
async fn bounded(case: &Case, check: &str, machine: &Enforcing) {
    match check {
        "a following unsigned EnsureContainer create that mounts it is refused" => {
            let create = unary::<op::EnsureContainer>(json!({"want": mounting_want()}));
            let answer = send(machine, &create).await;
            assert!(
                matches!(&answer, Answer::Refused { capability: Some(name), .. }
                    if name == ployz_core::SIGNED_VERSION_CAPABILITY),
                "{case:?}: the create after it was not refused: {answer:?}"
            );
        }
        "a following unsigned EnsureContainer create that uses it pulls the image again itself" => {
            let seen = pulls(machine);
            let want = want_of("api", spec("api", "nginx:1.29-alpine"), true);
            let answer = send(
                machine,
                &unary::<op::EnsureContainer>(json!({"want": want})),
            )
            .await;
            assert!(
                !answer.for_authority() && pulls(machine) == seen + 1,
                "{case:?}: the create after it did not pull: {answer:?}"
            );
        }
        "the Machine pulls the image itself" => {
            assert_eq!(pulls(machine), 1, "{case:?}");
        }
        "the Machine pulls the image by digest and writes no name" => {
            assert_eq!(pulls(machine), 1, "{case:?}");
            assert_eq!(
                machine.fake.images.lock().unwrap().get("nginx:1.29-alpine"),
                None,
                "{case:?}: the tag was written"
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
        "restrictions Cloud wrote change nothing the Machine trusts" => {
            let record = machine.store.record();
            assert_eq!(
                record.devices().count(),
                1,
                "{case:?}: the approved Device stays"
            );
            assert!(
                record.removed_keys().is_empty() && record.removed_members().is_empty(),
                "{case:?}: nothing was removed"
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
    let frozen: BTreeSet<_> = UNIT_ONLY
        .iter()
        .map(|(capability, branch, _)| (*capability, *branch))
        .collect();
    assert_eq!(elsewhere, frozen, "the cases proven elsewhere changed");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

async fn check(case: &'static Case) -> Outcome {
    let machine = Enforcing::new().await;
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

#[path = "admission_golden_tests.rs"]
mod admission_golden;

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
        let machine = Enforcing::new().await;
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
    async fn turning_private_access_off_needs_no_approval() {
        let machine = Enforcing::new().await;
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
    }
}

mod revoked {
    //! A Device's call admitted when it arrived, whose key is removed while it waits for
    //! its turn: the turn it waited for finds the key gone and runs nothing.

    use super::*;
    use crate::storage::test_support::FakePlugin;
    use crate::volume_copy::{INSPECT_ROUTE, WRITER_ROUTE};

    /// This Machine's API served to the management connection `key` authenticated.
    async fn served_to(machine: &Enforcing, key: ployz_core::ClientKey) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let api = crate::machine_api::MachineApi::from_local(
            machine.service.clone().with_management_client(key),
        );
        tokio::spawn(Server::builder().serve_with_incoming(api, TcpListenerStream::new(listener)));
        address
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_device_removed_while_its_removal_drains_stops_and_deletes_nothing() {
        let machine = Enforcing::new().await;
        let id = running_container(&machine).await;
        machine
            .service
            .local
            .docker()
            .unwrap()
            .lifecycle()
            .start(&id)
            .await
            .unwrap();
        let device = DeviceKey::from(Signer::from_seed(&[7; 32]).public());
        let kept = DeviceKey::from(Signer::from_seed(&[9; 32]).public());
        machine
            .store
            .mutate(move |store| store.approve_device(kept).unwrap())
            .await
            .unwrap();
        let address = served_to(&machine, device.into()).await;
        let mut withdrawn = machine
            .service
            .local
            .docker()
            .unwrap()
            .subscribe_stopping_for_test();
        let turn = machine.store.admission_lock().write_owned().await;
        let removal = op::EnsureContainer::into_request(
            serde_json::from_value(json!({"want": {
                "state": "gone", "container": {"id": id}, "drain": 0,
                "anonymous_volumes": "remove",
            }}))
            .unwrap(),
        );
        let call = tokio::spawn(async move {
            unary_at(address, op::EnsureContainer::PATH, &removal, false).await
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            withdrawn.wait_for(|stopping| stopping.contains_key(&id)),
        )
        .await
        .expect("the removal was admitted and waits for its turn")
        .unwrap();
        let before = machine.fake.requests.lock().unwrap().len();
        machine
            .store
            .mutate(move |store| assert!(store.remove_by_code(device.code()).unwrap()))
            .await
            .unwrap();
        drop(turn);
        let answer = call.await.unwrap();
        let effects: Vec<_> = machine
            .fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .skip(before)
            .filter(|(method, path)| *method == http::Method::DELETE || path.ends_with("/stop"))
            .cloned()
            .collect();
        assert!(effects.is_empty(), "{answer:?} after {effects:?}");
        assert!(
            matches!(&answer, Answer::Refused { code, .. } if code == "Unauthenticated"),
            "{answer:?}"
        );
        machine.remove();
    }

    /// The Docker calls since `before` that `method` sent to a path ending in `suffix`.
    fn sent(machine: &Enforcing, before: usize, method: http::Method, suffix: &str) -> usize {
        machine
            .fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .skip(before)
            .filter(|(sent, path)| *sent == method && path.ends_with(suffix))
            .count()
    }

    /// Until Docker has seen a call since `before` that `method` sent to a path ending in
    /// `suffix`.
    async fn until_sent(machine: &Enforcing, before: usize, method: http::Method, suffix: &str) {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while sent(machine, before, method.clone(), suffix) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("Docker never saw {method} {suffix}"));
    }

    fn ensure(want: Value) -> RpcRequest {
        op::EnsureContainer::into_request(serde_json::from_value(json!({ "want": want })).unwrap())
    }

    /// What this Machine answers `want` with when the management connection `key`
    /// authenticated asks at Full access, as a Device's own connection does.
    async fn ensure_from(
        machine: &Enforcing,
        key: ployz_core::ClientKey,
        want: Value,
    ) -> Result<RpcResponse, Status> {
        use ployz_core::MachineRpc;
        machine
            .service
            .clone()
            .with_management_client(key)
            .ensure_container(Request::new(ensure(want).encode().unwrap()))
            .await
            .map(|reply| reply.into_inner().decode_response().unwrap())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_full_start_queued_while_its_device_slot_goes_is_held_to_the_operator_slot_left() {
        let machine = Enforcing::new().await;
        let device = DeviceKey::from(Signer::from_seed(&[7; 32]).public());
        let kept = DeviceKey::from(Signer::from_seed(&[9; 32]).public());
        machine
            .store
            .mutate(move |store| {
                store.approve_device(kept).unwrap();
                let cloud = ployz_core::ManagementClientLabel::cloud();
                store.stage_management_client(cloud, device.into()).unwrap();
                store.activate_management_client(device.into()).unwrap();
            })
            .await
            .unwrap();
        // Created at Full access, so the Container is kept under the Device's own key.
        let created = ensure_from(
            &machine,
            device.into(),
            new_want("api", "nginx:1.29-alpine"),
        )
        .await
        .unwrap();
        let ployz_core::EnsuredContainer::Ensured {
            now: ployz_core::ContainerNow::Present(observed),
            ..
        } = created.decode::<op::EnsureContainer>().unwrap()
        else {
            panic!("the Device's create made no Container");
        };
        let id = observed.container_id;
        let containers = machine.service.local.docker().unwrap();
        containers.lifecycle().stop(&id, None, None).await.unwrap();
        let before = machine.fake.requests.lock().unwrap().len();
        let turn = machine.store.admission_lock().write_owned().await;
        let call = tokio::spawn({
            let service = machine.service.clone();
            async move {
                use ployz_core::MachineRpc;
                let start = ensure(json!({
                    "state": "running", "rotation": "pending",
                    "container": {"existing": {"id": id}},
                }));
                service
                    .with_management_client(device.into())
                    .ensure_container(Request::new(start.encode().unwrap()))
                    .await
                    .map(|reply| reply.into_inner().decode_response().unwrap())
            }
        });
        until_sent(&machine, before, http::Method::GET, &format!("{id}/json")).await;
        machine
            .store
            .mutate(move |store| assert!(store.remove_by_code(device.code()).unwrap()))
            .await
            .unwrap();
        assert!(
            machine
                .store
                .record()
                .accepts_management_client(&device.into())
        );
        drop(turn);
        let answer = call.await.unwrap();
        assert_eq!(
            sent(&machine, before, http::Method::POST, "/start"),
            0,
            "{answer:?}"
        );
        assert!(
            matches!(&answer, Ok(response) if matches!(response.body, RpcResponseBody::Error(_)))
                || answer.is_err(),
            "{answer:?}"
        );
        machine.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_device_removed_between_its_stop_and_its_removal_deletes_nothing() {
        let machine = Enforcing::new().await;
        let id = running_container(&machine).await;
        machine
            .service
            .local
            .docker()
            .unwrap()
            .lifecycle()
            .start(&id)
            .await
            .unwrap();
        let device = DeviceKey::from(Signer::from_seed(&[7; 32]).public());
        let kept = DeviceKey::from(Signer::from_seed(&[9; 32]).public());
        machine
            .store
            .mutate(move |store| store.approve_device(kept).unwrap())
            .await
            .unwrap();
        let address = served_to(&machine, device.into()).await;
        let before = machine.fake.requests.lock().unwrap().len();
        let stopping = machine.fake.stop_gate.clone().write_owned().await;
        let removal = ensure(json!({
            "state": "gone", "container": {"id": id}, "drain": 0,
            "anonymous_volumes": "remove",
        }));
        let call = tokio::spawn(async move {
            unary_at(address, op::EnsureContainer::PATH, &removal, false).await
        });
        until_sent(&machine, before, http::Method::POST, "/stop").await;
        machine
            .store
            .mutate(move |store| assert!(store.remove_by_code(device.code()).unwrap()))
            .await
            .unwrap();
        drop(stopping);
        let answer = call.await.unwrap();
        assert_eq!(
            sent(&machine, before, http::Method::DELETE, ""),
            0,
            "{answer:?}"
        );
        assert!(
            matches!(&answer, Answer::Refused { code, .. } if code == "Unauthenticated"),
            "{answer:?}"
        );
        machine.remove();
    }

    /// The Device key `Enforcing` approves, held in Cloud's slot too, as a key Cloud also
    /// connects with is: losing its Device slot leaves it an Operator's.
    async fn device_in_two_slots(machine: &Enforcing) -> DeviceKey {
        let device = DeviceKey::from(Signer::from_seed(&[7; 32]).public());
        let kept = DeviceKey::from(Signer::from_seed(&[9; 32]).public());
        machine
            .store
            .mutate(move |store| {
                store.approve_device(kept).unwrap();
                let cloud = ployz_core::ManagementClientLabel::cloud();
                store.stage_management_client(cloud, device.into()).unwrap();
                store.activate_management_client(device.into()).unwrap();
            })
            .await
            .unwrap();
        device
    }

    /// The Device key `Enforcing` approves, in no other slot, beside another Device so its
    /// removal is not the last key's: removing it removes the key.
    async fn device_alone(machine: &Enforcing) -> DeviceKey {
        let kept = DeviceKey::from(Signer::from_seed(&[9; 32]).public());
        machine
            .store
            .mutate(move |store| store.approve_device(kept).unwrap())
            .await
            .unwrap();
        DeviceKey::from(Signer::from_seed(&[7; 32]).public())
    }

    /// What `call` answers when it arrived at Full access from `device`'s own connection
    /// and `device`'s Device slot went before it took its turn.
    async fn removed_before_its_turn(
        machine: &Enforcing,
        device: DeviceKey,
        call: impl std::future::Future<Output = Result<tonic::Response<OpaquePayload>, Status>>,
    ) -> Answer {
        let turn = machine.store.admission_lock().write_owned().await;
        removed_while(machine, device, turn, call).await
    }

    /// What `call` answers when it arrived at Full access from `device`'s own connection
    /// and `device`'s Device slot went while `held` kept it queued.
    async fn removed_while(
        machine: &Enforcing,
        device: DeviceKey,
        held: impl Sized,
        call: impl std::future::Future<Output = Result<tonic::Response<OpaquePayload>, Status>>,
    ) -> Answer {
        tokio::pin!(call);
        if let std::task::Poll::Ready(answered) = futures_util::poll!(&mut call) {
            panic!(
                "answered before the lock it queues on: {:?}",
                Answer::of(answered.map(|reply| reply.into_inner().decode_response().unwrap()))
            );
        }
        machine
            .store
            .mutate(move |store| assert!(store.remove_by_code(device.code()).unwrap()))
            .await
            .unwrap();
        drop(held);
        Answer::of(
            call.await
                .map(|reply| reply.into_inner().decode_response().unwrap()),
        )
    }

    /// [`removed_before_its_turn`] for a key that keeps Cloud's slot.
    async fn downgraded_before_its_turn(
        machine: &Enforcing,
        device: DeviceKey,
        call: impl std::future::Future<Output = Result<tonic::Response<OpaquePayload>, Status>>,
    ) -> Answer {
        let answer = removed_before_its_turn(machine, device, call).await;
        assert!(
            machine
                .store
                .record()
                .accepts_management_client(&device.into())
        );
        answer
    }

    fn request<T: Rpc>(payload: Value) -> Request<OpaquePayload>
    where
        T::Request: serde::de::DeserializeOwned,
    {
        Request::new(
            T::into_request(serde_json::from_value(payload).unwrap())
                .encode()
                .unwrap(),
        )
    }

    /// Whether Docker holds an image under `tag`, which a tag pull writes.
    fn tagged(machine: &Enforcing, tag: &str) -> bool {
        machine.fake.images.lock().unwrap().contains_key(tag)
    }

    #[track_caller]
    fn refused_naming(answer: &Answer, capability: &str) {
        assert!(
            matches!(answer, Answer::Refused { capability: Some(named), .. } if named == capability),
            "{answer:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_tag_pull_queued_while_its_device_slot_goes_writes_no_tag() {
        use ployz_core::MachineRpc;
        let machine = Enforcing::new().await;
        let device = device_in_two_slots(&machine).await;
        let caller = machine
            .service
            .clone()
            .with_management_client(device.into());
        let pull = request::<op::PullImage>(json!({
            "image": "redis:7-alpine", "policy": "always", "registry_auth": null,
        }));
        let answer = downgraded_before_its_turn(&machine, device, caller.pull_image(pull)).await;
        assert!(!tagged(&machine, "redis:7-alpine"), "{answer:?}");
        refused_naming(&answer, ployz_core::PULL_IMAGE_CAPABILITY);
        machine.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_free_pull_queued_while_its_device_slot_goes_pulls_as_an_operator() {
        use ployz_core::MachineRpc;
        let machine = Enforcing::new().await;
        let device = device_in_two_slots(&machine).await;
        let caller = machine
            .service
            .clone()
            .with_management_client(device.into());
        let pull = request::<op::PullFreeImage>(json!({
            "image": "redis:7-alpine", "policy": "always",
        }));
        let before = machine.fake.requests.lock().unwrap().len();
        let answer =
            downgraded_before_its_turn(&machine, device, caller.pull_free_image(pull)).await;
        assert!(matches!(answer, Answer::Served), "{answer:?}");
        assert!(!tagged(&machine, "redis:7-alpine"), "{answer:?}");
        // Only an Operator's pull asks the registry what the tag names, then pulls that.
        let resolved = machine
            .fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .skip(before)
            .filter(|(method, path)| {
                *method == http::Method::GET && path.contains("/distribution/redis:7-alpine/")
            })
            .count();
        assert_eq!(resolved, 1, "{answer:?}");
        machine.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_peer_tag_pull_queued_while_its_device_slot_goes_is_refused() {
        use ployz_core::MachineRpc;
        let machine = Enforcing::new().await;
        let device = device_in_two_slots(&machine).await;
        let caller = machine
            .service
            .clone()
            .with_management_client(device.into());
        let pull = request::<op::PullImageFromMachine>(json!({
            "pull": {"mode": "reference", "image": "redis:7-alpine"},
            "source": {"management_address": "fd00::1", "port": 7570},
            "platform": "linux/amd64",
        }));
        let answer =
            downgraded_before_its_turn(&machine, device, caller.pull_image_from_machine(pull))
                .await;
        refused_naming(&answer, ployz_core::PULL_IMAGE_FROM_MACHINE_CAPABILITY);
        machine.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture")]
    async fn a_hook_start_queued_while_its_device_slot_goes_runs_no_hook_again() {
        use ployz_core::MachineRpc;
        let machine = Enforcing::new().await;
        let mut hooked = spec("migrate", "nginx:1.29-alpine");
        hooked["pre_deploy"] = json!({ "command": ["migrate"] });
        let spec: ployz_core::ResolvedServiceSpec = serde_json::from_value(hooked).unwrap();
        let hook = machine
            .service
            .local
            .create_container(
                ployz_core::ContainerKind::PreDeployHook,
                &ployz_core::Namespace::parse("app").unwrap(),
                &spec,
                None,
                None,
                None,
            )
            .await
            .unwrap()
            .container_id;
        let device = device_in_two_slots(&machine).await;
        let caller = machine
            .service
            .clone()
            .with_management_client(device.into());
        let before = machine.fake.requests.lock().unwrap().len();
        let start = request::<op::EnsureContainer>(json!({"want": {
            "state": "running", "rotation": "pending",
            "container": {"existing": {"id": hook}},
        }}));
        let answer =
            downgraded_before_its_turn(&machine, device, caller.ensure_container(start)).await;
        assert_eq!(
            sent(&machine, before, http::Method::POST, "/start"),
            0,
            "{answer:?}"
        );
        refused_naming(&answer, ployz_core::SIGNED_VERSION_CAPABILITY);
        machine.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture")]
    async fn a_full_create_queued_while_its_device_slot_goes_pulls_nothing() {
        use ployz_core::MachineRpc;
        let machine = Enforcing::new().await;
        let device = device_in_two_slots(&machine).await;
        let caller = machine
            .service
            .clone()
            .with_management_client(device.into());
        let mut resolved = spec("cache", "redis:7-alpine");
        resolved["container"]["pull_policy"] = json!("always");
        let create =
            request::<op::EnsureContainer>(json!({ "want": want_of("cache", resolved, false) }));
        let before = machine.fake.requests.lock().unwrap().len();
        let answer =
            downgraded_before_its_turn(&machine, device, caller.ensure_container(create)).await;
        assert_eq!(
            sent(&machine, before, http::Method::POST, "/images/create"),
            0,
            "{answer:?}"
        );
        assert!(answer.for_authority(), "{answer:?}");
        machine.remove();
    }

    fn answered(result: Result<tonic::Response<OpaquePayload>, Status>) -> Answer {
        Answer::of(result.map(|reply| reply.into_inner().decode_response().unwrap()))
    }

    /// `machine`'s service with a fake volume plugin whose Volume is a Mirror at snapshot
    /// 7, and whose Writer route promotes it, served from a directory that lives as long as
    /// the guard: a socket path under the Machine's own would be too long.
    fn with_mirror(
        machine: &Enforcing,
        plugin: &FakePlugin,
    ) -> (MachineService, tempfile::TempDir) {
        let snapshot = json!({"name":"f-1", "guid":"7", "created_unix_seconds":1_700_000_000});
        plugin.reply(
            INSPECT_ROUTE,
            json!({"Ok": {"kind":"slot", "slot":"1", "readonly":true,
                "newest":snapshot, "resume_token":null, "pull":null}}),
        );
        plugin.reply(
            WRITER_ROUTE,
            json!({"Ok": {"kind":"root", "readonly":false, "newest":snapshot, "epoch":1,
                "seed":false}}),
        );
        let directory = tempfile::tempdir().unwrap();
        let (socket, _server) = plugin.serve(directory.path());
        (
            machine.service.clone().with_volume_plugin(socket),
            directory,
        )
    }

    /// A promote of the Mirror [`with_mirror`] serves, with `start` when one is given.
    fn promote(start: Option<Value>) -> Request<OpaquePayload> {
        request::<op::SetVolumeCopy>(json!({"name":"app_data",
            "expect":{"kind":"mirror", "newest":"7", "slot":"1"},
            "want":{"kind":"writer", "at":"7", "epoch":1, "start":start}}))
    }

    /// Until `asked` counts a request for the switch past `before`.
    async fn until_switch_asked(asked: &mut tokio::sync::watch::Receiver<usize>, before: usize) {
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            asked.wait_for(|asked| *asked > before),
        )
        .await
        .expect("the call asks for the switch")
        .unwrap();
    }

    /// A Writer whose start mounts no Volume it promotes, which a Full caller may send and
    /// an Operator may not, queued on the switch while the key loses its Device slot.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_promote_queued_while_its_device_slot_goes_promotes_nothing_an_operator_may_not() {
        use ployz_core::MachineRpc;
        let machine = Enforcing::new().await;
        let device = device_in_two_slots(&machine).await;
        let plugin = FakePlugin::default();
        let (served, _plugin_dir) = with_mirror(&machine, &plugin);
        let caller = served.with_management_client(device.into());
        let local = machine.service.local.clone();
        let held = local.lock_switch().await.unwrap();
        let mut asked = local.switch_asked();
        let before = *asked.borrow();
        let start = json!({"kind":"create", "namespace":"app",
            "resolved_spec":spec("api", "nginx:1.29-alpine"), "creation_key":"move-1",
            "gen":2, "rotation":"pending"});
        let call =
            tokio::spawn(
                async move { answered(caller.set_volume_copy(promote(Some(start))).await) },
            );
        until_switch_asked(&mut asked, before).await;
        machine
            .store
            .mutate(move |store| assert!(store.remove_by_code(device.code()).unwrap()))
            .await
            .unwrap();
        drop(held);
        let answer = call.await.unwrap();
        assert!(
            !plugin
                .routes_called()
                .iter()
                .any(|route| route == WRITER_ROUTE),
            "{answer:?}"
        );
        assert!(answer.for_authority(), "{answer:?}");
        machine.remove();
    }

    /// A floor raise takes no turn and no switch, so it checks its caller under the floor
    /// lock it queued on.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_floor_raise_queued_while_its_device_goes_raises_nothing() {
        use ployz_core::MachineRpc;
        let machine = Enforcing::new().await;
        let device = device_alone(&machine).await;
        let caller = machine
            .service
            .clone()
            .with_management_client(device.into());
        let floor = request::<op::SetVolumeCopy>(json!({"name":"app_data",
            "expect":{"kind":"none"}, "want":{"kind":"floor"}, "gen":5}));
        let lifecycle = machine.service.local.docker().unwrap().lifecycle();
        let held = lifecycle.floor_turn().await;
        let answer = removed_while(&machine, device, held, caller.set_volume_copy(floor)).await;
        let below = lifecycle
            .floor_turn()
            .await
            .check(&"app_data".parse().unwrap(), ployz_core::VolumeGen::new(4))
            .await
            .unwrap();
        assert_eq!(below, Ok(()), "{answer:?}");
        assert!(
            matches!(&answer, Answer::Refused { code, .. } if code == "Unauthenticated"),
            "{answer:?}"
        );
        machine.remove();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_policy_edit_queued_while_its_device_goes_changes_nothing() {
        use ployz_core::MachineRpc;
        let machine = Enforcing::new().await;
        let device = device_alone(&machine).await;
        let caller = machine
            .service
            .clone()
            .with_management_client(device.into());
        let edit = request::<op::UpdateMachine>(json!({"update": {"accepts_builds": false}}));
        let answer = removed_before_its_turn(&machine, device, caller.update_machine(edit)).await;
        assert!(
            machine.store.record().machine().unwrap().accepts_builds,
            "{answer:?}"
        );
        assert!(
            matches!(&answer, Answer::Refused { code, .. } if code == "Unauthenticated"),
            "{answer:?}"
        );
        machine.remove();
    }

    /// A hook this Server's own socket created and never started, in the slot a Device's
    /// create names with the same spec, sent at Full access while the Device slot goes.
    #[tokio::test(flavor = "multi_thread")]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture")]
    async fn a_create_queued_while_its_device_slot_goes_adopts_no_hook_another_key_made() {
        use ployz_core::MachineRpc;
        let machine = Enforcing::new().await;
        let mut hooked = spec("migrate", "nginx:1.29-alpine");
        hooked["pre_deploy"] = json!({ "command": ["migrate"] });
        let resolved: ployz_core::ResolvedServiceSpec =
            serde_json::from_value(hooked.clone()).unwrap();
        let mut want = want_of("migrate", hooked, false);
        want["container"]["new"]["slot"]["kind"] = json!("pre_deploy_hook");
        machine
            .service
            .local
            .create_container(
                ployz_core::ContainerKind::PreDeployHook,
                &ployz_core::Namespace::parse("app").unwrap(),
                &resolved,
                Some("attacker-0001".to_owned()),
                None,
                None,
            )
            .await
            .unwrap();
        let device = device_in_two_slots(&machine).await;
        let caller = machine
            .service
            .clone()
            .with_management_client(device.into());
        let before = machine.fake.requests.lock().unwrap().len();
        let create = request::<op::EnsureContainer>(json!({ "want": want }));
        let answer =
            downgraded_before_its_turn(&machine, device, caller.ensure_container(create)).await;
        assert_eq!(
            sent(&machine, before, http::Method::POST, "/start"),
            0,
            "{answer:?}"
        );
        assert!(answer.for_authority(), "{answer:?}");
        machine.remove();
    }

    /// Cloud's key handed over to its next key while the old key's promote runs inside the
    /// switch: the handover retires the old key only once that step ends.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_handover_waits_out_the_retiring_keys_volume_step() {
        use ployz_core::MachineRpc;
        let machine = Enforcing::new().await;
        let retiring: ployz_core::ClientKey =
            DeviceKey::from(Signer::from_seed(&[11; 32]).public()).into();
        let next: ployz_core::ClientKey =
            DeviceKey::from(Signer::from_seed(&[12; 32]).public()).into();
        machine
            .store
            .mutate(move |store| {
                let cloud = ployz_core::ManagementClientLabel::cloud();
                store
                    .stage_management_client(cloud.clone(), retiring)
                    .unwrap();
                store.activate_management_client(retiring).unwrap();
                store.stage_management_client(cloud, next).unwrap();
            })
            .await
            .unwrap();
        let plugin = FakePlugin::default();
        let (served, _plugin_dir) = with_mirror(&machine, &plugin);
        let caller = served.with_management_client(retiring);
        let promoting = plugin.hold(WRITER_ROUTE).await;
        let step =
            tokio::spawn(async move { answered(caller.set_volume_copy(promote(None)).await) });
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !plugin
                .routes_called()
                .iter()
                .any(|route| route == WRITER_ROUTE)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the old key's promote reaches the plugin");
        let local = machine.service.local.clone();
        let mut asked = local.switch_asked();
        let before = *asked.borrow();
        let mut handover =
            tokio::spawn(async move { local.activate_management_client(next).await });
        tokio::select! {
            done = &mut handover => {
                panic!("the handover ended while the old key's promote still runs: {done:?}");
            }
            () = until_switch_asked(&mut asked, before) => {}
        }
        drop(promoting);
        let promoted = step.await.unwrap();
        assert!(matches!(promoted, Answer::Served), "{promoted:?}");
        handover.await.unwrap().unwrap();
        assert!(!machine.store.record().accepts_management_client(&retiring));
        machine.remove();
    }

    /// A Full caller's Container in the Private Access router's slot, whose spec mounts a
    /// Provisioned Volume. Its ensure holds the doors lock, which a Volume step inside the
    /// switch takes too, so it must not wait for the switch.
    #[tokio::test(flavor = "multi_thread")]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture")]
    async fn a_reserved_slot_mounting_a_volume_is_refused_before_the_switch() {
        use ployz_core::MachineRpc;
        let machine = Enforcing::new().await;
        let mut mounting =
            serde_json::to_value(crate::docker::test_support::spec_with_sources(vec![
                crate::docker::test_support::provisioned_source("data", 1_073_741_824),
            ]))
            .unwrap();
        mounting["name"] = json!("tailscale");
        let want = json!({"state":"running", "rotation":"pending", "container":{"new":{
            "slot": {"namespace": "ployz-system", "service": "tailscale",
                     "kind": "service_container", "creation_key": "attacker-0001"},
            "resolved_spec": mounting, "gen": 1}}});
        let local = machine.service.local.clone();
        let held = local.lock_switch().await.unwrap();
        let mut asked = local.switch_asked();
        let before = *asked.borrow();
        let service = machine.service.clone();
        let mut ensure = tokio::spawn(async move {
            let create = request::<op::EnsureContainer>(json!({ "want": want }));
            answered(service.ensure_container(create).await)
        });
        let answer = tokio::select! {
            answer = &mut ensure => answer.unwrap(),
            () = until_switch_asked(&mut asked, before) => {
                panic!("the reserved ensure asks for the switch while it holds the doors lock");
            }
        };
        drop(held);
        assert!(
            matches!(&answer, Answer::Refused { code, .. } if code == "refused"),
            "{answer:?}"
        );
        machine.remove();
    }
}
