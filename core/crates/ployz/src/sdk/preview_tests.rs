use std::collections::BTreeMap;
use std::sync::Mutex;

use ployz_core::authority::RuleId;
use ployz_core::{
    DockerVolumeName, MachineRpcServer, OpaquePayload, PreviewsHeld, RpcRequestBody, VolumeRemoved,
};
use ployz_testkit::fake_machine::{Fake, FakeMachine, reply, target};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status, transport::Server};

use super::*;
use crate::sdk::test_support::{self as support, DeployService};

/// Machines that hold previews open, answer a Close by forgetting the preview, and keep
/// Volumes of another driver.
#[derive(Clone, Default)]
struct Held {
    open: Arc<Mutex<BTreeMap<MachineId, Vec<Namespace>>>>,
    closing: Arc<Mutex<BTreeMap<MachineId, Vec<Namespace>>>>,
    closed: Arc<Mutex<Vec<(MachineId, Namespace)>>>,
    removed: Arc<Mutex<Vec<(MachineId, DockerVolumeName)>>>,
    kept: Vec<DockerVolumeName>,
}

struct Previews {
    cluster: DeployService,
    held: Held,
}

#[tonic::async_trait]
impl FakeMachine for Previews {
    async fn describe_contract(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.cluster.describe_contract(request).await
    }

    async fn list_machines(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.cluster.list_machines(request).await
    }

    async fn list_previews(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let machine = target(&request)?;
        let open = self.held.open.lock().unwrap().get(&machine).cloned();
        let closing = self.held.closing.lock().unwrap().get(&machine).cloned();
        reply(PreviewsHeld {
            open: open.unwrap_or_default(),
            closing: closing.unwrap_or_default(),
        })
    }

    async fn close_preview(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let machine = target(&request)?;
        let RpcRequestBody::ClosePreview(close) =
            request.into_inner().decode_request().unwrap().body
        else {
            return Err(Status::invalid_argument("expected close_preview"));
        };
        for held in [&self.held.open, &self.held.closing] {
            if let Some(held) = held.lock().unwrap().get_mut(&machine) {
                held.retain(|namespace| *namespace != close.namespace);
            }
        }
        self.held
            .closed
            .lock()
            .unwrap()
            .push((machine, close.namespace));
        reply(PreviewClosed {
            kept: self.held.kept.clone(),
            ..PreviewClosed::default()
        })
    }

    async fn remove_volume(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let machine = target(&request)?;
        let RpcRequestBody::RemoveVolume(remove) =
            request.into_inner().decode_request().unwrap().body
        else {
            return Err(Status::invalid_argument("expected remove_volume"));
        };
        self.held
            .removed
            .lock()
            .unwrap()
            .push((machine, remove.name));
        reply(VolumeRemoved {})
    }
}

/// A Session to a Cluster of `machines`, each holding what `held` says.
async fn cluster(
    machines: Vec<MachineObservation>,
    held: Held,
) -> (
    Session,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let previews = Previews {
        cluster: DeployService::empty().with_machines(machines),
        held,
    };
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = tcp.local_addr().unwrap();
    let server = tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(Fake(previews)))
            .serve_with_incoming(TcpListenerStream::new(tcp)),
    );
    let session = super::super::connect_connections(
        vec![Connection::tcp(address)],
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap();
    (session, server)
}

fn preview_on(machine: &MachineObservation, nonce: u8) -> Namespace {
    PreviewNamespace::mint(
        &RuleId::from_bytes([1; 16]),
        &machine.machine.id,
        [nonce; 4],
    )
    .namespace()
    .clone()
}

fn volume(machine: &MachineObservation, name: String) -> DockerVolumeId {
    DockerVolumeId {
        machine_id: machine.machine.id,
        name: DockerVolumeName::parse(name).unwrap(),
    }
}

