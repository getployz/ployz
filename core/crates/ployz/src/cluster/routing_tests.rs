//! Where each kind of send goes: which daemon answers it and whether it carries the target
//! header. A fleet of three fake daemons, `a` the entry and `b` reached by a Full connection,
//! logs every request it receives, so a change to how the Client routes shows as a different log.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use ployz_core::{
    InspectRequest, LeaveRequest, ListMachinesRequest, ONE_TARGET_HEADER, OpaquePayload,
    RpcRequestBody, SetDeviceEnforcementRequest,
};
use tonic::{Request, Response, Status, Streaming, transport::Channel};

use super::*;

/// What one daemon received: its name, the command (`stream` for any stream), and the target
/// header it carried.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Seen {
    daemon: char,
    call: String,
    target: Option<String>,
}

fn at(daemon: char, call: &str, target: Option<&str>) -> Seen {
    Seen {
        daemon,
        call: call.to_owned(),
        target: target.map(str::to_owned),
    }
}

type Log = Arc<Mutex<Vec<Seen>>>;

const STREAMS: &[&str] = &[
    op::Build::PATH,
    op::Exec::PATH,
    op::TailLogs::PATH,
    op::LogHistory::PATH,
    op::RuntimeWatch::PATH,
];

fn machine_id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

fn target_header(metadata: &tonic::metadata::MetadataMap) -> Option<String> {
    metadata
        .get(ONE_TARGET_HEADER)
        .map(|value| value.to_str().unwrap().to_owned())
}

/// A daemon that confirms as machine `id`, logs everything else it receives, and refuses it.
async fn daemon(id: char, log: &Log) -> Channel {
    let unary_log = log.clone();
    let stream_log = log.clone();
    let (client, _server) = crate::connect::test_support::rpc_routes(
        move |rpc: Request<OpaquePayload>| {
            let log = unary_log.clone();
            async move {
                let target = target_header(rpc.metadata());
                let body = rpc.into_inner().decode_request().unwrap().body;
                log.lock()
                    .unwrap()
                    .push(at(id, body.command(), target.as_deref()));
                if let RpcRequestBody::DescribeContract(_) = body {
                    let described =
                        ployz_core::RpcResponse::from(ployz_core::ContractDescription {
                            attest: None,
                            machine_id: machine_id(id),
                            protocol_major: ployz_core::PROTOCOL_MAJOR,
                            daemon_version: "test".into(),
                            capabilities: Default::default(),
                            sealing: None,
                        });
                    Ok(Response::new(described.encode().unwrap()))
                } else {
                    Err(Status::unimplemented("pinned"))
                }
            }
        },
        STREAMS,
        move |rpc: Request<Streaming<OpaquePayload>>| {
            stream_log.lock().unwrap().push(at(
                id,
                "stream",
                target_header(rpc.metadata()).as_deref(),
            ));
            async move {
                Ok(Response::new(tokio_stream::empty::<
                    Result<OpaquePayload, Status>,
                >()))
            }
        },
    )
    .await;
    client.session.channel()
}

/// Dials the daemon whose Machine the connection names, and counts each dial.
struct Fleet {
    channels: BTreeMap<MachineId, Channel>,
    dials: Mutex<Vec<MachineId>>,
}

#[tonic::async_trait]
impl Connector for Fleet {
    async fn connect(&self, connection: &Connection) -> Result<Channel, ConnectError> {
        let id = *connection
            .machine_id()
            .expect("a fleet connection names its Machine");
        self.dials.lock().unwrap().push(id);
        Ok(self.channels[&id].clone())
    }

    async fn dial_proxy(
        &self,
        _: &Connection,
        _: &str,
        _: &str,
    ) -> Result<BoxProxyStream, ConnectError> {
        unreachable!("a pin opens no proxy")
    }
}

fn connection(id: char) -> Connection {
    Connection::unix(format!("/fleet/{id}.sock"))
        .unwrap()
        .with_machine_id(machine_id(id))
}

struct Pinned {
    client: Client,
    log: Log,
    fleet: Arc<Fleet>,
}

