//! `ployz cloud enroll`: enroll `initialize` or `join` on this Machine.

use std::{future::Future, time::Duration};

use clap::ArgMatches;
use ipnet::Ipv4Net;
use ployz_core::{
    CloudEnrollToken, CloudPairing, DescribeContractRequest, InitializeRequest, InspectRequest,
    JoinRequest, LocalMachinePhase, Machine, MachineDetails, MachineName, MachineToken,
    MachineTokenRequest, SetCloudPairingRequest, StorageChoice, op,
};

use super::{Error, config_path, leaf_matches, required, runtime};
use crate::cloud_enroll::{self, EnrollIdentity, InitializeMode, Join, Outcome};
use crate::connect::{Client, ConnectError};
use crate::context::{ContextError, Transport};

pub(super) fn enroll(root: &ArgMatches) -> Result<(), Error> {
    enroll_with_installer(root, &|storage| async move {
        if storage == StorageChoice::Zfs {
            crate::provisioning::provision_local(env!("CARGO_PKG_VERSION"), storage).await?;
        } else {
            crate::provisioning::synchronize_local_daemon().await?;
        }
        Ok(())
    })
}

/// Run Cloud enrollment, installing this CLI's daemon version with `install`.
///
/// `install` receives `none` for software-only synchronization and `zfs` for
/// storage preparation; tests substitute it to avoid provisioning a real daemon.
///
/// # Errors
///
/// Returns the same CLI failure as [`enroll`].
#[doc(hidden)]
pub fn enroll_with_installer<Install, InstallFuture>(
    root: &ArgMatches,
    install: &Install,
) -> Result<(), Error>
where
    Install: Fn(StorageChoice) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let matches = leaf_matches(root);
    let token = CloudEnrollToken::parse(required(matches, "token")?)?;
    let cloud_url = matches
        .get_one::<String>("cloud-url")
        .expect("cloud-url has a default");
    let url = cloud_enroll::enroll_url(cloud_url, &token);
    let requested_name = matches
        .get_one::<String>("name")
        .map(MachineName::parse)
        .transpose()?;
    let requested_storage = *matches
        .get_one::<StorageChoice>("storage")
        .expect("storage has a default");
    let cluster_network = *matches
        .get_one::<Ipv4Net>("network")
        .expect("Cluster network has a default");

    runtime()?.block_on(async {
        let mut client = connect_machine(matches).await?;
        client = synchronize_daemon(matches, client, install).await?;
        if matches.get_flag("reset") {
            client = ensure_uninitialized(matches, matches.get_flag("yes"), true, client).await?;
        }
        let (details, machine_token, name, outcome) =
            enroll_current_identity(&mut client, requested_name, requested_storage, &url).await?;
        match outcome {
            Outcome::Join(join) => enroll_join(matches, client, details, *join, install).await,
            Outcome::Initialize {
                mode,
                pairing,
                storage,
            } => {
                enroll_founder(
                    matches,
                    client,
                    details,
                    machine_token,
                    name,
                    cluster_network,
                    mode,
                    pairing,
                    storage,
                    cloud_url,
                    &token,
                    install,
                )
                .await
            }
        }
    })
}

fn already_assigned(details: &MachineDetails, assigned: &Machine) -> bool {
    details.phase == LocalMachinePhase::Participating
        && details
            .machine
            .as_ref()
            .is_some_and(|machine| machine.id == assigned.id)
}

async fn enroll_current_identity(
    client: &mut Client,
    requested_name: Option<MachineName>,
    requested_storage: StorageChoice,
    url: &str,
) -> Result<(MachineDetails, MachineToken, MachineName, Outcome), Error> {
    let details = client
        .call_repeatable::<op::Inspect>(InspectRequest::default(), None)
        .await?;
    let machine_token = client
        .call_repeatable::<op::MachineToken>(MachineTokenRequest::default(), None)
        .await?;
    let name = crate::handlers::machine::machine_name(requested_name, &machine_token)?;
    let identity =
        EnrollIdentity::from_machine_token(name.clone(), &machine_token, requested_storage);
    let outcome = cloud_enroll::enroll(url, &identity).await?;
    Ok((details, machine_token, name, outcome))
}

