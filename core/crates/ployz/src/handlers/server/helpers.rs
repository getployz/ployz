use std::sync::Arc;

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
        value => PublicIpDiscovery::Override(
            value
                .parse()
                .map_err(|_| Error::usage(format!("invalid public IP {value:?}")))?,
        ),
    };
    Ok(InspectRequest {
        advertised_endpoints: parse_endpoints(&string_values(matches, "wg-endpoint"))?,
        token: Some(TokenRequest { public_ip }),
        ..Default::default()
    })
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

/// The `--name` the user passed, refused with the normalised form when it isn't a Machine Name.
pub(in crate::handlers) fn requested_name(
    matches: &ArgMatches,
) -> Result<Option<MachineName>, Error> {
    let Some(requested) = matches.get_one::<String>("name") else {
        return Ok(None);
    };
    MachineName::parse(requested.as_str())
        .map(Some)
        .map_err(|error| match MachineName::from_hostname(requested) {
            Some(suggested) => Error::caused(
                RpcErrorCode::InvalidArgument,
                format!("{requested:?} is not a Server name. Try --name {suggested}."),
                error,
            ),
            None => error.into(),
        })
}

pub(in crate::handlers) fn machine_name(
    requested: Option<MachineName>,
    token: &MachineToken,
) -> MachineName {
    requested.unwrap_or_else(|| hostname_machine_name(&token.runtime.hostname))
}

/// The Machine Name a Server takes from its hostname when none was asked for, said on stderr.
pub(crate) fn hostname_machine_name(hostname: &str) -> MachineName {
    let name = MachineName::from_hostname(hostname).unwrap_or_else(|| {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        MachineName::parse(format!("server-{}", &suffix[..8]))
            .expect("server- and hex digits make a DNS label")
    });
    crate::ui::note(format_args!(
        "Naming this Server {name}, from its hostname {hostname:?}."
    ));
    name
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
    fn an_unnamed_server_takes_its_hostname_normalised() {
        let long = "x".repeat(70);
        for (hostname, name) in [
            ("Vultr1", "vultr1"),
            ("machine-a", "machine-a"),
            ("HP-WIN11", "hp-win11"),
            ("build_box.lan", "build-box-lan"),
            (long.as_str(), &long[..63]),
        ] {
            assert_eq!(
                machine_name(None, &token_with_hostname(hostname)).as_str(),
                name
            );
        }
    }

    #[test]
    fn an_unusable_hostname_falls_back_to_a_generated_name() {
        for hostname in ["", "___"] {
            let name = machine_name(None, &token_with_hostname(hostname));
            let suffix = name.as_str().strip_prefix("server-").unwrap();
            assert_eq!(suffix.len(), 8, "{name}");
        }
    }

    #[test]
    fn a_requested_name_wins_over_the_hostname() {
        let requested = MachineName::parse("web-1").unwrap();
        assert_eq!(
            machine_name(Some(requested.clone()), &token_with_hostname("HP-WIN11")),
            requested
        );
    }

    #[test]
    fn an_invalid_requested_name_is_refused_with_its_normalised_form() {
        let matches = |name: &str| {
            clap::Command::new("add")
                .arg(clap::Arg::new("name").long("name"))
                .get_matches_from(["add", "--name", name])
        };
        assert_eq!(
            crate::ui::chain_text(&requested_name(&matches("HP-WIN11")).unwrap_err()),
            "\"HP-WIN11\" is not a Server name. Try --name hp-win11: invalid Machine Name \"HP-WIN11\": a 1-63 character lowercase DNS label"
        );
        assert_eq!(
            requested_name(&matches("___")).unwrap_err().to_string(),
            "invalid Machine Name \"___\": a 1-63 character lowercase DNS label"
        );
        assert_eq!(
            requested_name(&matches("web-1")).unwrap().unwrap().as_str(),
            "web-1"
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