/// Machines `a`, `b` and `c` as daemons, entered through `a`. The client holds a Full
/// connection to `b` when `full` says so; none reaches `c`. The log starts after the entry's
/// confirmation, which it keeps in `confirmed`.
async fn pinned(full: bool) -> (Pinned, Vec<Seen>) {
    let log = Log::default();
    let mut channels = BTreeMap::new();
    for id in ['a', 'b', 'c'] {
        channels.insert(machine_id(id), daemon(id, &log).await);
    }
    let fleet = Arc::new(Fleet {
        channels,
        dials: Mutex::default(),
    });
    let connector: Arc<dyn Connector> = fleet.clone();
    let mut client = crate::connect::connect_one(
        &connection('a'),
        &ConnectionSource::Direct,
        &connector,
        false,
        None,
    )
    .await
    .unwrap();
    if full {
        client = client.with_full_routes(FullRoutes::new(vec![connection('b')], false));
    }
    let confirmed = std::mem::take(&mut *log.lock().unwrap());
    (Pinned { client, log, fleet }, confirmed)
}

impl Pinned {
    fn seen(&self) -> Vec<Seen> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }

    fn dials(&self) -> Vec<MachineId> {
        std::mem::take(&mut *self.fleet.dials.lock().unwrap())
    }
}

fn target(id: char) -> MachineTarget {
    MachineTarget::from(&machine_id(id))
}

#[tokio::test]
async fn the_entry_is_confirmed_by_describing_itself_and_no_full_connection_is_dialed_early() {
    for full in [false, true] {
        let (pinned, confirmed) = pinned(full).await;
        assert_eq!(
            confirmed,
            [at('a', "describe_contract", None)],
            "full: {full}"
        );
        assert_eq!(pinned.dials(), [machine_id('a')], "full: {full}");
    }
}

#[tokio::test]
async fn a_unary_call_goes_direct_only_on_a_full_route_and_only_to_a_target_a_full_connection_names()
 {
    let (pinned, _) = pinned(true).await;
    let client = &pinned.client;
    let _ = pinned.dials();

    // A Full route to `b`: dialed itself, confirmed first, sent with no target header.
    let _ = client
        .call_shared::<op::SetDeviceEnforcement>(
            SetDeviceEnforcementRequest {
                enforced: false,
                starting_set: None,
            },
            Call::once().to(&target('b')),
        )
        .await;
    assert_eq!(
        pinned.seen(),
        [
            at('b', "describe_contract", None),
            at('b', "set_device_enforcement", None)
        ]
    );
    assert_eq!(pinned.dials(), [machine_id('b')]);

    // A route a Full caller alone may use, as Leave is, goes the same way; each call dials anew.
    for _ in 0..2 {
        let _ = client
            .call_shared::<op::Leave>(LeaveRequest {}, Call::once().to(&target('b')))
            .await;
    }
    assert_eq!(
        pinned.seen(),
        [
            at('b', "describe_contract", None),
            at('b', "leave", None),
            at('b', "describe_contract", None),
            at('b', "leave", None),
        ]
    );
    assert_eq!(pinned.dials(), [machine_id('b'), machine_id('b')]);

    // An Operator route is forwarded by the entry, with the target named.
    let _ = client
        .call_shared::<op::ListMachines>(ListMachinesRequest {}, Call::once().to(&target('b')))
        .await;
    assert_eq!(
        pinned.seen(),
        [at('a', "list_machines", Some(&target('b').to_string()))]
    );
    assert!(pinned.dials().is_empty());

    // A Full route to the entry itself, or to a Machine no Full connection names, is the
    // entry's too, naming the target.
    for id in ['a', 'c'] {
        let _ = client
            .call_shared::<op::Leave>(LeaveRequest {}, Call::once().to(&target(id)))
            .await;
        assert_eq!(
            pinned.seen(),
            [at('a', "leave", Some(&target(id).to_string()))],
            "{id}"
        );
    }

    // No target: the entry, no header, whatever the route.
    let _ = client
        .call_shared::<op::Inspect>(InspectRequest::default(), Call::once())
        .await;
    let _ = client
        .call_shared::<op::Leave>(LeaveRequest {}, Call::once())
        .await;
    assert_eq!(
        pinned.seen(),
        [at('a', "inspect", None), at('a', "leave", None)]
    );
    assert!(pinned.dials().is_empty());
}

#[tokio::test]
async fn a_client_with_no_full_connection_sends_every_route_through_the_entry() {
    let (pinned, _) = pinned(false).await;
    let _ = pinned.dials();
    let _ = pinned
        .client
        .call_shared::<op::Leave>(LeaveRequest {}, Call::once().to(&target('b')))
        .await;
    assert_eq!(
        pinned.seen(),
        [at('a', "leave", Some(&target('b').to_string()))]
    );
    assert!(pinned.dials().is_empty());
}

