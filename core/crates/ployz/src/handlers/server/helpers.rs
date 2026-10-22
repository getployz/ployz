use std::{net::IpAddr, sync::Arc};

use clap::ArgMatches;
use ployz_core::{
    EnrollRequest, Enrolled, InspectRequest, LeaveRequest, Left, LocalMachinePhase,
    MACHINE_START_WAIT, Machine, MachineDetails, MachineName, MachineToken, PinnedAddresses,
    PublicIpDiscovery, RpcErrorCode, TokenRequest, op,
};

use super::parse_endpoints;
use crate::ui::Hint;
use crate::{
    connect::{Client, ConnectError, SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections, Transport},
    handlers::{Error, string_values},
};

pub(super) fn token_request(matches: &ArgMatches) -> Result<InspectRequest, Error> {
    let public_ip = match matches
        .get_one::<String>("public-ip")
        .map(String::as_str)
        .unwrap_or("auto")
    {
        "auto" => PublicIpDiscovery::Auto,
        "" | "none" => PublicIpDiscovery::Disabled,
        value => PublicIpDiscovery::Override(public_ip(value)?),
    };
    Ok(InspectRequest {
        advertised_endpoints: parse_endpoints(&string_values(matches, "wg-endpoint"))?,
        token: Some(TokenRequest { public_ip }),
        ..Default::default()
    })
}

/// A public IP the user gave: refused when no visitor could reach it, warned about when only a
/// private network could (#2914).
pub(super) fn public_ip(value: &str) -> Result<IpAddr, Error> {
    let ip = value
        .parse::<IpAddr>()
        .map_err(|_| Error::usage(format!("invalid public IP {value:?}")))?;
    let unreachable = match ip {
        ip if ip.is_unspecified() => Some("an unspecified"),
        ip if ip.is_loopback() => Some("a loopback"),
        ip if ip.is_multicast() => Some("a multicast"),
        IpAddr::V4(v4) if v4.is_link_local() => Some("a link-local"),
        IpAddr::V6(v6) if v6.is_unicast_link_local() => Some("a link-local"),
        IpAddr::V4(v4) if v4.is_broadcast() => Some("the broadcast"),
        IpAddr::V4(_) | IpAddr::V6(_) => None,
    };
    if let Some(kind) = unreachable {
        return Err(Error::usage(format!(
            "{ip} is {kind} address, which no visitor can reach. Give the address the internet reaches this Server at, or none."
        )));
    }
    let private = match ip {
        IpAddr::V4(v4) => v4.is_private(),
        IpAddr::V6(v6) => v6.is_unique_local(),
    };
    if private {
        crate::ui::warn(format!(
            "{ip} is a private address: only its own network reaches it, not the internet."
        ));
    }
    Ok(ip)
}

pub(super) fn pinned(request: &InspectRequest) -> PinnedAddresses {
    PinnedAddresses::chosen(
        request
            .token
            .as_ref()
            .map_or(&PublicIpDiscovery::Auto, |token| &token.public_ip),
        &request.advertised_endpoints,
    )
}

/// Inspect a newcomer for the token a Cluster registers it with.
pub(in crate::handlers) async fn inspect_token(
    client: &mut Client,
    request: InspectRequest,
) -> Result<(MachineDetails, MachineToken), Error> {
    let mut details = client.call_repeatable::<op::Inspect>(request, None).await?;
    let token = details.token.take().ok_or_else(|| {
        Error::coded(
            RpcErrorCode::Unsupported,
            format!(
                "{} runs a Ployz that cannot report its join token. Upgrade Ployz on this Server, then retry.",
                client.connection()
            ),
        )
    })?;
    Ok((details, token))
}

