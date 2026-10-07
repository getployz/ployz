//! `ployz debug`: verification-only verbs.

use clap::Command;

#[cfg(feature = "verify-faults")]
pub(crate) fn attach(command: Command) -> Command {
    command.subcommand(enabled::command())
}

#[cfg(not(feature = "verify-faults"))]
pub(crate) fn attach(command: Command) -> Command {
    command
}

#[cfg(feature = "verify-faults")]
pub(super) fn handler(path: &str) -> Option<super::Handler> {
    match path {
        "volume-rpc" => Some(enabled::volume_rpc),
        _ => None,
    }
}

#[cfg(not(feature = "verify-faults"))]
pub(super) fn handler(_path: &str) -> Option<super::Handler> {
    None
}

#[cfg(feature = "verify-faults")]
mod enabled {
    use clap::{ArgMatches, Command};
    use ployz_core::{MachineTarget, RpcErrorCode, RpcRequest, RpcRequestBody, RpcResponseBody};
    use serde_json::json;

    use crate::{
        cli::{base, positional},
        connect::TARGET_RPC_TIMEOUT,
        handlers::{Error, leaf_matches, with_client},
        ui,
    };

    pub(super) fn command() -> Command {
        base("debug", "Verification-only commands")
            .hide(true)
            .subcommand(
                base("volume-rpc", "Send one unary Machine RPC as JSON and print its response")
                    .long_about("Send one unary Machine RPC to a Server and print the response body. REQUEST is the JSON request body, for example {\"command\":\"adopt_lease\",\"payload\":{...}}. An error response fails the command with the error's code and details.")
                    .arg(positional("server", true))
                    .arg(positional("request", true).value_name("REQUEST")),
            )
    }

    pub(super) fn volume_rpc(root: &ArgMatches) -> Result<(), Error> {
        let matches = leaf_matches(root);
        let selector = MachineTarget::parse(
            matches
                .get_one::<String>("server")
                .ok_or_else(|| Error::usage("server is required"))?,
        )?;
        let body: RpcRequestBody = serde_json::from_str(
            matches
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
        let path = body.unary_path().ok_or_else(|| {
            Error::usage("request names a streaming RPC; volume-rpc sends unary RPCs")
        })?;
        let request = RpcRequest::from(body);
        with_client(root, |client| {
            Box::pin(async move {
                let response = client
                    .invoke_raw(&request, path, &selector, Some(TARGET_RPC_TIMEOUT))
                    .await?;
                if let RpcResponseBody::Error(error) = response.body {
                    return Err(Error::from(error));
                }
                ui::show(&json!({ "response": response }))
            })
        })
    }
}
