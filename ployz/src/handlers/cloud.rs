//! `ployz cloud enroll`: enroll `initialize` or `join` on this Machine.

use std::{ops::AsyncFnMut, time::Duration};

use clap::ArgMatches;
use ipnet::Ipv4Net;
use ployz_core::{
    CloudEnrollToken, CloudPairing, DescribeContractRequest, InitializeRequest, InspectRequest,
    JoinRequest, LocalMachinePhase, Machine, MachineDetails, MachineName, MachineToken,
    MachineTokenRequest, ReserveDomainRequest, ResetRequest, SetCloudPairingRequest, StorageChoice,
    op,
};

use super::{Error, config_path, connect_client, leaf_matches, required, runtime};
use crate::cloud_enroll::{self, EnrollIdentity, InitializeMode, Join, Outcome};
use crate::connect::{Client, ConnectError};
use crate::context::{ContextError, Transport};

/// Installs the local Machine daemon for Cloud enrollment.
pub trait EnrollInstaller {
    /// Install this CLI's daemon version without preparing storage.
    ///
    /// # Errors
    ///
    /// Returns the installer failure reported to the CLI.
    fn install_cli_daemon_without_storage(&self) -> Result<(), Error>;
}

struct EmbeddedInstaller;

impl EnrollInstaller for EmbeddedInstaller {
    fn install_cli_daemon_without_storage(&self) -> Result<(), Error> {
        crate::provisioning::provision_local(StorageChoice::None).map_err(Into::into)
    }
}

pub(super) fn enroll(root: &ArgMatches) -> Result<(), Error> {
    enroll_with_installer(root, &EmbeddedInstaller)
}

