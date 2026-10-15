use super::*;
use ployz_core::{
    AnswerWhen, ContractDescription, ImageIngestDestination, ImageIngestOpened, ImageSummary,
    Machine, MachineImages, MachineList, MachineName, MachineObservation, OpaquePayload,
    PROTOCOL_MAJOR, RpcRequestBody, RpcResponse, ServiceMode, WireGuardPublicKey,
};
use tonic::{Request, Response, Status};

#[derive(Clone, Copy, PartialEq)]
enum Silent {
    Ingest,
    PeerPull,
    RegistryPull,
    ClusterDown,
    NeverSettles,
}

fn holder(hex: char) -> Machine {
    Machine {
        id: machine(hex),
        name: MachineName::parse(format!("machine-{hex}")).unwrap(),
        subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
            .parse()
            .unwrap(),
        public_key: WireGuardPublicKey([hex as u8; 32]),
        public_ip: None,
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        advertised_endpoints: Vec::new(),
        runtime: ployz_core::MachineRuntime {
            architecture: "x86_64".into(),
            ..Default::default()
        },
        build_concurrency: None,
    }
}

fn images(image: &str, held: bool) -> MachineImages {
    MachineImages {
        containerd_store: true,
        images: held
            .then(|| ImageSummary {
                id: format!("sha256:{}", "4".repeat(64)),
                repo_tags: vec![image.into()],
                created: 0,
                size: 1,
                containers: 0,
                platforms: vec!["linux/amd64".into()],
                last_tagged: None,
            })
            .into_iter()
            .collect(),
        docker_root: None,
    }
}

async fn destination_and_holder(
    image: &'static str,
    silent: Silent,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let powered_off = || {
        ployz_core::rpc::target_unanswered_status(&std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "deadline has elapsed",
        ))
    };
    crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| async move {
        let target = rpc
            .metadata()
            .get("machine")
            .map(|value| MachineId::parse(value.to_str().unwrap()).unwrap());
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "the fixture answers only what an image pull and a start send"
        )]
        let response = match rpc.into_inner().decode_request().unwrap().body {
            RpcRequestBody::DescribeContract(_) => RpcResponse::from(ContractDescription {
                machine_id: machine('1'),
                protocol_major: PROTOCOL_MAJOR,
                daemon_version: "fixture".into(),
                capabilities: Default::default(),
            }),
            RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                enrollment: None,
                machines: ['1', '2']
                    .map(|hex| MachineObservation::new(holder(hex), MembershipObservation::Up))
                    .into(),
            }),
            RpcRequestBody::ListImages(_) => {
                RpcResponse::from(images(image, target == Some(machine('2'))))
            }
            RpcRequestBody::EnsureImageIngest(_) if silent == Silent::Ingest => {
                return Err(powered_off());
            }
            RpcRequestBody::EnsureImageIngest(_) => RpcResponse::from(ImageIngestOpened {
                destination: ImageIngestDestination {
                    management_address: holder('2').management_address(),
                    port: ployz_core::UNREGISTRY_PORT,
                },
            }),
            RpcRequestBody::PullImageFromMachine(_) if silent == Silent::PeerPull => {
                return Err(powered_off());
            }
            RpcRequestBody::PullImage(_) if silent == Silent::RegistryPull => {
                return Err(powered_off());
            }
            RpcRequestBody::PullImage(_) if silent == Silent::ClusterDown => {
                return Err(Status::unavailable("Cluster is not available"));
            }
            RpcRequestBody::ListContainers(_) | RpcRequestBody::EnsureContainer(_)
                if silent == Silent::NeverSettles =>
            {
                return std::future::pending().await;
            }
            other => panic!("unexpected request: {other:?}"),
        };
        Ok(Response::new(response.encode().unwrap()))
    })
    .await
}

fn silence(machine_id: MachineId, transport: &str) -> String {
    format!("Machine {machine_id} did not answer: {transport}")
}

#[tokio::test(start_paused = true)]
async fn a_peer_that_cannot_open_ingest_is_named() {
    let image = "alpine:3.23.3";
    let (client, server) = destination_and_holder(image, Silent::Ingest).await;

    let error = crate::image::ensure_cluster_image(
        &client,
        &machine('1'),
        image,
        ployz_core::PullPolicy::Missing,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, silence(machine('2'), "deadline has elapsed"));
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_destination_silent_during_a_peer_pull_is_named() {
    let image = "alpine:3.23.3";
    let (client, server) = destination_and_holder(image, Silent::PeerPull).await;

    let error = crate::image::ensure_cluster_image(
        &client,
        &machine('1'),
        image,
        ployz_core::PullPolicy::Missing,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, silence(machine('1'), "deadline has elapsed"));
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_destination_silent_during_its_registry_pull_is_named() {
    let (client, server) = destination_and_holder("alpine:3.23.3", Silent::RegistryPull).await;
    let mut service = spec(None, None, None);
    service.container.pull_policy = ployz_core::PullPolicy::Always;

    let error = MachineOperations::ensure_image(&client, &machine('1'), &service)
        .await
        .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, silence(machine('1'), "deadline has elapsed"));
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_destination_that_answers_unavailable_keeps_its_words() {
    let (client, server) = destination_and_holder("alpine:3.23.3", Silent::ClusterDown).await;
    let mut service = spec(None, None, None);
    service.container.pull_policy = ployz_core::PullPolicy::Always;

    let error = MachineOperations::ensure_image(&client, &machine('1'), &service)
        .await
        .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, "Cluster is not available");
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_machine_that_never_settles_a_container_is_named() {
    let (client, server) = destination_and_holder("alpine:3.23.3", Silent::NeverSettles).await;

    let error = MachineOperations::ensure_container(
        &client,
        &machine('1'),
        ContainerWant::Running {
            container: RunTarget::Existing(ContainerLocator::Id(container('a'))),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(
        error.message,
        silence(machine('1'), "target Machine RPC timed out")
    );
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_machine_silent_while_a_global_start_checks_its_slot_is_named() {
    let (client, server) = destination_and_holder("alpine:3.23.3", Silent::NeverSettles).await;
    let mut global = spec(None, None, None);
    global.mode = ServiceMode::Global;

    let error = MachineOperations::ensure_container(
        &client,
        &machine('1'),
        ContainerWant::Running {
            container: RunTarget::New(NewContainer {
                slot: slot_key(
                    &test_namespace(),
                    ContainerKind::ServiceContainer,
                    crate::cluster::global_creation_key(&global),
                ),
                resolved_spec: global,
                deployment_id: None,
                registry_auth: None,
                image_prepared: true,
                answer: AnswerWhen::Started,
            }),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(
        error.message,
        silence(machine('1'), "target Machine RPC timed out")
    );
    server.abort();
}
