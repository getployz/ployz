//! `ployz debug`: verification-only verbs.

use clap::{ArgMatches, Command};
use ployz_core::{MachineTarget, RpcErrorCode, RpcRequest, RpcRequestBody, RpcResponseBody};
use serde_json::json;

use crate::{
    cli::{base, positional},
    connect::TARGET_RPC_TIMEOUT,
    handlers::{Error, leaf_matches, with_client},
    ui,
};

pub(super) fn group(command: Command) -> Command {
    base(command, "Verification-only commands").hide(true)
}

pub(super) fn volume_rpc_command(command: Command) -> Command {
    base(command, "Send one unary Machine RPC as JSON and print its response")
        .long_about("Send one unary Machine RPC to a Server and print the response body. REQUEST is the JSON request body, for example {\"command\":\"inspect_volume_copy\",\"payload\":{\"name\":\"data\"}}. An error response fails the command with the error's code and details.")
        .arg(positional("server", true))
        .arg(positional("request", true).value_name("REQUEST"))
}

pub(super) fn local_rpc_command(command: Command) -> Command {
    base(command, "Send one unary Machine RPC to the dialed daemon, unrouted")
        .long_about("Send one unary Machine RPC to the daemon this CLI dials, with no routing, and print the response body. It reaches a Machine outside any cluster, which volume-rpc cannot route to. REQUEST is the JSON request body, as for volume-rpc.")
        .arg(positional("request", true).value_name("REQUEST"))
}

pub(super) fn volume_rpc(root: &ArgMatches) -> Result<(), Error> {
    let selector = MachineTarget::parse(
        leaf_matches(root)
            .get_one::<String>("server")
            .ok_or_else(|| Error::usage("server is required"))?,
    )?;
    send(root, Some(selector))
}

pub(super) fn local_rpc(root: &ArgMatches) -> Result<(), Error> {
    send(root, None)
}

fn send(root: &ArgMatches, target: Option<MachineTarget>) -> Result<(), Error> {
    let body: RpcRequestBody = serde_json::from_str(
        leaf_matches(root)
            .get_one::<String>("request")
            .ok_or_else(|| Error::usage("request is required"))?,
    )
    .map_err(|error| {
        Error::caused(
            RpcErrorCode::InvalidArgument,
            "request is not an RPC request body",
            error,
        )
    })?;
    let path = body
        .unary_path()
        .ok_or_else(|| Error::usage("request names a streaming RPC; debug sends unary RPCs"))?;
    let request = RpcRequest::from(body);
    with_client(root, |client| {
        Box::pin(async move {
            let response = client
                .invoke_raw(&request, path, target.as_ref(), Some(TARGET_RPC_TIMEOUT))
                .await?;
            if let RpcResponseBody::Error(error) = response.body {
                return Err(Error::from(error));
            }
            ui::show(&json!({ "response": response }))
        })
    })
}