/// Run Cloud enrollment with an injected local Machine installer.
///
/// # Errors
///
/// Returns the same CLI failure as [`enroll`].
#[doc(hidden)]
pub fn enroll_with_installer(
    root: &ArgMatches,
    installer: &impl EnrollInstaller,
) -> Result<(), Error> {
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
        client = synchronize_daemon(matches, client, installer).await?;
        if matches.get_flag("reset") {
            client = ensure_uninitialized(matches, matches.get_flag("yes"), true, client).await?;
        }
        let (details, machine_token, name, outcome) =
            enroll_current_identity(&mut client, requested_name, requested_storage, &url).await?;
        match outcome {
            Outcome::Join(join) => enroll_join(matches, client, details, *join).await,
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
        .call::<op::Inspect>(InspectRequest::default(), None)
        .await?;
    let machine_token = client
        .call::<op::MachineToken>(MachineTokenRequest::default(), None)
        .await?;
    let name = crate::handlers::machine::machine_name(requested_name, &machine_token)?;
    let identity =
        EnrollIdentity::from_machine_token(name.clone(), &machine_token, requested_storage);
    let outcome = cloud_enroll::enroll(url, &identity).await?;
    Ok((details, machine_token, name, outcome))
}

async fn enroll_join(
    matches: &ArgMatches,
    mut client: Client,
    details: MachineDetails,
    join: Join,
) -> Result<(), Error> {
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
    provision_storage(&client, join.storage)?;
    client
        .call::<op::Join>(
            JoinRequest {
                registration: join.registration,
                wireguard_mtu: matches.get_one::<u32>("wg-mtu").copied(),
                cloud_pairing: Some(join.pairing),
            },
            None,
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
async fn enroll_founder(
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
) -> Result<(), Error> {
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
                "new founding claim requires an uninitialized Machine, but the local phase is {phase:?}"
            )));
        }
        (InitializeMode::Resume, phase) => {
            return Err(Error::usage(format!(
                "matching founding Machine cannot resume from local phase {phase:?}"
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
            provision_storage(&client, storage)?;
            let initialized = client
                .call::<op::Initialize>(
                    InitializeRequest {
                        name,
                        cluster_network,
                        public_ip: machine_token.public_ip,
                        advertised_endpoints: machine_token.advertised_endpoints,
                        wireguard_mtu: matches.get_one::<u32>("wg-mtu").copied(),
                        cloud_pairing: None,
                    },
                    None,
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
        match ready.domain_if_reserved().await? {
            Some(domain) => println!("Using reserved Cluster domain: {domain}"),
            None => {
                let domain = retry_founder_operation(
                    &mut ready,
                    "domain reservation",
                    ConnectError::is_retryable,
                    async |client| {
                        client
                            .call::<op::ReserveDomain>(
                                ReserveDomainRequest {
                                    endpoint: crate::dns::HOSTED_DNS_ENDPOINT.to_owned(),
                                },
                                None,
                            )
                            .await
                    },
                )
                .await?;
                println!("Reserved Cluster domain: {}", domain.name);
            }
        }
    }
    if let Some(requested) = ingress {
        retry_founder_operation(
            &mut ready,
            "Ingress deployment",
            crate::deploy::ApplyError::is_retryable_transport,
            async |client| crate::deploy::apply_requested(client, &requested).await,
        )
        .await?;
        if !no_dns {
            retry_founder_operation(
                &mut ready,
                "DNS publication",
                crate::dns::Error::is_retryable_transport,
                async |client| crate::dns::update_records_for_ingress(client).await,
            )
            .await?;
        }
    }
    retry_founder_operation(
        &mut ready,
        "Cloud Pairing publication",
        ConnectError::is_retryable,
        async |client| {
            client
                .call::<op::SetCloudPairing>(
                    SetCloudPairingRequest {
                        cloud_pairing: Some(pairing.clone()),
                    },
                    None,
                )
                .await
        },
    )
    .await?;
    cloud_enroll::callback(
        &cloud_enroll::callback_url(cloud_url, token),
        machine.id,
        pairing.secret(),
    )
    .await?;
    println!("Initialised Machine {} ({})", machine.name, machine.id);
    Ok(())
}

async fn retry_founder_operation<C, T, E>(
    context: &mut C,
    operation: &'static str,
    retryable: impl Fn(&E) -> bool,
    mut run: impl AsyncFnMut(&mut C) -> Result<T, E>,
) -> Result<T, Error>
where
    E: Into<Error>,
{
    for attempt in 0..3 {
        match run(context).await {
            Ok(value) => return Ok(value),
            Err(error) if retryable(&error) && attempt < 2 => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(error) if retryable(&error) => {
                let error: Error = error.into();
                return Err(Error::usage(format!(
                    "{operation} failed after brief retries: {error}; rerun the same ployz cloud enroll command"
                )));
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("founder operation attempts are non-zero")
}

fn provision_storage(client: &Client, storage: StorageChoice) -> Result<(), Error> {
    crate::provisioning::announce_storage(storage);
    if storage != StorageChoice::Zfs {
        return Ok(());
    }
    if !matches!(client.connection().transport(), Transport::Unix(_)) {
        return Err(Error::usage(format!(
            "zfs storage preparation requires running ployz cloud enroll on the Machine itself; connected through {}",
            client.connection()
        )));
    }
    crate::provisioning::provision_local(storage)?;
    Ok(())
}

async fn synchronize_daemon(
    matches: &ArgMatches,
    mut client: Client,
    installer: &impl EnrollInstaller,
) -> Result<Client, Error> {
    let daemon = client
        .call::<op::DescribeContract>(DescribeContractRequest {}, None)
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
    installer.install_cli_daemon_without_storage()?;
    let mut client = wait_client(matches).await?;
    let daemon = client
        .call::<op::DescribeContract>(DescribeContractRequest {}, None)
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
    match crate::connect::connect(&config, connect, None).await {
        Ok(client) => Ok(client),
        Err(ConnectError::Context(ContextError::NoConfig)) => {
            crate::provisioning::provision_local(ployz_core::StorageChoice::None)?;
            wait_client(matches).await
        }
        Err(error) => Err(error.into()),
    }
}

fn retry_local_connect(error: &ConnectError) -> bool {
    matches!(error, ConnectError::Context(ContextError::NoConfig)) || error.is_unreachable()
}

async fn wait_client(matches: &ArgMatches) -> Result<Client, Error> {
    let config = config_path(matches)?;
    let connect = matches.get_one::<String>("connect").map(String::as_str);
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match crate::connect::connect(&config, connect, None).await {
                Ok(client) => return Ok(client),
                Err(error) if retry_local_connect(&error) => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Err(error) => return Err(Error::from(error)),
            }
        }
    })
    .await
    .map_err(|_| Error::usage("local daemon did not start".to_owned()))?
}

async fn ensure_uninitialized(
    matches: &ArgMatches,
    yes: bool,
    reset: bool,
    mut client: Client,
) -> Result<Client, Error> {
    let details = client
        .call::<op::Inspect>(InspectRequest::default(), None)
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
    client.call::<op::Reset>(ResetRequest {}, None).await?;
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
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(mut client) = connect_client(matches, None).await
                && client
                    .call::<op::Inspect>(InspectRequest::default(), None)
                    .await
                    .is_ok_and(|details| details.phase == phase)
            {
                return client;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .map_err(|_| Error::usage(timeout_message.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io, path::PathBuf};

    use crate::context::ConnectionSource;

    struct RetryFailure;

    impl From<RetryFailure> for Error {
        fn from(_: RetryFailure) -> Self {
            Self::usage("safe transport detail".to_owned())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_retry_formats_the_converted_cli_failure() {
        let mut attempts = 0;
        let error = retry_founder_operation(
            &mut attempts,
            "operation",
            |_| true,
            async |attempts| {
                *attempts += 1;
                Err::<(), _>(RetryFailure)
            },
        )
        .await
        .unwrap_err();

        assert_eq!(attempts, 3);
        assert_eq!(
            error.to_string(),
            "operation failed after brief retries: safe transport detail; rerun the same ployz cloud enroll command"
        );
    }

    #[test]
    fn wait_retries_no_config_and_unreachable_connect_errors() {
        assert!(retry_local_connect(&ConnectError::Context(
            ContextError::NoConfig
        )));
        assert!(retry_local_connect(&ConnectError::Io(io::Error::from(
            io::ErrorKind::ConnectionRefused
        ))));
        assert!(retry_local_connect(&ConnectError::AllFailed {
            source: ConnectionSource::LocalSocket,
            attempts: 1,
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
