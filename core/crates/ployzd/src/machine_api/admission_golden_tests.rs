//! What every coverage case is answered, word for word, as each kind of caller and in each
//! state a Machine admits from.
//!
//! The refusal's order, text, code and details are what a CLI, Cloud and an older daemon read,
//! so a change to how the Machine API admits a call shows up here as a diff to review.
//! `PLOYZ_BLESS=1` rewrites `admission_golden.txt`.

use std::fmt::Write as _;
use std::sync::Arc;

use ployz_core::authority::coverage::CASES;
use ployz_core::{
    MachineRpcClient, OpaquePayload, RpcResponse, RpcResponseBody, rpc::status_details,
};
use tonic::{Request, Status, transport::Channel};

use super::{Enforcing, Sample, refusal_in, sample};
use crate::machine_api::{MachineApi, test_socket};

const GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/machine_api/admission_golden.txt"
);

/// A caller as the Machine API sees it: what it connects over and which marks it carries.
#[derive(Clone, Copy)]
struct Caller {
    name: &'static str,
    local_socket: bool,
    operator_header: bool,
    cloud_routed_header: bool,
}

const fn caller(name: &'static str, local_socket: bool, operator: bool, routed: bool) -> Caller {
    Caller {
        name,
        local_socket,
        operator_header: operator,
        cloud_routed_header: routed,
    }
}

const CALLERS: &[Caller] = &[
    caller("mesh", false, false, false),
    caller("mesh with operator header", false, true, false),
    caller("mesh with cloud-routed header", false, false, true),
    caller("local socket", true, false, false),
    caller("local socket with operator header", true, true, false),
];

#[derive(Clone, Copy)]
enum State {
    Participating,
    Resetting,
    Busy,
}

impl State {
    const ALL: [Self; 3] = [Self::Participating, Self::Resetting, Self::Busy];

    fn name(self) -> &'static str {
        match self {
            Self::Participating => "participating",
            Self::Resetting => "resetting",
            Self::Busy => "busy with a mutation",
        }
    }
}

/// Every case sent as `caller` to a fresh Machine in `state`, one line each.
async fn answers(caller: Caller, state: State) -> String {
    let runs: Vec<_> = CASES
        .iter()
        .map(|case| tokio::spawn(one(case, caller, state)))
        .collect();
    let mut text = format!("## {} / {}\n", state.name(), caller.name);
    for (case, run) in CASES.iter().zip(runs) {
        writeln!(
            text,
            "{} | {} => {}",
            case.capability,
            case.branch,
            run.await.unwrap()
        )
        .unwrap();
    }
    text
}

async fn one(
    case: &'static ployz_core::authority::coverage::Case,
    caller: Caller,
    state: State,
) -> String {
    let machine = Enforcing::new().await;
    let sample = sample(case, &machine).await;
    let answer = match &sample {
        Sample::Elsewhere => "proven in a unit test".to_owned(),
        Sample::Unparsed(error) => panic!("{error}"),
        Sample::Unary(..) | Sample::Exec | Sample::Build => {
            send_in(&machine, &sample, caller, state).await
        }
    };
    machine.remove();
    answer
}

async fn send_in(machine: &Enforcing, sample: &Sample, caller: Caller, state: State) -> String {
    let release = Arc::new(tokio::sync::Notify::new());
    let held = match state {
        State::Participating => None,
        State::Resetting => {
            machine
                .store
                .mutate(|store| store.begin_reset().unwrap())
                .await
                .unwrap();
            None
        }
        State::Busy => {
            let started = Arc::new(tokio::sync::Notify::new());
            let local = machine.service.local();
            let hold = tokio::spawn({
                let (started, release) = (Arc::clone(&started), Arc::clone(&release));
                async move { local.hold_mutation_for_test(started, release).await }
            });
            started.notified().await;
            Some(hold)
        }
    };
    let (channel, server) = connect(machine, caller).await;
    // real-wait: a call queued behind the held mutation never answers until it is released
    let answer = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        call(channel, sample, caller),
    )
    .await
    .map_or_else(
        |_| "waits for the turn".to_owned(),
        |answer| normalize(&answer),
    );
    if let Some(hold) = held {
        release.notify_one();
        let _ = hold.await;
    }
    if let Some(server) = server {
        server.abort();
    }
    answer
}