#[tokio::test]
async fn a_sweep_closes_only_the_previews_the_store_does_not_hold_open() {
    let (a, b) = (support::machine('a', "one"), support::machine('b', "two"));
    let (open, closed, unknown) = (preview_on(&a, 1), preview_on(&a, 2), preview_on(&b, 3));
    let held = Held::default();
    held.open.lock().unwrap().extend([
        (a.machine.id, vec![open.clone(), closed.clone()]),
        (b.machine.id, vec![unknown.clone()]),
    ]);
    let (session, server) = cluster(vec![a.clone(), b.clone()], held.clone()).await;
    let client = session.client().unwrap();

    let store_holds = vec![open.clone()];
    let swept = sweep(&client, &[a.clone(), b.clone()], |asked| {
        assert_eq!(asked, [open.clone(), closed.clone(), unknown.clone()]);
        async move { Ok(store_holds) }
    })
    .await
    .unwrap();

    let both = [(a.machine.id, closed), (b.machine.id, unknown)];
    assert_eq!(*held.closed.lock().unwrap(), both);
    assert_eq!(swept.closed.len(), 2, "{swept:?}");
    assert_eq!(
        held.open.lock().unwrap().get(&a.machine.id),
        Some(&vec![open]),
        "the open preview was closed"
    );
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn a_sweep_finishes_a_close_its_server_left_halfway() {
    let machine = support::machine('a', "one");
    let stuck = preview_on(&machine, 1);
    let held = Held::default();
    held.closing
        .lock()
        .unwrap()
        .insert(machine.machine.id, vec![stuck.clone()]);
    let (session, server) = cluster(vec![machine.clone()], held.clone()).await;
    let client = session.client().unwrap();

    let swept = sweep(&client, std::slice::from_ref(&machine), |_| async {
        Ok(Vec::new())
    })
    .await
    .unwrap();

    assert_eq!(*held.closed.lock().unwrap(), [(machine.machine.id, stuck)]);
    assert_eq!(swept.closed.len(), 1, "{swept:?}");
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn a_removed_preview_is_closed_on_its_home_and_its_volumes_reported_from_that_close() {
    let (home, other) = (support::machine('a', "one"), support::machine('b', "two"));
    let namespace = preview_on(&home, 1);
    let held = Held {
        kept: vec![DockerVolumeName::parse(format!("{namespace}_cache")).unwrap()],
        ..Held::default()
    };
    let (session, server) = cluster(vec![home.clone(), other.clone()], held.clone()).await;
    let deletes = vec![
        volume(&home, format!("{namespace}_data")),
        volume(&home, format!("{namespace}_cache")),
        volume(&other, format!("{namespace}_data")),
    ];

    let removals = session.close_preview(&namespace, deletes).await;

    assert_eq!(
        *held.closed.lock().unwrap(),
        [(home.machine.id, namespace.clone())]
    );
    let outcomes: Vec<_> = removals
        .iter()
        .map(|removal| (removal.id.machine_id, &removal.outcome))
        .collect();
    let [data, cache, elsewhere] = outcomes.as_slice() else {
        panic!("one outcome per Volume: {outcomes:?}");
    };
    assert_eq!(*data, (home.machine.id, &VolumeRemovalOutcome::Removed));
    assert!(
        matches!(cache.1, VolumeRemovalOutcome::Failed { .. }),
        "a Volume the Close kept is not removed: {outcomes:?}"
    );
    assert_eq!(
        *elsewhere,
        (other.machine.id, &VolumeRemovalOutcome::Removed)
    );
    let data = DockerVolumeName::parse(format!("{namespace}_data")).unwrap();
    assert_eq!(
        *held.removed.lock().unwrap(),
        [(other.machine.id, data)],
        "only the copy off its home is removed one by one"
    );
    session.close().await;
    server.abort();
}

#[tokio::test]
async fn a_preview_whose_home_left_the_cluster_leaves_its_volumes_to_retry() {
    let (gone, other) = (support::machine('a', "one"), support::machine('b', "two"));
    let namespace = preview_on(&gone, 1);
    let held = Held::default();
    let (session, server) = cluster(vec![other], held.clone()).await;

    let removals = session
        .close_preview(&namespace, vec![volume(&gone, format!("{namespace}_data"))])
        .await;

    assert!(held.closed.lock().unwrap().is_empty());
    let outcomes: Vec<_> = removals.iter().map(|removal| &removal.outcome).collect();
    assert_eq!(outcomes, [&VolumeRemovalOutcome::Omitted]);
    session.close().await;
    server.abort();
}

#[test]
fn a_preview_is_placed_only_on_a_server_taking_services_that_can_close_it() {
    let capable = |hex, name| {
        let mut machine = support::machine(hex, name);
        machine.machine.runtime.capabilities = [PREVIEW_HOME_CAPABILITY, CLOSE_PREVIEW_CAPABILITY]
            .into_iter()
            .map(Into::into)
            .collect();
        machine
    };
    let home = capable('a', "one");
    let mut no_services = capable('b', "two");
    no_services.machine.accepts_services = false;
    let mut leaving = capable('c', "three");
    leaving.machine.runtime.leaving = true;
    let mut older = capable('d', "four");
    older
        .machine
        .runtime
        .capabilities
        .retain(|capability| capability != CLOSE_PREVIEW_CAPABILITY);
    let unclaimed = support::machine('e', "five");

    let found = homes(&[home.clone(), no_services, leaving, older, unclaimed]);

    assert_eq!(found, [home.machine.id]);
}
