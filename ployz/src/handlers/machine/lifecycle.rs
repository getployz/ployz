use std::{
    io::{self, IsTerminal, Write},
    sync::Arc,
    time::Duration,
};

use clap::ArgMatches;
use ployz_core::{
    InspectRequest, JoinRequest, LocalMachinePhase, Machine, MachineName, MachineSelector,
    MachineTokenRequest, NameMatches, PublicIpDiscovery, RegisterRequest,
    RemoveLocalMachineRequest, RemoveMachineRequest, RpcRequest, resolve_machine_selector,
};

use super::{ConnectionOptions, machine_list, parse_endpoints, runtime, target};
use crate::{
    connect::{Client, ConnectError, SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections, Transport},
    handlers::{Error, leaf_matches, string_values},
};

fn select_machine(
    machines: &[ployz_core::MachineObservation],
    selector: &str,
) -> Result<Machine, Error> {
    let selector = MachineSelector::parse(selector).map_err(|error| error.to_string())?;
    match resolve_machine_selector(&selector, machines.iter().map(|entry| &entry.machine)) {
        NameMatches::None => Err(format!("Machine {selector:?} was not found").into()),
        NameMatches::One(machine) => Ok(machine.clone()),
        NameMatches::Ambiguous(matches) => Err(format!(
            "Machine name {selector:?} is ambiguous: {}",
            matches
                .into_iter()
                .map(|machine| machine.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into()),
    }
}

fn is_target_unreachable(error: &ConnectError) -> bool {
    matches!(
        error,
        ConnectError::Attempt(_) | ConnectError::AllFailed { .. }
    ) || matches!(
        error,
        ConnectError::Rpc(status) if status.code() == tonic::Code::Unavailable
    )
}

pub(in crate::handlers) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let options = ConnectionOptions::from_matches(root)?;
    let matches = leaf_matches(root);
    let selector = target(matches, "machine")?.to_owned();
    let no_reset = matches.get_flag("no-reset");
    let yes = matches.get_flag("yes");
    runtime()?.block_on(async {
        let mut client = options.connect().await?;
        let context_name = match client.connection_source() {
            ConnectionSource::Context(name) => Some(name.clone()),
            ConnectionSource::Direct | ConnectionSource::LocalSocket => None,
        };
        let machines = machine_list(&mut client).await?;
        let selected = select_machine(&machines, &selector)?;
        let selected_target = MachineSelector::from(&selected.id);
        let current = client
            .describe_contract()
            .await
            .map_err(|error| error.to_string())?
            .machine_id;
        if selected.id == current && machines.len() > 1 {
            return Err(
                "the current entry Machine cannot be removed while another Machine is visible"
                    .into(),
            );
        }
        confirm(
            yes,
            &format!("Remove Machine {} ({})?", selected.name, selected.id),
        )?;

        // TODO(UT-055): do not reroute away from the current entry before removal.
        // TODO(UT-056): there is no drain or unschedulable phase before cleanup.
        let mut removed_by_target = false;
        if !no_reset {
            match client
                .request(
                    RpcRequest::remove_local_machine(RemoveLocalMachineRequest {}),
                    Some(&selected_target),
                )
                .await
            {
                Ok(response) => {
                    let removed = response
                        .decode_local_machine_removed()
                        .map_err(|error| error.to_string())?;
                    if let Some(warning) = &removed.reset_warning {
                        eprintln!("WARNING: target reset failed: {warning}");
                    }
                    removed_by_target = true;
                }
                Err(error) if is_target_unreachable(&error) => {
                    eprintln!("WARNING: target is unreachable; removing shared rows: {error}");
                }
                Err(error) => return Err(error.to_string().into()),
            }
        }
        if !removed_by_target {
            client
                .request(
                    RpcRequest::remove_machine(RemoveMachineRequest {
                        machine_id: selected.id.clone(),
                    }),
                    None,
                )
                .await
                .map_err(|error| error.to_string())?
                .decode_machine_removed()
                .map_err(|error| error.to_string())?;
        }

        // TODO(UT-057): removal does not update hosted DNS or Caddy projections.
        if let Some(context_name) = context_name {
            let mut config = options.load_config()?;
            let context = config
                .contexts
                .get_mut(&context_name)
                .ok_or_else(|| format!("context {context_name:?} not found"))?;
            if let Some(index) = context
                .connections
                .iter()
                .position(|connection| connection.machine_id() == Some(&selected.id))
            {
                context.connections.remove(index);
            }
            config.save().map_err(|error| error.to_string())?;
        }
        println!("Removed Machine {} ({})", selected.name, selected.id);
        Ok::<_, Error>(())
    })
}

pub(in crate::handlers) fn add(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    if !matches.get_flag("no-caddy") {
        return Err("machine add currently requires --no-caddy".into());
    }
    let options = ConnectionOptions::from_matches(root)?;
    let (mut config, context_name) = options.active_config()?;
    let destination = target(matches, "destination")?;
    let connection = destination
        .parse::<Connection>()
        .map_err(|error| error.to_string())?;
    let mut connection = configure_ssh_key(
        connection,
        matches.get_one::<String>("ssh-key").map(String::as_str),
    )?;
    let name = matches
        .get_one::<String>("name")
        .map(|name| MachineName::parse(name).map_err(|error| error.to_string()))
        .transpose()?;
    let public_ip = matches
        .get_one::<String>("public-ip")
        .map(String::as_str)
        .unwrap_or("auto");
    let public_ip = match public_ip {
        "auto" => PublicIpDiscovery::Auto,
        "" | "none" => PublicIpDiscovery::Disabled,
        value => PublicIpDiscovery::Override(
            value
                .parse()
                .map_err(|_| format!("invalid public IP {value:?}"))?,
        ),
    };
    let port = *matches
        .get_one::<u16>("wg-port")
        .expect("WireGuard port has a default");
    let endpoints = parse_endpoints(&string_values(matches, "wg-endpoint"), port)?;
    let wireguard_mtu = matches.get_one::<u32>("wg-mtu").copied();
    let yes = matches.get_flag("yes");
    if !matches.get_flag("no-install") {
        crate::provisioning::provision(matches)?;
    }

    let assigned = runtime()?.block_on(async {
        let mut entry = options.connect().await?;
        let visible = machine_list(&mut entry).await?;
        let mut target_client = connect_direct(&connection).await?;
        let token_request = MachineTokenRequest {
            advertised_endpoints: endpoints,
            public_ip,
            wireguard_port: port,
        };
        let mut token = target_client
            .request(RpcRequest::machine_token(token_request.clone()), None)
            .await
            .map_err(|error| error.to_string())?
            .decode_machine_token()
            .cloned()
            .map_err(|error| error.to_string())?;
        let details = target_client
            .request(
                RpcRequest::inspect(InspectRequest {
                    advertised_endpoints: token.advertised_endpoints.clone(),
                    ..Default::default()
                }),
                None,
            )
            .await
            .map_err(|error| error.to_string())?
            .decode_machine_details()
            .cloned()
            .map_err(|error| error.to_string())?;
        if details.phase != LocalMachinePhase::Uninitialized {
            if visible
                .iter()
                .any(|machine| machine.machine.public_key == token.public_key)
            {
                return Err("Machine already belongs to this Cluster".into());
            }
            confirm(yes, "Reset the Machine before adding it to this Cluster?")?;
            target_client
                .request(RpcRequest::reset(), None)
                .await
                .map_err(|error| error.to_string())?
                .decode_reset_accepted()
                .map_err(|error| error.to_string())?;
            target_client = reconnect_direct(&connection).await?;
            token = target_client
                .request(RpcRequest::machine_token(token_request), None)
                .await
                .map_err(|error| error.to_string())?
                .decode_machine_token()
                .cloned()
                .map_err(|error| error.to_string())?;
        }
        let name = match name {
            Some(name) => name,
            None if token.runtime.hostname.is_empty() => {
                return Err("Machine name is required because the remote hostname is empty".into());
            }
            None => MachineName::parse(token.runtime.hostname.clone())
                .map_err(|error| error.to_string())?,
        };

        // TODO(UT-140): registration is intentionally unfenced and may succeed on a minority.
        let registration = entry
            .request(
                RpcRequest::register(RegisterRequest {
                    name,
                    public_key: token.public_key,
                    public_ip: token.public_ip,
                    advertised_endpoints: token.advertised_endpoints,
                    runtime: token.runtime,
                }),
                None,
            )
            .await
            .map_err(|error| error.to_string())?
            .decode_registered()
            .cloned()
            .map_err(|error| error.to_string())?;
        let assigned = registration.assigned_machine.clone();
        target_client
            .request(
                RpcRequest::join(JoinRequest {
                    registration,
                    wireguard_mtu,
                }),
                None,
            )
            .await
            .map_err(|error| error.to_string())?
            .decode_join_accepted()
            .map_err(|error| error.to_string())?;
        Ok::<_, Error>(assigned)
    })?;

    connection = connection.with_machine_id(assigned.id.clone());
    config
        .contexts
        .get_mut(&context_name)
        .expect("active context was validated")
        .connections
        .push(connection);
    config.save().map_err(|error| error.to_string())?;
    println!("Added Machine {} ({})", assigned.name, assigned.id);
    Ok(())
}

fn configure_ssh_key(mut connection: Connection, key: Option<&str>) -> Result<Connection, Error> {
    if matches!(connection.transport(), Transport::Ssh { .. })
        && let Some(key) = key
    {
        connection = connection
            .with_ssh_key_file(key)
            .map_err(|error| error.to_string())?;
    }
    Ok(connection)
}

async fn connect_direct(connection: &Connection) -> Result<Client, Error> {
    connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![connection.clone()],
        },
        Arc::new(SystemConnector::default()),
    )
    .await
    .map_err(|error| error.to_string().into())
}

