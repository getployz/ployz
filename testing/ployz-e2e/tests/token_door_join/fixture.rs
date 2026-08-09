use super::{CLUSTER_NAME, FOUNDER_NAME, WAIT_BUDGET, WAIT_DELAY};
use bollard::Docker;
use ployz::init::ssh::SshPeerKey;
use ployz::mesh::context::{SSH_CONTEXT_HANDOFF_PREFIX, SshContextHandoff};
use ployz_core::corrosion::{MachineDocument, MachineTransport, SqliteValue};
use ployz_core::ids::{MachineName, TokenName};
use ployz_core::join::JoinBlob;
use ployz_e2e::dind::{
    DindMachine, ExecOutcome, RELEASE_MANIFEST, assert_keeper_isolation_root, corrosion_query,
    exec_in_container, exec_ok, require,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Instant;

#[derive(Clone)]
pub(super) struct RosterMachine {
    pub(super) id: MachineName,
    pub(super) document: MachineDocument,
}

#[derive(Clone, Copy)]
pub(super) struct CorrosionAccess<'a> {
    pub(super) docker: &'a Docker,
    pub(super) machine: &'a DindMachine,
    pub(super) address: &'a str,
    pub(super) token: &'a str,
}

pub(super) async fn run_founding(
    docker: &Docker,
    machine: &DindMachine,
    operator: &SshPeerKey,
) -> Result<SshContextHandoff, String> {
    let manifest = format!("file://{RELEASE_MANIFEST}");
    let command = [
        "env".to_owned(),
        format!("PLOYZ_RELEASE_MANIFEST_URL={manifest}"),
        "/opt/ployz/artifacts/ployz".to_owned(),
        "init".to_owned(),
        "--storage".to_owned(),
        "plain".to_owned(),
        "--container-network".to_owned(),
        "10.210.0.0/16".to_owned(),
        "--service-urls".to_owned(),
        "disabled".to_owned(),
        "--cluster-name".to_owned(),
        CLUSTER_NAME.to_owned(),
        "--machine-name".to_owned(),
        FOUNDER_NAME.to_owned(),
        "--driver-peer-id".to_owned(),
        operator.peer_id.to_string(),
        "--driver-peer-public-key".to_owned(),
        operator.public_key.as_str().to_owned(),
    ];
    let refs = command.iter().map(String::as_str).collect::<Vec<_>>();
    let outcome = exec_in_container(docker, &machine.container_id, &refs)
        .await
        .map_err(|error| error.to_string())?;
    require(
        outcome.success(),
        format!("founding command failed: {outcome:?}"),
    )?;
    assert_keeper_isolation_root(docker, machine, "ployzd-keeper.service").await?;
    parse_handoff(&outcome.stdout)
}

fn parse_handoff(stdout: &str) -> Result<SshContextHandoff, String> {
    let mut handoffs = stdout
        .lines()
        .filter(|line| line.starts_with(SSH_CONTEXT_HANDOFF_PREFIX));
    let Some(line) = handoffs.next() else {
        return Err(format!(
            "founding output omitted SSH context handoff: {stdout}"
        ));
    };
    require(
        handoffs.next().is_none(),
        "founding emitted more than one SSH context handoff",
    )?;
    SshContextHandoff::decode_handoff(line).map_err(|error| error.to_string())
}

pub(super) async fn assert_missing_endpoint_refuses_without_a_token(
    store: CorrosionAccess<'_>,
    cli: &Path,
    home: &Path,
) -> Result<(), String> {
    let refused = run_cli(
        cli,
        home,
        ["token", "create", "endpoint-probe", "--ttl", "1h"].map(str::to_owned),
    )?;
    require(
        !refused.status.success(),
        "token create without an advertised door endpoint unexpectedly succeeded",
    )?;
    let expected = "cannot create a join token because no public door endpoint is advertised; run `ployz machine endpoint set <machine> <ip:wireguard-port>`\n";
    require(
        refused.stdout.is_empty() && refused.stderr == expected.as_bytes(),
        format!(
            "token create returned the wrong refusal: stdout={} stderr={}",
            String::from_utf8_lossy(&refused.stdout),
            String::from_utf8_lossy(&refused.stderr)
        ),
    )?;
    let rows = corrosion_query(
        store.docker,
        store.machine,
        store.address,
        store.token,
        "SELECT COUNT(*) FROM tokens",
    )
    .await?;
    require(
        rows == vec![vec![SqliteValue::Integer(0)]],
        format!("refused token create wrote a row: {rows:?}"),
    )
}

pub(super) fn handoff_with_known_endpoint(
    handoff: SshContextHandoff,
    endpoint: SocketAddr,
) -> Result<SshContextHandoff, String> {
    let SshContextHandoff {
        cluster_id,
        provider,
        machine_transport,
    } = handoff;
    let machine_transport = match machine_transport {
        MachineTransport::Wireguard {
            pubkey,
            addr_v6,
            endpoint: _,
            subnet_v4,
        } => MachineTransport::Wireguard {
            pubkey,
            addr_v6,
            endpoint: Some(endpoint),
            subnet_v4,
        },
        MachineTransport::Tailscale { .. } => {
            return Err("founding handoff does not use builtin WireGuard".to_owned());
        }
    };
    Ok(SshContextHandoff {
        cluster_id,
        provider,
        machine_transport,
    })
}

