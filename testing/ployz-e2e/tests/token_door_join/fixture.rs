use super::{CLUSTER_NAME, FOUNDER_NAME, WAIT_BUDGET, WAIT_DELAY};
use bollard::Docker;
use ployz::init::ssh::SshPeerKey;
use ployz::mesh::context::{SSH_CONTEXT_HANDOFF_PREFIX, SshContextHandoff};
use ployz_core::corrosion::{MachineDocument, MachineTransport, SqliteValue};
use ployz_core::ids::{MachineRowId, TokenId};
use ployz_core::join::{
    JoinBlob, MachineEndpointSetReply, MachineEndpointSetRequest, TokenCreateRefusal,
};
use ployz_core::machine::MachineName;
use ployz_core::{MACHINE_ENDPOINT_ROUTE_PREFIX, TOKEN_CREATE_ROUTE};
use ployz_e2e::dind::{
    DindMachine, ExecOutcome, RELEASE_MANIFEST, corrosion_query, env_value, exec_in_container,
    exec_ok, require,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Instant;

#[derive(Clone)]
pub(super) struct RosterMachine {
    pub(super) id: MachineRowId,
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
        "--driver-peer-name".to_owned(),
        operator.peer_name.clone(),
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

pub(super) async fn founding_api_address(
    docker: &Docker,
    machine: &DindMachine,
) -> Result<String, String> {
    let env = exec_ok(docker, machine, &["cat", "/var/lib/ployz/ployzd.env"])
        .await?
        .stdout;
    env_value(&env, "PLOYZ_API_LISTEN_ADDR")
}

pub(super) async fn assert_missing_endpoint_refuses_without_a_token(
    docker: &Docker,
    machine: &DindMachine,
    api_address: &str,
    corrosion_address: &str,
    corrosion_token: &str,
) -> Result<(), String> {
    let refusal: TokenCreateRefusal = local_api_json(
        docker,
        machine,
        api_address,
        TOKEN_CREATE_ROUTE,
        &json!({"ttl_seconds": 3600}),
    )
    .await?;
    let TokenCreateRefusal::NoAdvertisedDoorEndpoint { repair_command } = refusal else {
        return Err(format!(
            "missing endpoint returned the wrong refusal: {refusal:?}"
        ));
    };
    require(
        repair_command == "ployz machine endpoint set <machine> <ip:wireguard-port>",
        format!("token refusal named the wrong repair command: {repair_command}"),
    )?;
    let rows = corrosion_query(
        docker,
        machine,
        corrosion_address,
        corrosion_token,
        "SELECT COUNT(*) FROM tokens",
    )
    .await?;
    require(
        rows == vec![vec![SqliteValue::Integer(0)]],
        format!("refused token create wrote a row: {rows:?}"),
    )
}

pub(super) async fn set_endpoint_on_local_public_api(
    docker: &Docker,
    machine: &DindMachine,
    api_address: &str,
    endpoint: SocketAddr,
) -> Result<MachineEndpointSetReply, String> {
    let reply: MachineEndpointSetReply = local_api_json(
        docker,
        machine,
        api_address,
        MACHINE_ENDPOINT_ROUTE_PREFIX,
        &MachineEndpointSetRequest {
            machine_name: MachineName::try_new(FOUNDER_NAME).map_err(|error| error.to_string())?,
            endpoint,
        },
    )
    .await?;
    require(
        matches!(&reply.machine.transport, MachineTransport::Wireguard { endpoint: Some(found), .. } if *found == endpoint),
        "endpoint-set reply did not carry the requested endpoint",
    )?;
    Ok(reply)
}

async fn local_api_json<Request, Reply>(
    docker: &Docker,
    machine: &DindMachine,
    api_address: &str,
    route: &str,
    request: &Request,
) -> Result<Reply, String>
where
    Request: Serialize + ?Sized,
    Reply: DeserializeOwned,
{
    let body = serde_json::to_string(request).map_err(|error| error.to_string())?;
    let url = format!("http://{api_address}{route}");
    let outcome = exec_ok(
        docker,
        machine,
        &[
            "curl",
            "--noproxy",
            "*",
            "--silent",
            "--show-error",
            "--max-time",
            "5",
            "--request",
            "POST",
            "--header",
            "Content-Type: application/json",
            "--data-binary",
            &body,
            &url,
        ],
    )
    .await?;
    serde_json::from_str(outcome.stdout.trim()).map_err(|error| {
        format!(
            "{route} returned invalid JSON ({error}): {}",
            outcome.stdout
        )
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

pub(super) fn extract_token_id(stdout: &str) -> Result<TokenId, String> {
    let value = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("token  "))
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| format!("token create output omitted token id: {stdout}"))?;
    TokenId::try_new(value).map_err(|error| error.to_string())
}

pub(super) async fn wait_for_machine_roster(
    store: CorrosionAccess<'_>,
    minimum: usize,
) -> Result<BTreeMap<MachineRowId, RosterMachine>, String> {
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
) -> Result<BTreeMap<MachineRowId, RosterMachine>, String> {
    let mut roster = BTreeMap::new();
    for row in rows {
        let [SqliteValue::Text(id), SqliteValue::Text(document)] = row.as_slice() else {
            return Err(format!("machine query returned an invalid row: {row:?}"));
        };
        let id = MachineRowId::try_new(id.clone()).map_err(|error| error.to_string())?;
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