async fn enroll_join<Install, InstallFuture>(
    matches: &ArgMatches,
    mut client: Client,
    details: MachineDetails,
    join: Join,
    install: &Install,
) -> Result<(), Error>
where
    Install: Fn(StorageChoice) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let assigned = join.registration.assigned_machine.clone();
    if already_assigned(&details, &assigned) {
        println!("Initialised Machine {} ({})", assigned.name, assigned.id);
        return Ok(());
    }

    client = ensure_uninitialized(
        matches,
        matches.get_flag("yes"),
        matches.get_flag("reset"),
        client,
    )
    .await?;
    client = provision_storage(matches, client, join.storage, install).await?;
    crate::handlers::machine::join(
        &mut client,
        JoinRequest {
            registration: join.registration,
            wireguard_mtu: matches.get_one::<u32>("wg-mtu").copied(),
            cloud_pairing: Some(join.pairing),
        },
    )
    .await?;
    let mut ready = wait_phase(
        matches,
        LocalMachinePhase::Participating,
        "joined Machine did not become ready",
    )
    .await?;
    if let Err(error) = crate::global_catch_up::catch_up_globals(
        &mut ready,
        &assigned,
        matches.get_flag("no-ingress"),
    )
    .await
    {
        return Err(Error::usage(crate::global_catch_up::joined_catch_up_error(
            error,
        )));
    }
    println!("Joined Machine {} ({})", assigned.name, assigned.id);
    Ok(())
}

enum FounderLocalState {
    Initialize,
    Resume { machine: Box<Machine> },
}

#[expect(
    clippy::too_many_arguments,
    reason = "the founder tail consumes the existing cloud-enroll command interface"
)]
async fn enroll_founder<Install, InstallFuture>(
    matches: &ArgMatches,
    mut client: Client,
    details: MachineDetails,
    machine_token: MachineToken,
    name: MachineName,
    cluster_network: Ipv4Net,
    mode: InitializeMode,
    pairing: CloudPairing,
    storage: StorageChoice,
    cloud_url: &str,
    token: &CloudEnrollToken,
    install: &Install,
) -> Result<(), Error>
where
    Install: Fn(StorageChoice) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let state = match (mode, details.phase) {
        (InitializeMode::Resume, LocalMachinePhase::Participating) => FounderLocalState::Resume {
            machine: Box::new(details.machine.ok_or_else(|| {
                Error::usage("matching founding Machine has no participating identity".to_owned())
            })?),
        },
        (InitializeMode::Resume, LocalMachinePhase::Uninitialized)
        | (InitializeMode::New, LocalMachinePhase::Uninitialized) => FounderLocalState::Initialize,
        (InitializeMode::New, phase) => {
            return Err(Error::usage(format!(
                "new founding claim requires an uninitialized Machine, but the local phase is {}",
                phase.as_str().escape_debug()
            )));
        }
        (InitializeMode::Resume, phase) => {
            return Err(Error::usage(format!(
                "matching founding Machine cannot resume from local phase {}",
                phase.as_str().escape_debug()
            )));
        }
    };
    let no_ingress = matches.get_flag("no-ingress");
    let no_dns = matches.get_flag("no-dns");
    let ingress_image = matches.get_one::<String>("ingress-image").cloned();
    let ingress = if no_ingress {
        None
    } else {
        Some(crate::ingress::service_spec(ingress_image, Vec::new(), None).await?)
    };
    let (machine, mut ready) = match state {
        FounderLocalState::Resume { machine } => (*machine, client),
        FounderLocalState::Initialize => {
            client = ensure_uninitialized(
                matches,
                matches.get_flag("yes"),
                matches.get_flag("reset"),
                client,
            )
            .await?;
            client = provision_storage(matches, client, storage, install).await?;
            let initialized = crate::handlers::machine::initialize(
                &mut client,
                InitializeRequest {
                    name,
                    cluster_network,
                    public_ip: machine_token.public_ip,
                    advertised_endpoints: machine_token.advertised_endpoints,
                    wireguard_mtu: matches.get_one::<u32>("wg-mtu").copied(),
                    cloud_pairing: None,
                },
            )
            .await?;
            let ready = wait_phase(
                matches,
                LocalMachinePhase::Participating,
                "initial Machine did not become ready",
            )
            .await?;
            (initialized.machine, ready)
        }
    };

    if !no_dns {
        let domain =
            crate::dns::reserve_if_missing(&mut ready, crate::dns::HOSTED_DNS_ENDPOINT.to_owned())
                .await.map_err(|error| Error::usage(format!("Machine initialized; DNS reservation pending: {error}; rerun the same ployz cloud enroll command without --reset (keep all other options)")))?;
        println!("Reserved Cluster domain: {domain}");
    }
    if let Some(requested) = ingress {
        // An interrupted Apply may have completed mutations. Do not replay it.
        crate::deploy::apply_requested(&mut ready, &requested).await.map_err(|error| {
            let error: Error = error.into();
            Error::usage(format!("Machine initialized; Ingress deployment incomplete: {error}; rerun the same ployz cloud enroll command without --reset (keep all other options) to reconcile the observed state"))
        })?;
        if !no_dns {
            crate::dns::update_records_for_ingress(&mut ready).await.map_err(|error| {
                Error::usage(format!("Machine initialized; DNS publication pending: {error}; rerun the same ployz cloud enroll command without --reset (keep all other options)"))
            })?;
        }
    }
    // Setting the same pairing is idempotent.
    ready.call_repeatable::<op::SetCloudPairing>(SetCloudPairingRequest { cloud_pairing: Some(pairing.clone()) }, None)
        .await.map_err(|error| Error::usage(format!("Machine initialized; Cloud Pairing publication incomplete: {error}; rerun the same ployz cloud enroll command without --reset (keep all other options)")))?;
    cloud_enroll::callback(
        &cloud_enroll::callback_url(cloud_url, token),
        machine.id,
        pairing.secret(),
    )
    .await?;
    println!("Initialised Machine {} ({})", machine.name, machine.id);
    Ok(())
}