pub(super) fn run_cli(
    cli: &Path,
    home: &Path,
    args: impl IntoIterator<Item = String>,
) -> Result<Output, String> {
    Command::new(cli)
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not run shipped CLI {}: {error}", cli.display()))
}

pub(super) fn require_success(output: &Output, operation: &str) -> Result<(), String> {
    require(
        output.status.success(),
        format!(
            "{operation} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

pub(super) fn extract_join_blob(stdout: &str) -> Result<JoinBlob, String> {
    let value = stdout
        .lines()
        .find_map(|line| {
            line.strip_prefix("JOIN_BLOB='")
                .and_then(|value| value.strip_suffix('\''))
        })
        .ok_or_else(|| format!("token create output omitted JOIN_BLOB: {stdout}"))?;
    JoinBlob::try_parse(value).map_err(|error| error.to_string())
}

pub(super) fn extract_token_id(stdout: &str) -> Result<TokenName, String> {
    let value = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("token  "))
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| format!("token create output omitted token id: {stdout}"))?;
    TokenName::try_new(value).map_err(|error| error.to_string())
}

pub(super) async fn wait_for_machine_roster(
    store: CorrosionAccess<'_>,
    minimum: usize,
) -> Result<BTreeMap<MachineName, RosterMachine>, String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("roster was not queried");
    while Instant::now() < deadline {
        match corrosion_query(
            store.docker,
            store.machine,
            store.address,
            store.token,
            "SELECT id, document FROM machines",
        )
        .await
        {
            Ok(rows) => match parse_machine_rows(rows) {
                Ok(roster) if roster.len() >= minimum => return Ok(roster),
                Ok(roster) => last = format!("only {} machine rows", roster.len()),
                Err(error) => last = error,
            },
            Err(error) => last = error,
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!("machine roster did not converge: {last}"))
}

fn parse_machine_rows(
    rows: Vec<Vec<SqliteValue>>,
) -> Result<BTreeMap<MachineName, RosterMachine>, String> {
    let mut roster = BTreeMap::new();
    for row in rows {
        let [SqliteValue::Text(id), SqliteValue::Text(document)] = row.as_slice() else {
            return Err(format!("machine query returned an invalid row: {row:?}"));
        };
        let id = MachineName::try_new(id.clone()).map_err(|error| error.to_string())?;
        let document: MachineDocument = serde_json::from_str(document)
            .map_err(|error| format!("machine row was invalid: {error}"))?;
        roster.insert(id.clone(), RosterMachine { id, document });
    }
    Ok(roster)
}

pub(super) fn machine_subnet(document: &MachineDocument) -> Result<String, String> {
    match &document.transport {
        MachineTransport::Wireguard { subnet_v4, .. }
        | MachineTransport::Tailscale { subnet_v4, .. } => Ok(subnet_v4.as_string()),
    }
}

pub(super) async fn corrosion_transaction(
    store: CorrosionAccess<'_>,
    statements: &Value,
) -> Result<(), String> {
    let body = statements.to_string();
    let url = format!("http://{}/v1/transactions", store.address);
    let authorization = format!("Authorization: Bearer {}", store.token);
    let outcome = exec_ok(
        store.docker,
        store.machine,
        &[
            "curl",
            "--noproxy",
            "*",
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "5",
            "--header",
            &authorization,
            "--header",
            "Accept: application/json",
            "--header",
            "Content-Type: application/json",
            "--data-binary",
            &body,
            &url,
        ],
    )
    .await?;
    require(
        !outcome.stdout.contains("\"error\""),
        format!("Corrosion transaction failed: {}", outcome.stdout),
    )
}

pub(super) async fn wait_for_command(
    docker: &Docker,
    machine: &DindMachine,
    command: &[&str],
    predicate: impl Fn(&ExecOutcome) -> bool,
    description: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("command was not attempted");
    while Instant::now() < deadline {
        match exec_in_container(docker, &machine.container_id, command).await {
            Ok(outcome) if predicate(&outcome) => return Ok(()),
            Ok(outcome) => last = format!("{outcome:?}"),
            Err(error) => last = error.to_string(),
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!("timed out waiting for {description}: {last}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_handoff_uses_known_outer_endpoint_without_changing_mesh_identity() {
        let handoff = SshContextHandoff::decode_handoff(&format!(
            "{SSH_CONTEXT_HANDOFF_PREFIX}{{\"cluster_id\":\"main\",\"provider\":\"builtin_wireguard\",\"machine_transport\":{{\"kind\":\"wireguard\",\"pubkey\":\"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\",\"addr_v6\":\"fd00::10\",\"endpoint\":null,\"subnet_v4\":\"10.210.10.0/24\"}}}}"
        ))
        .expect("founding handoff");
        let cluster_id = handoff.cluster_id.clone();
        let endpoint = "192.0.2.10:51820".parse().expect("outer endpoint");

        let dialable = handoff_with_known_endpoint(handoff, endpoint).expect("dialable handoff");

        assert_eq!(dialable.cluster_id, cluster_id);
        assert!(matches!(
            dialable.machine_transport,
            MachineTransport::Wireguard {
                endpoint: Some(found),
                ..
            } if found == endpoint
        ));
    }
}
