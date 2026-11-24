//! What enrolling asks of the Server's own daemon: the token it registers with, Enroll and
//! Leave, and the readiness check after it restarts. `server add`, `server add --standalone`
//! and Cloud enrollment share it.

use crate::cluster::{Call, Deadline, Wait};
use ployz_core::error_chain;
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use ployz_core::{
    AdvertisedEndpoint, EnrollForRecoveryRequest, EnrollRequest, Enrolled, Hint, InspectRequest,
    LeaveRequest, Left, LocalMachinePhase, MACHINE_START_WAIT, Machine, MachineDetails,
    MachineName, MachineToken, PinnedAddresses, PublicIpDiscovery, RecoverySeed, Rpc, RpcErrorCode,
    TokenRequest, op,
};

use crate::{
    connect::{Client, ConnectError, SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections},
    failure::Failure as Error,
};

/// [`token_request`] from the `--public-ip` and `--wg-endpoint` values given.
pub(crate) fn token_request_for(
    public_ip: Option<&str>,
    wg_endpoints: &[String],
) -> Result<InspectRequest, Error> {
    let request = checked_token_request_for(public_ip, wg_endpoints)?;
    if let Some(TokenRequest {
        public_ip: PublicIpDiscovery::Override(ip),
    }) = &request.token
    {
        warn_private(*ip);
    }
    Ok(request)
}

/// [`token_request_for`] without its warnings, for a check before the request it builds is used.
pub(crate) fn checked_token_request_for(
    public_ip: Option<&str>,
    wg_endpoints: &[String],
) -> Result<InspectRequest, Error> {
    let public_ip = match public_ip.unwrap_or("auto") {
        "auto" => PublicIpDiscovery::Auto,
        "" | "none" => PublicIpDiscovery::Disabled,
        value => PublicIpDiscovery::Override(reachable_public_ip(value)?),
    };
    Ok(InspectRequest {
        advertised_endpoints: parse_endpoints(wg_endpoints)?,
        token: Some(TokenRequest { public_ip }),
        ..Default::default()
    })
}

/// A public IP the user gave: refused when no visitor could reach it, warned about when only a
/// private network could (#2914).
pub(crate) fn public_ip(value: &str) -> Result<IpAddr, Error> {
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

pub(crate) fn pinned(request: &InspectRequest) -> PinnedAddresses {
    PinnedAddresses::chosen(
        request
            .token
            .as_ref()
            .map_or(&PublicIpDiscovery::Auto, |token| &token.public_ip),
        &request.advertised_endpoints,
    )
}

/// Inspect a newcomer for the token a Cluster registers it with.
pub(crate) async fn inspect_token(
    client: &mut Client,
    request: InspectRequest,
) -> Result<(MachineDetails, MachineToken), Error> {
    let mut details = client
        .call::<op::Inspect>(request, Call::read().waiting(Wait::standard()))
        .await?;
    let token = details.token.take().ok_or_else(|| {
        Error::coded(
            RpcErrorCode::Unsupported,
            format!(
                "{} runs a Ployz that cannot report its join token. Upgrade Ployz on this Server, then retry.",
                client.connection()
            ),
        )
    })?;
    let token = joining_as_approved(&details, token);
    Ok((details, token))
}

/// A Server still joining asks for the addresses its join was approved with. Its token
/// rediscovers them, and once its WireGuard is up they differ, so a rerun would ask the
/// admitting Server for a join it never approved and be refused.
fn joining_as_approved(details: &MachineDetails, mut token: MachineToken) -> MachineToken {
    if details.phase == LocalMachinePhase::Joining
        && let Some(machine) = details.machine.as_ref()
        && machine.id == token.id
        && machine.public_key == token.public_key
    {
        token.public_ip = machine.public_ip;
        token
            .advertised_endpoints
            .clone_from(&machine.advertised_endpoints);
    }
    token
}

/// [`requested_name`] from the `--name` value given.
pub(crate) fn machine_name_requested(
    requested: Option<&str>,
) -> Result<Option<MachineName>, Error> {
    let Some(requested) = requested else {
        return Ok(None);
    };
    MachineName::parse(requested).map(Some).map_err(|error| {
        let failure = Error::from(error);
        match MachineName::from_hostname(requested) {
            Some(suggested) => failure.hint(Hint::Closest(suggested.into())),
            None => failure,
        }
    })
}

/// The requested name, or the hostname's normalised form, said on stderr. A hostname with
/// nothing usable falls back to a name from the Machine ID, so a retry asks under the same name.
pub(crate) fn machine_name(requested: Option<MachineName>, token: &MachineToken) -> MachineName {
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

/// [`connect_direct`] with this SSH setup budget.
pub(crate) async fn connect_direct_within(
    ssh_timeout: std::time::Duration,
    connection: &Connection,
) -> Result<Client, ConnectError> {
    connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![connection.clone()],
        },
        Arc::new(SystemConnector::default().with_ssh_timeout(ssh_timeout)),
    )
    .await
}

pub(crate) use crate::setup_retry::STARTING;