#[tokio::test]
async fn a_raw_call_routes_by_scope_as_a_unary_one_does() {
    let (pinned, _) = pinned(true).await;
    let _ = pinned.dials();
    let request = op::Leave::into_request(LeaveRequest {});
    let _ = pinned
        .client
        .call_raw(&request, op::Leave::PATH, Call::once().to(&target('b')))
        .await;
    assert_eq!(
        pinned.seen(),
        [at('b', "describe_contract", None), at('b', "leave", None)]
    );
}

fn no_input() -> impl tokio_stream::Stream<Item = OpaquePayload> + Send + 'static {
    tokio_stream::empty::<OpaquePayload>()
}

fn nothing() -> OpaquePayload {
    OpaquePayload::from_json(&serde_json::json!({})).unwrap()
}

#[tokio::test]
async fn a_stream_opens_where_its_route_goes() {
    let (pinned, _) = pinned(true).await;
    let client = &pinned.client;
    let named = |id: char| Some(target(id).to_string());
    let _ = pinned.dials();

    // Exec is a Full route: dialed itself, with no target header, as a unary Full call is.
    client
        .open_stream::<op::Exec>(Some(&target('b')), no_input())
        .await
        .unwrap();
    assert_eq!(
        pinned.seen(),
        [at('b', "describe_contract", None), at('b', "stream", None)]
    );
    assert_eq!(pinned.dials(), [machine_id('b')]);

    // To the entry itself, or a Machine no Full connection names, it goes through the entry.
    for id in ['a', 'c'] {
        client
            .open_stream::<op::Exec>(Some(&target(id)), no_input())
            .await
            .unwrap();
        assert_eq!(
            pinned.seen(),
            [at('a', "stream", named(id).as_deref())],
            "{id}"
        );
    }

    // A Build is submitted through the entry, naming its target, whatever Full connections
    // are held.
    client
        .open_stream::<op::Build>(Some(&target('b')), no_input())
        .await
        .unwrap();
    assert_eq!(pinned.seen(), [at('a', "stream", named('b').as_deref())]);

    // Logs go through the entry, naming their target.
    client
        .open_stream::<op::TailLogs>(Some(&target('b')), tokio_stream::once(nothing()))
        .await
        .unwrap();
    client
        .open_stream::<op::LogHistory>(Some(&target('b')), tokio_stream::once(nothing()))
        .await
        .unwrap();
    assert_eq!(
        pinned.seen(),
        [
            at('a', "stream", named('b').as_deref()),
            at('a', "stream", named('b').as_deref())
        ]
    );

    // A Runtime Watch is the entry's own, with no target.
    client
        .open_stream::<op::RuntimeWatch>(None, tokio_stream::once(nothing()))
        .await
        .unwrap();
    assert_eq!(pinned.seen(), [at('a', "stream", None)]);
    assert!(pinned.dials().is_empty());
}

#[tokio::test]
async fn an_exec_with_no_full_connection_goes_through_the_entry_naming_its_target() {
    let (pinned, _) = pinned(false).await;
    pinned
        .client
        .open_stream::<op::Exec>(Some(&target('b')), no_input())
        .await
        .unwrap();
    assert_eq!(
        pinned.seen(),
        [at('a', "stream", Some(&target('b').to_string()))]
    );
}

/// The paths a call to a named Machine dials itself, as the catalog and the authorization
/// table say. A route joining or leaving this list changes where a command's request goes.
/// A Build is on it, yet a Build is submitted through the entry whatever the connections.
const DIRECT: &[&str] = &[
    "WrapSealingKey",
    "RecoverSealingKey",
    "SetDevice",
    "SetDeviceEnforcement",
    "SetPasskey",
    "RemoveTrustedKey",
    "Leave",
    "Forget",
    "Exec",
    "Build",
];

