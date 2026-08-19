//! HTTP/1.1 adapter: WebSocket Register/Dial/Attach, POST List/Revoke.

use std::{net::SocketAddr, time::Duration};

use axum::{
    Router,
    body::Bytes,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use ployz_core::{MachineId, TunnelId};
use prost::Message as _;
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};

use crate::{
    ATTACH_PATH, DIAL_PATH, LIST_PATH, MACHINE_ID_HEADER, PAIRING_HEADER, REGISTER_PATH,
    REVOKE_PATH, RegisterRequest, Relay, RevokeResponse, Status, TUNNEL_ID_HEADER, TunnelFrame,
};

pub(crate) async fn bind(
    relay: Relay,
    listen: SocketAddr,
    drain: Duration,
) -> std::io::Result<(SocketAddr, JoinHandle<std::io::Result<()>>, crate::Goaway)> {
    let listener = TcpListener::bind(listen).await?;
    let address = listener.local_addr()?;
    let goaway = crate::Goaway {
        phase: relay.phase.clone(),
    };
    let closer = relay.clone();
    let mut expire_phase = closer.phase.subscribe();
    let app = Router::new()
        .route(REGISTER_PATH, get(register))
        .route(DIAL_PATH, get(dial))
        .route(ATTACH_PATH, get(attach))
        .route(LIST_PATH, post(list))
        .route(REVOKE_PATH, post(revoke))
        .with_state(relay);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                if expire_phase
                    .wait_for(|phase| *phase != crate::Phase::Live)
                    .await
                    .is_ok()
                {
                    tokio::time::sleep(drain).await;
                    closer.force_close();
                }
            })
            .await
    });
    Ok((address, handle, goaway))
}

async fn register(
    State(relay): State<Relay>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, Status> {
    let pairing = relay.register_pairing(bearer(&headers))?;
    relay.require_accepting()?;
    Ok(ws
        .on_upgrade(move |socket| run_register(relay, socket, pairing))
        .into_response())
}

async fn dial(
    State(relay): State<Relay>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, Status> {
    let pairing = relay.cloud_pairing_live(bearer(&headers), header(&headers, PAIRING_HEADER))?;
    let machine_id = parse_machine_id(header(&headers, MACHINE_ID_HEADER))?;
    let started = relay.start_dial(pairing, machine_id).await?;
    Ok(ws
        .on_upgrade(move |socket| {
            splice_ws(socket, started.inbound, started.outbound, started.cancel)
        })
        .into_response())
}

async fn attach(
    State(relay): State<Relay>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, Status> {
    relay.require_accepting()?;
    let tunnel_id = parse_tunnel_id(header(&headers, TUNNEL_ID_HEADER))?;
    let started = relay.start_attach(tunnel_id)?;
    Ok(ws
        .on_upgrade(move |socket| {
            splice_ws(socket, started.inbound, started.outbound, started.cancel)
        })
        .into_response())
}

async fn list(State(relay): State<Relay>, headers: HeaderMap) -> Result<Response, Status> {
    let pairing = relay.cloud_pairing_live(bearer(&headers), header(&headers, PAIRING_HEADER))?;
    Ok(protobuf(relay.list(&pairing)))
}

async fn revoke(State(relay): State<Relay>, headers: HeaderMap) -> Result<Response, Status> {
    let pairing = relay.cloud_pairing(bearer(&headers), header(&headers, PAIRING_HEADER))?;
    relay.revoke(pairing);
    Ok(protobuf(RevokeResponse {}))
}

async fn run_register(relay: Relay, mut socket: WebSocket, pairing: crate::PairingCredential) {
    let Some(bytes) = recv_binary(&mut socket).await else {
        return;
    };
    let Ok(hello) = RegisterRequest::decode(bytes.as_ref()) else {
        return;
    };
    if hello.echo() != 0 {
        return;
    }
    let Ok(machine_id) = hello.machine_id() else {
        return;
    };
    let Ok(started) = relay.start_register(pairing, machine_id) else {
        return;
    };
    let mut opens = started.opens;
    loop {
        tokio::select! {
            open = opens.recv() => {
                let Some(open) = open else { break };
                if send_msg(&mut socket, &open).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => {
                match next_binary(incoming) {
                    Next::Bytes(bytes) => {
                        let Ok(message) = RegisterRequest::decode(bytes.as_ref()) else {
                            break;
                        };
                        if started.inbound.send(message).await.is_err() {
                            break;
                        }
                    }
                    Next::Skip => {}
                    Next::End => break,
                }
            }
        }
    }
}

async fn splice_ws(
    mut socket: WebSocket,
    tx: mpsc::Sender<TunnelFrame>,
    mut rx: mpsc::Receiver<TunnelFrame>,
    mut cancel: tokio::sync::watch::Receiver<()>,
) {
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match next_binary(incoming) {
                    Next::Bytes(bytes) => {
                        let Ok(frame) = TunnelFrame::decode(bytes.as_ref()) else {
                            break;
                        };
                        if tx.send(frame).await.is_err() {
                            break;
                        }
                    }
                    Next::Skip => {}
                    Next::End => break,
                }
            }
            outgoing = rx.recv() => {
                let Some(frame) = outgoing else { break };
                if send_msg(&mut socket, &frame).await.is_err() {
                    break;
                }
            }
            _ = cancel.changed() => break,
        }
    }
}

enum Next {
    Bytes(Bytes),
    Skip,
    End,
}

fn next_binary(incoming: Option<Result<Message, axum::Error>>) -> Next {
    match incoming {
        Some(Ok(Message::Binary(bytes))) => Next::Bytes(bytes),
        Some(Ok(Message::Ping(_) | Message::Pong(_))) => Next::Skip,
        Some(Ok(Message::Text(_) | Message::Close(_))) | Some(Err(_)) | None => Next::End,
    }
}

async fn recv_binary(socket: &mut WebSocket) -> Option<Bytes> {
    loop {
        match next_binary(socket.recv().await) {
            Next::Bytes(bytes) => return Some(bytes),
            Next::Skip => {}
            Next::End => return None,
        }
    }
}

async fn send_msg(socket: &mut WebSocket, message: &impl prost::Message) -> Result<(), ()> {
    socket
        .send(Message::Binary(Bytes::from(message.encode_to_vec())))
        .await
        .map_err(|_| ())
}

fn protobuf(message: impl prost::Message) -> Response {
    (StatusCode::OK, message.encode_to_vec()).into_response()
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn header<'a>(headers: &'a HeaderMap, name: &'static str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn parse_machine_id(value: Option<&str>) -> Result<MachineId, Status> {
    MachineId::parse(
        value.ok_or_else(|| Status::invalid_argument("missing or invalid machine-id"))?,
    )
    .map_err(|error| Status::invalid_argument(error.to_string()))
}

fn parse_tunnel_id(value: Option<&str>) -> Result<TunnelId, Status> {
    let Some(value) = value else {
        return Err(Status::invalid_argument("missing or invalid tunnel-id"));
    };
    TunnelId::parse(value).map_err(|error| Status::invalid_argument(error.to_string()))
}

impl IntoResponse for Status {
    fn into_response(self) -> Response {
        (self.status(), self.message().to_owned()).into_response()
    }
}
