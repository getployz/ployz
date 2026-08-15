use std::{
    io::{self, IsTerminal, Write},
    sync::Arc,
    time::Duration,
};

use clap::ArgMatches;
use ployz_core::{MachineName, MachineToken, MachineTokenRequest, PublicIpDiscovery};

use super::parse_endpoints;
use crate::{
    connect::{Client, SystemConnector, connect_selected_with},
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

pub(super) fn machine_name(
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

pub(super) async fn connect_direct(connection: &Connection) -> Result<Client, Error> {
    Ok(connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![connection.clone()],
        },
        Arc::new(SystemConnector::default()),
    )
    .await?)
}

pub(super) async fn reconnect_direct(connection: &Connection) -> Result<Client, Error> {
    let mut last = None;
    for _ in 0..40 {
        match connect_direct(connection).await {
            Ok(client) => return Ok(client),
            Err(error) => last = Some(error),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(last.unwrap_or_else(|| Error::usage("Machine did not become reachable after reset")))
}

pub(super) fn confirm(yes: bool, prompt: &str) -> Result<(), Error> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Error::usage(format!(
            "cannot confirm {prompt:?} without a terminal; pass --yes"
        )));
    }
    print!("{prompt} [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(Error::usage("aborted"))
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::MachineToken;

    use super::*;

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

    fn token_with_hostname(hostname: &str) -> MachineToken {
        MachineToken {
            public_key: ployz_core::WireGuardPublicKey([0; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            runtime: ployz_core::MachineRuntime {
                hostname: hostname.into(),
                ..Default::default()
            },
        }
    }
}
