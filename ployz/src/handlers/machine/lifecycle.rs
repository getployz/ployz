use std::{
    io::{self, IsTerminal, Write},
    sync::Arc,
    time::Duration,
};

use clap::ArgMatches;
use ployz_core::{
    InspectRequest, JoinRequest, LocalMachinePhase, Machine, MachineName, MachineSelector,
    MachineTokenRequest, NameMatches, PublicIpDiscovery, RegisterRequest,
    RemoveLocalMachineRequest, RemoveMachineRequest, RpcError, RpcErrorCode, RpcRequest,
    removal_decision, resolve_machine_selector,
};

use super::{ConnectionOptions, machine_list, parse_endpoints, runtime, target};
use crate::{
    connect::{Client, ConnectError, SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections},
    handlers::{Error, leaf_matches, string_values},
};

fn select_machine(
    machines: &[ployz_core::MachineObservation],
    selector: &str,
) -> Result<Machine, Error> {
    let selector = MachineSelector::parse(selector).map_err(|error| error.to_string())?;
    match resolve_machine_selector(&selector, machines.iter().map(|entry| &entry.machine)) {
        NameMatches::None => Err(format!("Machine {selector:?} was not found")),
        NameMatches::One(machine) => Ok(machine.clone()),
        NameMatches::Ambiguous(matches) => Err(format!(
            "Machine name {selector:?} is ambiguous: {}",
            matches
                .into_iter()
                .map(|machine| machine.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

pub(in crate::handlers) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let options = ConnectionOptions::from_matches(root)?;
    let matches = leaf_matches(root);
    let selector = target(matches, "machine")?.to_owned();
    let no_reset = matches.get_flag("no-reset");
    let yes = matches.get_flag("yes");
    let (mut config, context_name) = options.active_config()?;
    runtime()?.block_on(async {
        let mut client = options.connect().await?;
        let machines = machine_list(&mut client).await?;
        let selected = select_machine(&machines, &selector)?;
        let selected_target = MachineSelector::from(&selected.id);
        let current = client
            .describe_contract()
            .await
            .map_err(|error| error.to_string())?
            .machine_id;
        let prepare_target =
            removal_decision(selected.id == current, machines.len(), no_reset, true)
                .map_err(|error| error.to_string())?;
        confirm(
            yes,
            &format!("Remove Machine {} ({})?", selected.name, selected.id),
        )?;

        // TODO(UT-055): do not reroute away from the current entry before removal.
        // TODO(UT-056): there is no drain or unschedulable phase before cleanup.
        let mut removed_by_target = false;
        if prepare_target {
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
                Err(ConnectError::Remote(RpcError {
                    code: RpcErrorCode::Unavailable,
                    message,
                    ..
                })) => eprintln!("WARNING: target is unreachable; removing shared rows: {message}"),
                Err(
                    error @ (ConnectError::Rpc(_)
                    | ConnectError::Attempt(_)
                    | ConnectError::AllFailed { .. }),
                ) => {
                    eprintln!("WARNING: target is unreachable; removing shared rows: {error}");
                }
                Err(error) => return Err(error.to_string()),
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
        let context = config
            .contexts
            .get_mut(&context_name)
            .expect("active context was validated");
        if let Some(index) = context
            .connections
            .iter()
            .position(|connection| connection.machine_id() == Some(&selected.id))
        {
            context.connections.remove(index);
        }
        config.save().map_err(|error| error.to_string())?;
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
    let mut connection = destination
        .parse::<Connection>()
        .map_err(|error| error.to_string())?;
    if let Some(key) = matches.get_one::<String>("ssh-key") {
        connection = connection
            .with_ssh_key_file(key)
            .map_err(|error| error.to_string())?;
    }
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
    let port = target(matches, "wg-port")?
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| "WireGuard port must be between 1 and 65535".to_owned())?;
    let endpoints = parse_endpoints(&string_values(matches, "wg-endpoint"), port)?;
    let wireguard_mtu = matches
        .get_one::<String>("wg-mtu")
        .map(|value| {
            value
                .parse::<u32>()
                .ok()
                .filter(|mtu| *mtu != 0)
                .ok_or_else(|| "WireGuard MTU must be positive".to_owned())
        })
        .transpose()?;
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
            .request(RpcRequest::inspect(InspectRequest::default()), None)
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

async fn connect_direct(connection: &Connection) -> Result<Client, Error> {
    connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![connection.clone()],
        },
        Arc::new(SystemConnector::default()),
    )
    .await
    .map_err(|error| error.to_string())
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
        return Err(format!(
            "cannot confirm {prompt:?} without a terminal; pass --yes"
        ));
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