/// How long an answering daemon stays Joining before the wait says what it waits on.
/// A healthy joiner catches up within seconds of its post-enroll restart.
const JOINING_NOTICE_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

const STILL_JOINING: &str = "The Server's daemon is running but still joining: it hasn't caught up with the Cluster from your other Servers yet. They reach each other over WireGuard on UDP 51820, so check that no firewall between them blocks it.";

/// Whether a readiness wait has seen the daemon answer while Joining, and since when (#3597).
#[derive(Debug, Default)]
pub(crate) struct Joining {
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

    /// `error` with [`Self::timeout_message`] when the wait ran out. A failure that
    /// stopped it early, as a startup refusal (#3703), supersedes any Joining seen
    /// before it.
    pub(crate) fn failed(
        &self,
        error: crate::setup_retry::Error<ConnectError>,
        message: &str,
    ) -> Error {
        let context = match &error {
            crate::setup_retry::Error::Exhausted { .. } => self.timeout_message(message),
            crate::setup_retry::Error::Permanent(_) => readiness_timeout_message(message),
        };
        Error::from(error).context(context)
    }

    /// The timeout context: the joining cause once the daemon was last seen Joining, otherwise
    /// [`readiness_timeout_message`], since nothing showed why it isn't ready.
    pub(crate) fn timeout_message(&self, message: &str) -> String {
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
pub(crate) async fn check_listed(
    client: &mut Client,
    joining: &mut Joining,
) -> Result<(), ConnectError> {
    let details = client
        .call::<op::Inspect>(
            InspectRequest::default(),
            Call::once().within(Deadline::SetupRead),
        )
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
pub(crate) async fn enroll(client: &mut Client, request: EnrollRequest) -> Result<Machine, Error> {
    Ok(enroll_with::<op::Enroll>(client, request).await?.0)
}

/// An approved join whose Server sent no confirmation. Its daemon predates confirmations, so
/// the admitting Server would never publish it.
pub(crate) fn confirmation_missing() -> Error {
    Error::conflict(
        "The joining Server's ployz is too old to finish an approved join. Update ployz on it, then rerun the same command.",
    )
}

/// Enroll as a member, with the confirmation an approved join returns for its driver to
/// relay in Register.
pub(crate) async fn enroll_member(
    client: &mut Client,
    request: EnrollRequest,
) -> Result<
    (
        Machine,
        Option<ployz_core::authority::join::JoinConfirmation>,
    ),
    Error,
> {
    enroll_with::<op::Enroll>(client, request).await
}

/// Enroll as the founder that recovers the Organization's Sealing Key instead of
/// generating one; resends replay the stored enrollment like [`enroll`].
pub(crate) async fn enroll_for_recovery(
    client: &mut Client,
    enroll: EnrollRequest,
    recovery: RecoverySeed,
) -> Result<Machine, Error> {
    Ok(
        enroll_with::<op::EnrollForRecovery>(client, EnrollForRecoveryRequest { enroll, recovery })
            .await?
            .0,
    )
}

async fn enroll_with<T: Rpc<Response = Enrolled>>(
    client: &mut Client,
    request: T::Request,
) -> Result<
    (
        Machine,
        Option<ployz_core::authority::join::JoinConfirmation>,
    ),
    Error,
> {
    let enrolled = client
        .call::<T>(request, Call::keyed().waiting(Wait::of(MACHINE_START_WAIT)))
        .await?;
    match enrolled {
        Enrolled::Enrolled {
            machine,
            join_confirmation,
        } => Ok((machine, join_confirmation)),
        Enrolled::Refused { error, .. } => Err(ConnectError::Remote(error).into()),
    }
}

/// Leave the Cluster, resending until a reply arrives: a resend on a Machine
/// that already left replies with its phase. `departing` is the Server as inspected
/// before the Leave, so a failed row removal names the id it had in its old Cluster.
pub(crate) async fn leave(client: &mut Client, departing: &MachineDetails) -> Result<(), Error> {
    let left = client
        .call::<op::Leave>(
            LeaveRequest {},
            Call::keyed().waiting(Wait::of(MACHINE_START_WAIT)),
        )
        .await?;
    match left {
        Left::Left { reset_warning, .. } => {
            if let Some(reason) = reset_warning {
                crate::ui::warn(unconfirmed_removal(departing, &reason));
            }
            Ok(())
        }
        Left::Refused { error, .. } => Err(ConnectError::Remote(error).into()),
    }
}

/// The Server's reset committed, but deleting its row from its own store failed, so
/// whether its old Cluster still shows it is unknown here.
fn unconfirmed_removal(departing: &MachineDetails, reason: &str) -> String {
    let id = &departing.id;
    let server = match &departing.machine {
        Some(machine) => format!("Server {} ({id})", machine.name),
        None => format!("Server {id}"),
    };
    error_chain::then_sentence(
        &error_chain::with_reason(
            format_args!("{server} reset, but couldn't confirm that its old Cluster removed it"),
            reason,
        ),
        &format!(
            "If that Cluster still shows it, whoever manages it can remove it there with `ployz server forget {id} --confirm dead`."
        ),
    )
}

/// The CLI can't see why the daemon isn't ready, so it names where the cause
/// shows rather than prescribing a recovery for one it never observed.
pub(crate) fn readiness_timeout_message(message: &str) -> String {
    format!(
        "{message}; check `journalctl -u ployz` (a first Corrosion image pull can take several minutes); if it shows ployzd `refusing to` change a network, follow the safe recovery that line ends with"
    )
}

/// Ask `question` before a reset, unless `--yes` was given: `ask` is this command with `--yes`.
pub(crate) fn confirm(ask: Option<crate::cli::Rerun>, question: &str) -> Result<(), Error> {
    let Some(yes) = ask else {
        return Ok(());
    };
    crate::ui::note(
        "This removes Ployz-managed containers and resets this machine's cluster membership.",
    );
    crate::ui::note(
        "Its writer copies of Volumes stay on its disk, read-only, but its mirrors can be deleted, so don't reset it while a mirror there may be a Volume's last copy.",
    );
    crate::ui::confirm(
        question,
        || {
            Error::coded(
                RpcErrorCode::ConfirmationRequired,
                "Resetting the Server needs confirmation. No changes made.",
            )
            .hint(yes)
        },
        "Cancelled. The Server was not reset.",
    )
}

pub(crate) fn parse_endpoints(values: &[String]) -> Result<Vec<AdvertisedEndpoint>, Error> {
    values
        .iter()
        .map(|value| {
            value
                .parse::<SocketAddr>()
                .or_else(|_| {
                    value
                        .parse::<IpAddr>()
                        .map(|address| SocketAddr::new(address, ployz_core::WIREGUARD_PORT))
                })
                .map(AdvertisedEndpoint)
                .map_err(|_| Error::usage(format!("invalid WireGuard endpoint {value:?}")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ployz_core::{DOCKER_NETWORK_CONFLICT_RECOVERY, MachineToken};

    use super::*;

    #[test]
    fn endpoints_accept_socket_or_ip_and_apply_the_default_port() {
        assert_eq!(
            parse_endpoints(&["192.0.2.1".into(), "[2001:db8::1]:6000".into()]).unwrap(),
            [
                AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap()),
                AdvertisedEndpoint("[2001:db8::1]:6000".parse().unwrap()),
            ]
        );
        assert!(parse_endpoints(&["not-an-address".into()]).is_err());
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

    #[test]
    fn a_refusal_after_joining_is_not_blamed_on_joining() {
        let mut joining = Joining::default();
        joining.seen(&LocalMachinePhase::Joining);
        let refused = ConnectError::Io(std::io::Error::other(format!(
            "{}refusing to change ployz-wg",
            ployz_core::STARTUP_REFUSED
        )));
        let error = joining.failed(crate::setup_retry::Error::Permanent(refused), "not ready");
        let text = error_chain::row(&error);
        assert!(!text.contains("stayed joining"), "{text}");
        assert!(text.contains("ployzd refused to start"), "{text}");
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

    /// A rerun on a Server stuck joining asks for the addresses its join was approved with,
    /// not the ones it rediscovers now that its WireGuard is up, which the admitting Server
    /// would refuse as a join it never approved.
    #[test]
    fn a_server_still_joining_asks_again_with_its_approved_addresses() {
        let mut machine = crate::global_fixtures::machine('a', "web-3");
        machine.public_ip = Some("203.0.113.7".parse().unwrap());
        machine.advertised_endpoints = vec![ployz_core::AdvertisedEndpoint(
            "203.0.113.7:51820".parse().unwrap(),
        )];
        let rediscovered = MachineToken {
            id: machine.id,
            public_key: machine.public_key,
            public_ip: Some("198.51.100.9".parse().unwrap()),
            advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                "10.210.1.1:51820".parse().unwrap(),
            )],
            ..token_with_hostname("web-3")
        };
        let details = |phase| MachineDetails {
            devices: Vec::new(),
            removed_keys: Vec::new(),
            id: machine.id,
            phase,
            public_key: machine.public_key,
            advertised_endpoints: Vec::new(),
            machine: Some(machine.clone()),
            store_version: Default::default(),
            rtts: Vec::new(),
            passkeys: Vec::new(),
            management_clients: Vec::new(),
            telemetry: None,
            storage: None,
            upgrade: None,
            token: None,
            sealing: None,
            founding: Default::default(),
            grants: Vec::new(),
            removed_grants: Vec::new(),
        };

        let asked = joining_as_approved(&details(LocalMachinePhase::Joining), rediscovered.clone());
        assert_eq!(
            (asked.public_ip, asked.advertised_endpoints),
            (machine.public_ip, machine.advertised_endpoints.clone())
        );
        let fresh = joining_as_approved(
            &details(LocalMachinePhase::Uninitialized),
            rediscovered.clone(),
        );
        assert_eq!(
            fresh.advertised_endpoints,
            rediscovered.advertised_endpoints
        );
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
            first_key: None,
        }
    }
}
