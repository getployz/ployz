use std::{
    io::{self, IsTerminal, Write},
    sync::Arc,
};

use clap::ArgMatches;
use ployz_core::{
    DOCKER_NETWORK_CONFLICT_RECOVERY, InspectRequest, LocalMachinePhase, MACHINE_START_WAIT,
    MachineName, MachineToken, MachineTokenRequest, PublicIpDiscovery, op,
};

use super::parse_endpoints;
use crate::{
    connect::{Client, ConnectError, SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections, Transport},
    handlers::{Error, string_values},
};

pub(super) fn token_request(matches: &ArgMatches) -> Result<MachineTokenRequest, Error> {
    let port = *matches
        .get_one::<u16>("wg-port")
        .expect("WireGuard port has a default");
    let public_ip = match matches
        .get_one::<String>("public-ip")
        .map(String::as_str)
        .unwrap_or("auto")
    {
        "auto" => PublicIpDiscovery::Auto,
        "" | "none" => PublicIpDiscovery::Disabled,
        value => PublicIpDiscovery::Override(
            value
                .parse()
                .map_err(|_| Error::usage(format!("invalid public IP {value:?}")))?,
        ),
    };
    Ok(MachineTokenRequest {
        advertised_endpoints: parse_endpoints(&string_values(matches, "wg-endpoint"), port)?,
        public_ip,
        wireguard_port: port,
    })
}

pub(in crate::handlers) fn machine_name(
    requested: Option<MachineName>,
    token: &MachineToken,
) -> Result<MachineName, Error> {
    match requested {
        Some(name) => Ok(name),
        None if token.runtime.hostname.is_empty() => Err(Error::usage(
            "Machine name is required because the remote hostname is empty",
        )),
        None => Ok(MachineName::parse(
            token.runtime.hostname.to_ascii_lowercase(),
        )?),
    }
}

pub(super) fn configure_ssh_key(
    mut connection: Connection,
    key: Option<&str>,
) -> Result<Connection, Error> {
    if matches!(connection.transport(), Transport::Ssh { .. })
        && let Some(key) = key
    {
        connection = connection.with_ssh_key_file(key)?;
    }
    Ok(connection)
}

pub(super) async fn connect_direct(
    matches: &ArgMatches,
    connection: &Connection,
) -> Result<Client, ConnectError> {
    connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![connection.clone()],
        },
        Arc::new(SystemConnector::default().with_ssh_timeout(crate::cli::ssh_timeout(matches))),
    )
    .await
}

pub(super) async fn reconnect_direct(
    matches: &ArgMatches,
    connection: &Connection,
) -> Result<Client, Error> {
    crate::setup_retry::run(
        &mut (),
        &format!("Reconnecting to {connection}"),
        crate::setup_retry::WAIT,
        ConnectError::is_setup_retryable,
        async |_| connect_direct(matches, connection).await,
    )
    .await
    .map_err(Into::into)
}

pub(super) async fn wait_direct_participating(
    matches: &ArgMatches,
    connection: &Connection,
    timeout_message: &str,
) -> Result<Client, Error> {
    crate::setup_retry::run(
        &mut (),
        &format!("Waiting for {connection} to participate"),
        MACHINE_START_WAIT,
        ConnectError::is_setup_retryable,
        async |_| {
            let mut client = connect_direct(matches, connection).await?;
            let details = client
                .call_repeatable::<op::Inspect>(InspectRequest::default(), None)
                .await?;
            if details.phase != LocalMachinePhase::Participating {
                return Err(ConnectError::Attempt(
                    format!("Machine phase is {}", details.phase.as_str().escape_debug()).into(),
                ));
            }
            Ok(client)
        },
    )
    .await
    .map_err(|error| {
        Error::usage(format!(
            "{}: {error}",
            readiness_timeout_message(timeout_message)
        ))
    })
}

/// Recover a lost Initialize reply without initializing or resetting twice.
pub(in crate::handlers) async fn initialize(
    client: &mut Client,
    request: ployz_core::InitializeRequest,
) -> Result<ployz_core::Initialized, Error> {
    let identity = client
        .call_repeatable::<op::Inspect>(InspectRequest::default(), None)
        .await?;
    let name = request.name.clone();
    match client.call_unretried::<op::Initialize>(request, None).await {
        Ok(initialized) => Ok(initialized),
        Err(error) if error.is_setup_retryable() => {
            let details = observe_mutation(
                client,
                "Initialization",
                &error,
                MACHINE_START_WAIT,
                |details| {
                    details.phase == LocalMachinePhase::Participating
                        && details
                            .machine
                            .as_ref()
                            .is_some_and(|machine| machine.name == name)
                },
            )
            .await?;
            if details.id != identity.id || details.public_key != identity.public_key {
                return Err(Error::usage(
                    "Initialization outcome belongs to a different Machine identity; inspect the Machine before retrying; do not reset it",
                ));
            }
            Ok(ployz_core::Initialized {
                machine: details
                    .machine
                    .expect("observed predicate verified the initialized Machine"),
            })
        }
        Err(error) => Err(error.into()),
    }
}