pub(in crate::handlers) fn machine_name(
    requested: Option<MachineName>,
    token: &MachineToken,
) -> Result<MachineName, Error> {
    match requested {
        Some(name) => Ok(name),
        None if token.runtime.hostname.is_empty() => Err(Error::usage(
            "Server name is required because the remote hostname is empty",
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

pub(in crate::handlers) async fn connect_direct(
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
            check_listed(&mut client).await?;
            Ok(client)
        },
    )
    .await
    .map_err(|error| Error::from(error).context(readiness_timeout_message(timeout_message)))
}

/// One readiness attempt: Participating and listed among its own Machines.
///
/// A founder turns Participating before its publisher writes its Machine row,
/// so a Deploy right after Participating can see an empty Deploy Snapshot.
pub(in crate::handlers) async fn check_listed(client: &mut Client) -> Result<(), ConnectError> {
    let details = client
        .call_repeatable::<op::Inspect>(InspectRequest::default(), None)
        .await?;
    if details.phase != LocalMachinePhase::Participating {
        return Err(ConnectError::Attempt(
            format!("Server phase is {}", details.phase.as_str().escape_debug()).into(),
        ));
    }
    // The published row is the participating identity, not the local key's id.
    let id = details.machine.map_or(details.id, |machine| machine.id);
    // A Participating daemon still opening its Cluster store, as after the
    // post-enroll restart, answers Unavailable: not ready yet, not a refusal.
    let machines = client.machines().await;
    if let Err(ConnectError::Remote(error)) = &machines
        && error.code == RpcErrorCode::Unavailable
    {
        return Err(ConnectError::Attempt(error.message.clone().into()));
    }
    let machines = machines?;
    if !machines.iter().any(|machine| machine.machine.id == id) {
        return Err(ConnectError::Attempt(
            "Server is not yet published in the Cluster store".into(),
        ));
    }
    Ok(())
}

/// Enroll, resending until a reply arrives: a resend replays the stored enrollment.
pub(in crate::handlers) async fn enroll(
    client: &mut Client,
    request: EnrollRequest,
) -> Result<Machine, Error> {
    let enrolled = client
        .call_repeatable_for::<op::Enroll>(request, None, None, None, MACHINE_START_WAIT)
        .await
        .map_err(|error| match error {
            crate::setup_retry::Error::Permanent(error) => error,
            exhausted => ConnectError::Exhausted(Box::new(exhausted)),
        })?;
    match enrolled {
        Enrolled::Enrolled { machine } => Ok(machine),
        Enrolled::Refused { error, .. } => Err(ConnectError::Remote(error).into()),
    }
}

/// Leave the Cluster, resending until a reply arrives: a resend on a Machine
/// that already left replies with its phase.
pub(in crate::handlers) async fn leave(client: &mut Client) -> Result<(), Error> {
    let left = client
        .call_repeatable_for::<op::Leave>(LeaveRequest {}, None, None, None, MACHINE_START_WAIT)
        .await
        .map_err(|error| match error {
            crate::setup_retry::Error::Permanent(error) => error,
            exhausted => ConnectError::Exhausted(Box::new(exhausted)),
        })?;
    match left {
        Left::Left { .. } => Ok(()),
        Left::Refused { error, .. } => Err(ConnectError::Remote(error).into()),
    }
}

/// The CLI can't see why the daemon isn't ready, so it names where the cause
/// shows rather than prescribing a recovery for one it never observed.
pub(in crate::handlers) fn readiness_timeout_message(message: &str) -> String {
    format!(
        "{message}; check `journalctl -u ployz` (a first Corrosion image pull can take several minutes); if it shows ployzd `refusing to` change a network, follow the safe recovery that line ends with"
    )
}

pub(in crate::handlers) fn confirm(yes: bool, question: &str) -> Result<(), Error> {
    if yes {
        return Ok(());
    }
    crate::ui::note(
        "This removes Ployz-managed containers and resets this machine's cluster membership.",
    );
    crate::ui::note(
        "Volume data will not be erased, but will lose access through the current cluster.",
    );
    crate::ui::confirm(
        question,
        || {
            Error::coded(
                RpcErrorCode::ConfirmationRequired,
                "Resetting the Server needs confirmation. No changes made.",
            )
            .hint(Hint::Retry(crate::handlers::typed_with(&["--yes"])))
        },
        "Cancelled. The Server was not reset.",
    )
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
            "Server name is required because the remote hostname is empty"
        );
    }

    #[test]
    fn readiness_timeout_names_the_start_delay_and_where_a_refusal_shows() {
        let message = readiness_timeout_message("initial Server did not become ready");
        assert!(
            message.contains("initial Server did not become ready"),
            "{message}"
        );
        assert!(message.contains("journalctl -u ployz"), "{message}");
        assert!(message.contains("Corrosion"), "{message}");
        assert!(message.contains("`refusing to`"), "{message}");
    }

    #[test]
    fn readiness_timeout_never_claims_a_docker_network_conflict() {
        // The CLI never observes the daemon's refusal, so it must not prescribe
        // removing a network the daemon may never have refused (#2972).
        let message = readiness_timeout_message("initial Server did not become ready");
        assert!(
            !message.contains(DOCKER_NETWORK_CONFLICT_RECOVERY),
            "{message}"
        );
        assert!(!message.contains("docker network rm"), "{message}");
        assert!(!message.contains("systemctl stop"), "{message}");
    }

    fn token_with_hostname(hostname: &str) -> MachineToken {
        MachineToken {
            id: ployz_core::MachineId::random(),
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