async fn provision_storage<Install, InstallFuture>(
    matches: &ArgMatches,
    client: Client,
    storage: StorageChoice,
    install: &Install,
) -> Result<Client, Error>
where
    Install: Fn(StorageChoice) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    crate::provisioning::announce_storage(storage);
    if storage != StorageChoice::Zfs {
        return Ok(client);
    }
    if !matches!(client.connection().transport(), Transport::Unix(_)) {
        return Err(Error::usage(format!(
            "zfs storage preparation requires running ployz cloud enroll on the Machine itself; connected through {}",
            client.connection()
        )));
    }
    install(storage).await?;
    wait_matching_daemon(matches).await
}

async fn synchronize_daemon<Install, InstallFuture>(
    matches: &ArgMatches,
    mut client: Client,
    install: &Install,
) -> Result<Client, Error>
where
    Install: Fn(StorageChoice) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let daemon = client
        .call_repeatable::<op::DescribeContract>(DescribeContractRequest {}, None)
        .await?;
    if daemon.daemon_version == env!("CARGO_PKG_VERSION") {
        return Ok(client);
    }
    if !matches!(client.connection().transport(), Transport::Unix(_)) {
        return Err(Error::usage(format!(
            "daemon version synchronization requires running ployz cloud enroll on the Machine itself; connected through {}",
            client.connection()
        )));
    }
    install(StorageChoice::None).await?;
    wait_matching_daemon(matches).await
}

async fn wait_matching_daemon(matches: &ArgMatches) -> Result<Client, Error> {
    let mut client = wait_client(matches).await?;
    let daemon = client
        .call_repeatable::<op::DescribeContract>(DescribeContractRequest {}, None)
        .await?;
    if daemon.daemon_version != env!("CARGO_PKG_VERSION") {
        return Err(Error::usage(format!(
            "daemon version remained {} after installing CLI version {}",
            daemon.daemon_version,
            env!("CARGO_PKG_VERSION")
        )));
    }
    Ok(client)
}

async fn connect_machine(matches: &ArgMatches) -> Result<Client, Error> {
    let config = config_path(matches)?;
    let connect = matches.get_one::<String>("connect").map(String::as_str);
    match crate::connect::connect_with_ssh_timeout(
        &config,
        connect,
        None,
        crate::cli::ssh_timeout(matches),
    )
    .await
    {
        Ok(client) => Ok(client),
        Err(ConnectError::Context(ContextError::NoConfig)) => {
            crate::provisioning::provision_local(
                env!("CARGO_PKG_VERSION"),
                ployz_core::StorageChoice::None,
            )
            .await?;
            wait_client(matches).await
        }
        Err(error) => Err(error.into()),
    }
}

fn retry_local_connect(error: &ConnectError) -> bool {
    matches!(error, ConnectError::Context(ContextError::NoConfig)) || error.is_setup_retryable()
}

