use std::{net::IpAddr, sync::Arc};

use clap::ArgMatches;
use ployz_core::{
    EnrollForRecoveryRequest, EnrollRequest, Enrolled, InspectRequest, LeaveRequest, Left,
    LocalMachinePhase, MACHINE_START_WAIT, Machine, MachineDetails, MachineName, MachineToken,
    PinnedAddresses, PublicIpDiscovery, RecoverySeed, Rpc, RpcErrorCode, TokenRequest, op,
};

use super::parse_endpoints;
use crate::ui::Hint;
use crate::{
    connect::{Client, ConnectError, SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections, Transport},
    handlers::{Error, string_values},
};

pub(in crate::handlers) fn token_request(matches: &ArgMatches) -> Result<InspectRequest, Error> {
    let request = checked_token_request(matches)?;
    if let Some(TokenRequest {
        public_ip: PublicIpDiscovery::Override(ip),
    }) = &request.token
    {
        warn_private(*ip);
    }
    Ok(request)
}

/// [`token_request`] without its warnings, for a check before the request it builds is used.
pub(super) fn checked_token_request(matches: &ArgMatches) -> Result<InspectRequest, Error> {
    let public_ip = match matches
        .get_one::<String>("public-ip")
        .map(String::as_str)
        .unwrap_or("auto")
    {
        "auto" => PublicIpDiscovery::Auto,
        "" | "none" => PublicIpDiscovery::Disabled,
        value => PublicIpDiscovery::Override(reachable_public_ip(value)?),
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
    let ip = reachable_public_ip(value)?;
    warn_private(ip);
    Ok(ip)
}

fn reachable_public_ip(value: &str) -> Result<IpAddr, Error> {
    let ip = value
        .parse::<IpAddr>()
        .map_err(|_| Error::usage(format!("invalid public IP {value:?}")))?;
    let unreachable = match ip.to_canonical() {
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
    Ok(ip)
}

fn warn_private(ip: IpAddr) {
    let private = match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_private(),
        IpAddr::V6(v6) => v6.is_unique_local(),
    };
    if private {
        crate::ui::warn(format!(
            "{ip} is a private address: only its own network reaches it, not the internet."
        ));
    }
}

pub(in crate::handlers) fn pinned(request: &InspectRequest) -> PinnedAddresses {
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
        .map_err(|error| {
            let failure = Error::from(error);
            match MachineName::from_hostname(requested) {
                Some(suggested) => failure.hint(Hint::Closest(suggested.into())),
                None => failure,
            }
        })
}

/// The requested name, or the hostname's normalised form, said on stderr. A hostname with
/// nothing usable falls back to a name from the Machine ID, so a retry asks under the same name.
pub(in crate::handlers) fn machine_name(
    requested: Option<MachineName>,
    token: &MachineToken,
) -> MachineName {
    if let Some(name) = requested {
        return name;
    }
    let hostname = &token.runtime.hostname;
    let name = MachineName::from_hostname(hostname).unwrap_or_else(|| {
        MachineName::parse(format!("server-{}", &token.id.as_str()[..8]))
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

/// The daemon restarts after it enrolls, so a first failed attempt is the expected path,
/// not a reachability problem: this setup reached the same connection moments before (#3371).
pub(in crate::handlers) const STARTING: crate::setup_retry::Expected =
    crate::setup_retry::Expected("Waiting for the Server's daemon to start…");

pub(super) async fn wait_direct_participating(
    matches: &ArgMatches,
    connection: &Connection,
    timeout_message: &str,
) -> Result<Client, Error> {
    let mut joining = Joining::default();
    crate::setup_retry::run_expecting(
        &mut joining,
        &format!("Waiting for {connection} to participate"),
        Some(STARTING),
        MACHINE_START_WAIT,
        ConnectError::is_setup_retryable,
        async |joining| {
            let mut client = connect_direct(matches, connection).await?;
            check_listed(&mut client, joining).await?;
            Ok(client)
        },
    )
    .await
    .map_err(|error| Error::from(error).context(joining.timeout_message(timeout_message)))
}

/// How long an answering daemon stays Joining before the wait says what it waits on.
/// A healthy joiner catches up within seconds of its post-enroll restart.
const JOINING_NOTICE_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

const STILL_JOINING: &str = "The Server's daemon is running but still joining: it hasn't caught up with the Cluster from your other Servers yet. They reach each other over WireGuard on UDP 51820, so check that no firewall between them blocks it.";

/// Whether a readiness wait has seen the daemon answer while Joining, and since when (#3597).
#[derive(Debug, Default)]
pub(in crate::handlers) struct Joining {
    since: Option<tokio::time::Instant>,
    told: bool,
}

impl Joining {
    fn seen(&mut self, phase: &LocalMachinePhase) {
        if *phase != LocalMachinePhase::Joining {
            self.since = None;
            return;
        }
        let since = *self.since.get_or_insert_with(tokio::time::Instant::now);
        if !self.told && since.elapsed() >= JOINING_NOTICE_AFTER {
            crate::ui::note(STILL_JOINING);
            self.told = true;
        }
    }

    /// The timeout context: the joining cause once the daemon was last seen Joining, otherwise
    /// [`readiness_timeout_message`], since nothing showed why it isn't ready.
    pub(in crate::handlers) fn timeout_message(&self, message: &str) -> String {
        if self.since.is_some() {
            format!(
                "{message}; its daemon runs but stayed joining, so it never caught up with the Cluster from your other Servers; allow UDP 51820 between it and them in every firewall, including one your provider runs in front of them, then run the command again"
            )
        } else {
            readiness_timeout_message(message)
        }
    }
}

/// One readiness attempt: Participating and listed among its own Machines.
///
/// A founder turns Participating before its publisher writes its Machine row,
/// so a Deploy right after Participating can see an empty Deploy Snapshot.
pub(in crate::handlers) async fn check_listed(
    client: &mut Client,
    joining: &mut Joining,
) -> Result<(), ConnectError> {
    let details = client
        .call_repeatable::<op::Inspect>(InspectRequest::default(), None)
        .await?;
    joining.seen(&details.phase);
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
    enroll_with::<op::Enroll>(client, request).await
}

/// Enroll as the founder that recovers the Organization's Sealing Key instead of
/// generating one; resends replay the stored enrollment like [`enroll`].
pub(in crate::handlers) async fn enroll_for_recovery(
    client: &mut Client,
    enroll: EnrollRequest,
    recovery: RecoverySeed,
) -> Result<Machine, Error> {
    enroll_with::<op::EnrollForRecovery>(client, EnrollForRecoveryRequest { enroll, recovery })
        .await
}

async fn enroll_with<T: Rpc<Response = Enrolled>>(
    client: &mut Client,
    request: T::Request,
) -> Result<Machine, Error> {
    let enrolled = client
        .call_repeatable_for::<T>(request, None, None, None, MACHINE_START_WAIT)
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
            ("MY-SERVER", "my-server"),
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
    fn an_unusable_hostname_falls_back_to_a_name_from_the_machine_id() {
        for hostname in ["", "___"] {
            let token = token_with_hostname(hostname);
            let expected = format!("server-{}", &token.id.as_str()[..8]);
            assert_eq!(machine_name(None, &token).as_str(), expected);
            assert_eq!(
                machine_name(None, &token).as_str(),
                expected,
                "a retry keeps it"
            );
        }
    }

    #[test]
    fn a_requested_name_wins_over_the_hostname() {
        let requested = MachineName::parse("web-1").unwrap();
        assert_eq!(
            machine_name(Some(requested.clone()), &token_with_hostname("MY-SERVER")),
            requested
        );
    }

    #[test]
    fn an_invalid_requested_name_is_refused_with_its_normalised_form_as_a_hint() {
        let matches = |name: &str| {
            clap::Command::new("add")
                .arg(clap::Arg::new("name").long("name"))
                .get_matches_from(["add", "--name", name])
        };
        let refused = requested_name(&matches("MY-SERVER")).unwrap_err();
        assert_eq!(
            refused.to_string(),
            "invalid Machine Name \"MY-SERVER\": a 1-63 character lowercase DNS label"
        );
        assert_eq!(refused.report().code, RpcErrorCode::InvalidArgument);
        assert_eq!(refused.hints(), [Hint::Closest("my-server".into())]);
        let refused = requested_name(&matches("___")).unwrap_err();
        assert!(refused.hints().is_empty());
        assert_eq!(
            requested_name(&matches("web-1")).unwrap().unwrap().as_str(),
            "web-1"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_daemon_left_joining_is_named_as_not_catching_up_over_udp_51820() {
        let mut joining = Joining::default();
        joining.seen(&LocalMachinePhase::Joining);
        tokio::time::advance(JOINING_NOTICE_AFTER - std::time::Duration::from_secs(1)).await;
        joining.seen(&LocalMachinePhase::Joining);
        assert!(!joining.told, "a healthy joiner's first minute stays quiet");
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        joining.seen(&LocalMachinePhase::Joining);
        assert!(joining.told);
        let message = joining.timeout_message("the joined Server did not become ready");
        assert!(
            message.starts_with("the joined Server did not become ready; "),
            "{message}"
        );
        assert!(message.contains("stayed joining"), "{message}");
        assert!(message.contains("UDP 51820"), "{message}");
        assert!(!message.contains("Corrosion"), "{message}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_daemon_never_seen_joining_keeps_the_general_readiness_message() {
        let mut joining = Joining::default();
        joining.seen(&LocalMachinePhase::Joining);
        joining.seen(&LocalMachinePhase::Uninitialized);
        tokio::time::advance(JOINING_NOTICE_AFTER).await;
        assert_eq!(
            joining.timeout_message("not ready"),
            readiness_timeout_message("not ready")
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