macro_rules! direct_routes {
    (
        package $package:literal
        unary { $($unary_variant:ident: ($unary_method:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident, $unary_scope:ident),)+ }
        server_streaming { $($stream_variant:ident: ($stream_method:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident, $stream_scope:ident),)+ }
        bidi_streaming { $($bidi_variant:ident: ($bidi_method:ident, $bidi_route:literal, $bidi_command:literal, $bidi_capability:ident, $bidi_capability_name:literal, $bidi_advertisement:ident, $bidi_scope:ident),)+ }
        capabilities { $($(#[$extra_meta:meta])* $extra_capability:ident: ($extra_capability_name:literal, $extra_advertisement:ident),)+ }
    ) => {
        [$($unary_route,)+ $($stream_route,)+ $($bidi_route,)+]
            .into_iter()
            .filter(|route| dials_direct(&format!("/{}.MachineRpc/{route}", $package)))
            .collect::<Vec<_>>()
    };
}

#[test]
fn the_routes_a_call_dials_itself_are_the_ones_pinned_here() {
    assert_eq!(ployz_core::rpc_catalog!(direct_routes), DIRECT);
}

/// A daemon that answers `ListMachines` with no Machines after `delay`, and anything else with
/// an error.
async fn slow_lister(
    delay: Duration,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| async move {
        let body = rpc.into_inner().decode_request().unwrap().body;
        let RpcRequestBody::ListMachines(_) = body else {
            return Err(Status::unimplemented("pinned"));
        };
        tokio::time::sleep(delay).await;
        let listed = ployz_core::MachineList {
            enrollment: None,
            machines: Vec::new(),
        };
        Ok(Response::new(
            ployz_core::RpcResponse::from(listed).encode().unwrap(),
        ))
    })
    .await
}

fn timed_out(error: &dyn std::fmt::Debug) {
    let error = format!("{error:?}");
    assert!(error.contains("target Machine RPC timed out"), "{error}");
}

/// Six reads of the Machine list have no deadline of their own: a daemon that answers late
/// answers them, as the retries alone bound them. Each read is named in the failure.
#[tokio::test(start_paused = true)]
async fn the_reads_that_list_machines_without_a_deadline_wait_for_a_slow_daemon() {
    let (mut client, server) = slow_lister(Duration::from_secs(11)).await;
    let mut gave_up = Vec::new();
    let mut note = |read: &str, error: Option<String>| {
        if let Some(error) = error.filter(|error| error.contains("target Machine RPC timed out")) {
            gave_up.push(format!("{read}: {error}"));
        }
    };
    note(
        "observe_running",
        client
            .observe_running()
            .await
            .err()
            .map(|e| format!("{e:?}")),
    );
    note(
        "observe_copies",
        client
            .observe_copies()
            .await
            .err()
            .map(|e| format!("{e:?}")),
    );
    note(
        "observe_volumes",
        client
            .observe_volumes(vec![DockerVolumeName::parse("data").unwrap()])
            .await
            .err()
            .map(|e| format!("{e:?}")),
    );
    note(
        "remove_volumes",
        client
            .remove_volumes(ployz_core::RemoveVolumesRequest {
                volumes: Vec::new(),
                force: false,
            })
            .await
            .err()
            .map(|e| format!("{e:?}")),
    );
    note(
        "observe_enrollment",
        crate::enrollment::observe_enrollment(&mut client)
            .await
            .err()
            .map(|e| format!("{e:?}")),
    );
    note(
        "metric_history",
        crate::sdk::metric_history(
            &client,
            crate::sdk::MetricSubject::Service {
                service: ployz_core::QualifiedService::parse("prod/web").unwrap(),
            },
            ployz_core::metrics::MetricRange {
                since_nanos: 0,
                until_nanos: 60_000_000_000,
            },
            None,
        )
        .await
        .err()
        .map(|e| format!("{e:?}")),
    );
    assert!(gave_up.is_empty(), "{gave_up:#?}");
    server.abort();
}

/// The Machine list's own reads, `machines()` and `machine_list()`, give up after the
/// deadline on a daemon that is slow and on one that never answers.
#[tokio::test(start_paused = true)]
async fn machines_and_machine_list_give_up_after_the_machine_list_deadline() {
    let (mut slow, slow_server) = slow_lister(Duration::from_secs(11)).await;
    timed_out(&slow.machines().await.unwrap_err());
    timed_out(&slow.machine_list().await.unwrap_err());
    slow_server.abort();

    let (mut silent, silent_server) = slow_lister(Duration::from_secs(86_400)).await;
    let waited = Duration::from_secs(600);
    timed_out(
        &tokio::time::timeout(waited, silent.machines())
            .await
            .expect("machines() gives up on its own")
            .unwrap_err(),
    );
    silent_server.abort();
}