type Server = tokio::task::JoinHandle<Result<(), tonic::transport::Error>>;

async fn connect(machine: &Enforcing, caller: Caller) -> (Channel, Option<Server>) {
    if !caller.local_socket {
        let channel = tonic::transport::Endpoint::from_shared(machine.address.clone())
            .unwrap()
            .connect()
            .await
            .unwrap();
        return (channel, None);
    }
    let socket = test_socket::socket_in(&machine.data_dir);
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let api = MachineApi::from_local(machine.service.clone());
    let server = tokio::spawn(tonic::transport::Server::builder().serve_with_incoming(
        api,
        tokio_stream::wrappers::UnixListenerStream::new(listener),
    ));
    (test_socket::channel_to(&socket).await, Some(server))
}

fn marked<T>(mut request: Request<T>, caller: Caller) -> Request<T> {
    let mark = tonic::metadata::MetadataValue::from_static("1");
    if caller.operator_header {
        request
            .metadata_mut()
            .insert(ployz_core::OPERATOR_SCOPE_HEADER, mark.clone());
    }
    if caller.cloud_routed_header {
        request
            .metadata_mut()
            .insert(ployz_core::CLOUD_ROUTED_HEADER, mark);
    }
    request
}

async fn call(channel: Channel, sample: &Sample, caller: Caller) -> String {
    match sample {
        Sample::Unary(path, request) => {
            let mut grpc = tonic::client::Grpc::new(channel);
            grpc.ready().await.unwrap();
            let result = grpc
                .unary(
                    marked(Request::new(request.encode().unwrap()), caller),
                    tonic::codegen::http::uri::PathAndQuery::from_static(path),
                    tonic::codec::ProstCodec::<OpaquePayload, OpaquePayload>::default(),
                )
                .await;
            describe(result.map(|reply| reply.into_inner().decode_response().unwrap()))
        }
        Sample::Exec | Sample::Build => {
            let mut client = MachineRpcClient::new(channel);
            let input = marked(
                Request::new(tokio_stream::iter([OpaquePayload::new(b"{}".to_vec())])),
                caller,
            );
            let opened = if matches!(sample, Sample::Exec) {
                client.exec(input).await.map(|_| ())
            } else {
                match client.build(input).await {
                    Ok(reply) => reply.into_inner().message().await.map(|_| ()),
                    Err(status) => Err(status),
                }
            };
            opened.map_or_else(|status| describe(Err(status)), |()| "served".to_owned())
        }
        Sample::Elsewhere | Sample::Unparsed(_) => unreachable!("never sent"),
    }
}

fn describe(result: Result<RpcResponse, Status>) -> String {
    let response = match result {
        Ok(response) => response,
        Err(status) => {
            let details = status_details(status.details()).map(|details| details.to_string());
            return format!(
                "status {:?}: {} {}",
                status.code(),
                status.message(),
                details.unwrap_or_default()
            );
        }
    };
    if let RpcResponseBody::Error(error) = &response.body {
        return format!(
            "error {:?}: {} {}",
            error.code, error.message, error.details
        );
    }
    match refusal_in(&serde_json::to_value(&response).unwrap()) {
        Some(refusal) => format!("refused {refusal}"),
        None => "served".to_owned(),
    }
}

/// Ids and paths differ per run; the words around them don't.
fn normalize(text: &str) -> String {
    use std::sync::LazyLock;
    static VARIABLE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|[0-9a-f]{16,}|[A-Za-z0-9+/]{43}=|127\.0\.0\.1:[0-9]+|/tmp/ployzd-[^\s\x22]*",
        )
        .unwrap()
    });
    VARIABLE.replace_all(text, "<id>").into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn every_case_is_answered_as_it_was_by_every_caller_in_every_state() {
    let mut text = String::new();
    for state in State::ALL {
        for caller in CALLERS {
            text.push_str(&answers(*caller, state).await);
        }
    }
    if std::env::var_os("PLOYZ_BLESS").is_some() {
        std::fs::write(GOLDEN, &text).unwrap();
        return;
    }
    assert!(
        std::fs::read_to_string(GOLDEN).unwrap() == text,
        "an admission answer changed; rerun with PLOYZ_BLESS=1 and review {GOLDEN}"
    );
}