async fn reconnect_direct(connection: &Connection) -> Result<Client, Error> {
    let mut last = None;
    for _ in 0..40 {
        match connect_direct(connection).await {
            Ok(client) => return Ok(client),
            Err(error) => last = Some(error),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(last.unwrap_or_else(|| "Machine did not become reachable after reset".into()))
}

fn confirm(yes: bool, prompt: &str) -> Result<(), Error> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(format!("cannot confirm {prompt:?} without a terminal; pass --yes").into());
    }
    print!("{prompt} [y/N] ");
    io::stdout().flush().map_err(|error| error.to_string())?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|error| error.to_string())?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err("aborted".into())
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{RpcError, RpcErrorCode};
    use serde_json::Value;

    use super::*;

    #[test]
    fn reached_target_cleanup_rejections_are_not_unreachable_fallbacks() {
        assert!(is_target_unreachable(&ConnectError::Rpc(Box::new(
            tonic::Status::unavailable("route failed")
        ))));
        assert!(!is_target_unreachable(&ConnectError::Rpc(Box::new(
            tonic::Status::unimplemented("older daemon")
        ))));
        assert!(!is_target_unreachable(&ConnectError::Remote(RpcError {
            code: RpcErrorCode::Unavailable,
            message: "Docker is unavailable".into(),
            details: Value::Null,
        })));
    }

    #[test]
    fn machine_add_only_configures_keys_for_ssh_connections() {
        let key = "/tmp/id_ed25519";
        let ssh = configure_ssh_key("root@example.com".parse().unwrap(), Some(key)).unwrap();
        assert_eq!(ssh.ssh_key_file(), Some(std::path::Path::new(key)));

        for destination in ["tcp://127.0.0.1:51000", "unix:///tmp/ployz.sock"] {
            let connection = configure_ssh_key(destination.parse().unwrap(), Some(key)).unwrap();
            assert_eq!(connection.ssh_key_file(), None, "destination {destination}");
        }
    }
}