async fn wait_client(matches: &ArgMatches) -> Result<Client, Error> {
    let config = config_path(matches)?;
    let connect = matches.get_one::<String>("connect").map(String::as_str);
    crate::setup_retry::run(
        &mut (),
        "Waiting for local daemon",
        crate::setup_retry::WAIT,
        retry_local_connect,
        async |_| {
            crate::connect::connect_with_ssh_timeout(
                &config,
                connect,
                None,
                crate::cli::ssh_timeout(matches),
            )
            .await
        },
    )
    .await
    .map_err(Into::into)
}

async fn ensure_uninitialized(
    matches: &ArgMatches,
    yes: bool,
    reset: bool,
    mut client: Client,
) -> Result<Client, Error> {
    let details = client
        .call_repeatable::<op::Inspect>(InspectRequest::default(), None)
        .await?;
    if details.phase == LocalMachinePhase::Uninitialized {
        return Ok(client);
    }
    if !reset {
        return Err(Error::usage(
            "Machine is already initialised; rerun with --reset to reset it before enrollment"
                .to_owned(),
        ));
    }
    crate::handlers::machine::confirm(yes, "Reset the Machine before joining this Cluster?")?;
    crate::handlers::machine::reset(&mut client).await?;
    wait_phase(
        matches,
        LocalMachinePhase::Uninitialized,
        "Machine did not reset",
    )
    .await
}

async fn wait_phase(
    matches: &ArgMatches,
    phase: LocalMachinePhase,
    timeout_message: &str,
) -> Result<Client, Error> {
    let participating = phase == LocalMachinePhase::Participating;
    let wait = if participating {
        ployz_core::MACHINE_START_WAIT
    } else {
        Duration::from_secs(60)
    };
    let config = config_path(matches)?;
    let connect = matches.get_one::<String>("connect").map(String::as_str);
    crate::setup_retry::run(
        &mut (),
        timeout_message,
        wait,
        ConnectError::is_setup_retryable,
        async |_| {
            let mut client = crate::connect::connect_with_ssh_timeout(
                &config,
                connect,
                None,
                crate::cli::ssh_timeout(matches),
            )
            .await?;
            let details = client
                .call_repeatable::<op::Inspect>(InspectRequest::default(), None)
                .await?;
            if details.phase != phase {
                return Err(ConnectError::Attempt(
                    format!("Machine phase is {}", details.phase.as_str().escape_debug()).into(),
                ));
            }
            Ok(client)
        },
    )
    .await
    .map_err(|error| {
        Error::usage(if participating {
            format!(
                "{}: {error}",
                crate::handlers::machine::readiness_timeout_message(timeout_message)
            )
        } else {
            error.to_string()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io, path::PathBuf};

    use crate::context::ConnectionSource;

    #[test]
    fn wait_retries_no_config_and_unreachable_connect_errors() {
        assert!(retry_local_connect(&ConnectError::Context(
            ContextError::NoConfig
        )));
        assert!(retry_local_connect(&ConnectError::Io(io::Error::from(
            io::ErrorKind::ConnectionRefused
        ))));
        assert!(!retry_local_connect(&ConnectError::AllFailed {
            source: ConnectionSource::LocalSocket,
            attempts: 1,
            setup_retryable: false,
            last: None,
        }));
        assert!(!retry_local_connect(&ConnectError::Context(
            ContextError::NoCurrentContext(PathBuf::from("config.yaml"))
        )));
        assert!(!retry_local_connect(&ConnectError::InvalidDialCredential));
    }

    #[test]
    fn post_join_ingress_error_names_membership_and_recovery() {
        let message = crate::global_catch_up::joined_catch_up_error(
            crate::global_catch_up::CatchUpError::new(
                crate::failure::Failure::usage("not running".to_owned()),
                vec![ployz_core::QualifiedService::system_ingress()],
            ),
        );
        assert!(message.contains("Machine joined"));
        assert!(message.contains("ployz ingress deploy"));
    }

    #[test]
    fn post_join_other_error_names_membership() {
        let message = crate::global_catch_up::joined_catch_up_error(
            crate::global_catch_up::CatchUpError::new(
                crate::failure::Failure::usage("listing failed".to_owned()),
                Vec::new(),
            ),
        );
        assert!(message.contains("Machine joined"));
        assert!(message.contains("listing failed"));
    }
}