/// Check local state after an interrupted reset instead of issuing another reset.
pub(in crate::handlers) async fn reset(client: &mut Client) -> Result<(), Error> {
    match client
        .call_unretried::<op::Reset>(ployz_core::ResetRequest {}, None)
        .await
    {
        Ok(_) => Ok(()),
        Err(error) if error.is_setup_retryable() => observe_mutation(
            client,
            "Reset",
            &error,
            crate::setup_retry::WAIT,
            |details| details.phase == LocalMachinePhase::Uninitialized,
        )
        .await
        .map(drop),
        Err(error) => Err(error.into()),
    }
}

/// A lost Join reply is success only when the assigned identity is observed joining.
pub(in crate::handlers) async fn join(
    client: &mut Client,
    request: ployz_core::JoinRequest,
) -> Result<(), Error> {
    let assigned = request.registration.assigned_machine.id;
    match client.call_unretried::<op::Join>(request, None).await {
        Ok(_) => Ok(()),
        Err(error) if error.is_setup_retryable() => {
            observe_mutation(client, "Join", &error, MACHINE_START_WAIT, |details| {
                details.id == assigned
                    && matches!(
                        details.phase,
                        LocalMachinePhase::Joining | LocalMachinePhase::Participating
                    )
            })
            .await
            .map(drop)
        }
        Err(error) => Err(error.into()),
    }
}

async fn observe_mutation(
    client: &mut Client,
    operation: &str,
    original: &ConnectError,
    wait: std::time::Duration,
    observed: impl Fn(&ployz_core::MachineDetails) -> bool,
) -> Result<ployz_core::MachineDetails, Error> {
    crate::setup_retry::run(client, &format!("Checking {operation} outcome"), wait,
        ConnectError::is_setup_retryable,
        async |client| {
            let details = client.call_repeatable::<op::Inspect>(InspectRequest::default(), None).await?;
            if observed(&details) { Ok(details) } else { Err(ConnectError::Attempt(format!("Machine phase is {}; expected {operation} outcome not yet observed", details.phase.as_str().escape_debug()).into())) }
        },
    ).await.map_err(|error| Error::usage(format!("{operation} may have completed: {original}; could not confirm the resulting Machine state: {error}; inspect the Machine before retrying; do not reset it")))
}

pub(in crate::handlers) fn readiness_timeout_message(message: &str) -> String {
    format!(
        "{message}; check `journalctl -u ployz` (a first Corrosion image pull can take several minutes); if ployzd refused a Docker network, safe recovery: {DOCKER_NETWORK_CONFLICT_RECOVERY}"
    )
}

pub(in crate::handlers) fn confirm(yes: bool, prompt: &str) -> Result<(), Error> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Error::usage(format!(
            "cannot confirm {} without a terminal; pass --yes",
            prompt.escape_debug()
        )));
    }
    println!("{prompt}");
    println!("This removes Ployz-managed containers and resets this machine's cluster membership.");
    println!("Volume data will not be erased, but will lose access through the current cluster.");
    print!("Type yes to confirm, or press Enter to cancel: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(Error::usage("Cancelled. The machine was not reset."))
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{DOCKER_NETWORK_CONFLICT_RECOVERY, MACHINE_API_PORT, MachineToken};

    use super::*;

    #[test]
    fn machine_add_only_configures_keys_for_ssh_connections() {
        let key = "/tmp/id_ed25519";
        let ssh = configure_ssh_key("root@example.com".parse().unwrap(), Some(key)).unwrap();
        assert_eq!(ssh.ssh_key_file(), Some(std::path::Path::new(key)));

        for destination in [
            format!("tcp://127.0.0.1:{MACHINE_API_PORT}"),
            "unix:///tmp/ployz.sock".into(),
        ] {
            let connection = configure_ssh_key(destination.parse().unwrap(), Some(key)).unwrap();
            assert_eq!(connection.ssh_key_file(), None, "destination {destination}");
        }
    }

    #[test]
    fn init_derives_a_machine_name_by_lowercasing_a_dns_label_hostname() {
        assert_eq!(
            machine_name(None, &token_with_hostname("Vultr1"))
                .unwrap()
                .as_str(),
            "vultr1"
        );
        assert_eq!(
            machine_name(None, &token_with_hostname("machine-a"))
                .unwrap()
                .as_str(),
            "machine-a"
        );
    }

    #[test]
    fn init_requires_a_name_when_the_remote_hostname_is_empty() {
        let token = token_with_hostname("");
        assert_eq!(
            machine_name(None, &token).unwrap_err().to_string(),
            "Machine name is required because the remote hostname is empty"
        );
    }

    #[test]
    fn readiness_timeout_names_the_start_delay_and_docker_network_recovery() {
        let message = readiness_timeout_message("initial Machine did not become ready");
        assert!(
            message.contains("initial Machine did not become ready"),
            "{message}"
        );
        assert!(message.contains("journalctl -u ployz"), "{message}");
        assert!(message.contains("Corrosion"), "{message}");
        assert!(
            message.contains(DOCKER_NETWORK_CONFLICT_RECOVERY),
            "{message}"
        );
    }

    fn token_with_hostname(hostname: &str) -> MachineToken {
        MachineToken {
            public_key: ployz_core::WireGuardPublicKey([0; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            runtime: ployz_core::MachineRuntime {
                hostname: hostname.into(),
                ..Default::default()
            },
            memory_total_bytes: None,
            disk_total_bytes: None,
            disk_available_bytes: None,
        }
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod setup_tests;
