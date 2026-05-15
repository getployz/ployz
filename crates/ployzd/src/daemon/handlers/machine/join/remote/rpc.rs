use ployz_api::{DaemonRequest, DaemonResponse};
use ployz_sdk::Transport;

use crate::daemon::ssh::{SshOptions, ssh_stdio_transport};

const REMOTE_RPC_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployzctl\" rpc-stdio";

pub(in crate::daemon::handlers::machine::join) async fn remote_rpc(
    target: &str,
    request: DaemonRequest,
    ssh_options: &SshOptions,
) -> Result<DaemonResponse, String> {
    let transport = ssh_stdio_transport(target, REMOTE_RPC_COMMAND, ssh_options);
    transport.request(request).await.map_err(|err| {
        format!(
            "remote rpc via '{}' failed: {err}",
            transport.command_display()
        )
    })
}

pub(in crate::daemon::handlers::machine::join) async fn remote_rpc_expect_ok(
    target: &str,
    request: DaemonRequest,
    ssh_options: &SshOptions,
) -> Result<(), String> {
    let response = remote_rpc(target, request, ssh_options).await?;
    if response.is_ok() {
        return Ok(());
    }
    Err(remote_response_error(&response))
}

pub(in crate::daemon::handlers::machine::join) fn remote_response_error(
    response: &DaemonResponse,
) -> String {
    format!(
        "remote daemon error [{}]: {}",
        response.code(),
        response.message()
    )
}
