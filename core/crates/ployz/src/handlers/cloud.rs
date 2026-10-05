//! Cloud enrollment behind `ployz server add`, and `ployz cloud reset`.

use std::{future::Future, time::Duration};

use clap::{ArgMatches, Command};

use crate::cli::{base, switch};
use ipnet::Ipv4Net;
use ployz_core::{
    CloudEnrollToken, DescribeContractRequest, InitializeRequest, InspectRequest, JoinRequest,
    LocalMachinePhase, Machine, MachineDetails, MachineName, MachineToken, MachineTokenRequest,
    ManagementCapability, ManagementClientLabel, SetManagementClientRequest, StorageChoice, op,
};

use super::{Error, config_path, leaf_matches, required, runtime};
use crate::cloud_enroll::{self, CloudPairing, EnrollIdentity, InitializeMode, Join, Outcome};
use crate::connect::{Client, ConnectError};
use crate::context::{Connection, ContextError, SelectedConnections, Transport};
use crate::setup_report::{SetupReport, Step};
use crate::ui::Hint;

/// Enroll with `token`: over SSH to `DESTINATION`, or on the host this runs on.
pub(super) fn enroll(
    root: &ArgMatches,
    token: CloudEnrollToken,
    cloud_url: &str,
) -> Result<(), Error> {
    if leaf_matches(root)
        .get_one::<String>("destination")
        .is_some()
    {
        // A remote host is provisioned up front, in `enroll_steps`.
        return enroll_token(root, token, cloud_url, &|_| async { Ok(()) });
    }
    enroll_token(root, token, cloud_url, &|storage| async move {
        if storage == StorageChoice::Zfs {
            crate::provisioning::provision_local(env!("CARGO_PKG_VERSION"), storage).await?;
        } else {
            crate::provisioning::synchronize_local_daemon().await?;
        }
        Ok(())
    })
}

/// Run `server add --token` enrollment, installing this CLI's daemon version with `install`.
///
/// `install` receives `none` for software-only synchronization and `zfs` for
/// storage preparation; tests substitute it to avoid provisioning a real daemon.
///
/// # Errors
///
/// Returns the same CLI failure as `ployz server add --token`.
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
        .map_or("ployz.dev", String::as_str);
    enroll_token(root, token, cloud_url, install)
}

/// Setup's outcome: `Ok` once the final callback succeeded, holding what printing the
/// committed result returned.
type Setup = Result<Result<(), Error>, Error>;

/// Enroll, then send Cloud the best-effort setup report for the outcome.
fn enroll_token<Install, InstallFuture>(
    root: &ArgMatches,
    token: CloudEnrollToken,
    cloud_url: &str,
    install: &Install,
) -> Result<(), Error>
where
    Install: Fn(StorageChoice) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let mut report = SetupReport::start();
    runtime()?.block_on(async {
        let setup = enroll_steps(root, &token, cloud_url, install, &mut report).await;
        report
            .send(
                &cloud_enroll::report_url(cloud_url, &token),
                setup.as_ref().err(),
            )
            .await;
        setup.and_then(|printed| printed)
    })
}

async fn enroll_steps<Install, InstallFuture>(
    root: &ArgMatches,
    token: &CloudEnrollToken,
    cloud_url: &str,
    install: &Install,
    report: &mut SetupReport,
) -> Setup
where
    Install: Fn(StorageChoice) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let matches = leaf_matches(root);
    let remote = matches.get_one::<String>("destination").is_some();
    // First, so a failed first install still reports what it ran on.
    if !remote && dials_this_host(matches) {
        report.read_host();
    }
    let initial_policy = super::server::enrollment_policy(matches)?;
    let url = cloud_enroll::enroll_url(cloud_url, token);
    let requested_name = matches
        .get_one::<String>("name")
        .map(MachineName::parse)
        .transpose()?;
    let requested_storage = super::server::requested_storage(matches);
    let cluster_network = *matches
        .get_one::<Ipv4Net>("network")
        .expect("Cluster network has a default");

    if remote && !matches.get_flag("no-install") {
        // A remote host is provisioned once, up front, with this CLI's release.
        crate::provisioning::provision(matches, requested_storage).await?;
    }
    let mut client = connect_machine(matches).await?;
    client = synchronize_daemon(matches, client, install).await?;
    if matches.get_flag("reset") {
        client = ensure_uninitialized(matches, matches.get_flag("yes"), true, client).await?;
    }
    report.step(Step::Enroll);
    let (details, machine_token, name, outcome) = enroll_current_identity(
        &mut client,
        requested_name,
        requested_storage,
        &initial_policy,
        &url,
    )
    .await?;
    report.sizes_from(&machine_token);
    report.step(Step::Storage);
    match outcome {
        Outcome::Join(join) => {
            report.enrolled(join.storage, false);
            enroll_join(
                matches,
                client,
                details,
                *join,
                &initial_policy,
                &cloud_enroll::callback_url(cloud_url, token),
                install,
                report,
            )
            .await
        }
        Outcome::Initialize {
            mode,
            pairing,
            storage,
        } => {
            report.enrolled(storage, true);
            enroll_founder(
                matches,
                client,
                details,
                machine_token,
                name,
                initial_policy,
                cluster_network,
                mode,
                pairing,
                storage,
                cloud_url,
                token,
                install,
                report,
            )
            .await
        }
    }
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
    initial_policy: &ployz_core::InitialMachinePolicy,
    url: &str,
) -> Result<(MachineDetails, MachineToken, MachineName, Outcome), Error> {
    let details = client
        .call_repeatable::<op::Inspect>(InspectRequest::default(), None)
        .await?;
    let machine_token = client
        .call_repeatable::<op::MachineToken>(MachineTokenRequest::default(), None)
        .await?;
    let name = crate::handlers::server::machine_name(requested_name, &machine_token)?;
    let identity = EnrollIdentity::from_machine_token(
        name.clone(),
        &machine_token,
        requested_storage,
        initial_policy.clone(),
    );
    let outcome = cloud_enroll::enroll(url, &identity).await?;
    Ok((details, machine_token, name, outcome))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the join tail consumes the existing cloud-enroll command interface"
)]
async fn enroll_join<Install, InstallFuture>(
    matches: &ArgMatches,
    mut client: Client,
    details: MachineDetails,
    join: Join,
    initial_policy: &ployz_core::InitialMachinePolicy,
    callback_url: &str,
    install: &Install,
    report: &mut SetupReport,
) -> Setup
where
    Install: Fn(StorageChoice) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let assigned = join.registration.assigned_machine.clone();
    if !initial_policy.matches(&assigned)
        || (already_assigned(&details, &assigned)
            && !initial_policy.matches(
                details
                    .machine
                    .as_ref()
                    .expect("already assigned Machine exists"),
            ))
    {
        return Err(Error::conflict(
            "initial policy differs from the currently observed Server; enrollment does not edit an existing Server",
        ));
    }
    let pairing = join.pairing;
    let mut ready = if already_assigned(&details, &assigned) {
        report.step(Step::Join);
        client
    } else {
        client = ensure_uninitialized(
            matches,
            matches.get_flag("yes"),
            matches.get_flag("reset"),
            client,
        )
        .await?;
        client = provision_storage(matches, client, join.storage, install).await?;
        report.step(Step::Join);
        crate::handlers::server::join(
            &mut client,
            JoinRequest {
                registration: join.registration,
                wireguard_mtu: matches.get_one::<u32>("wg-mtu").copied(),
            },
        )
        .await?;
        wait_phase(
            matches,
            LocalMachinePhase::Participating,
            "the joined Server did not become ready",
        )
        .await?
    };
    // Mint a fresh capability; Cloud verifies replacements when enrollment resumes.
    let capability = set_cloud_management_client(matches, &mut ready).await?;
    let catch_up = crate::global_catch_up::catch_up_globals(&mut ready, &assigned).await;
    // Cloud may use the replacement after publication, revoking this key.
    // A committed join remains enrolled even when Global catch-up needs a separate retry.
    cloud_enroll::publish(callback_url, assigned.id, &pairing.secret, &capability).await?;
    cloud_enroll::callback(callback_url, assigned.id, &pairing.secret).await?;
    crate::output::say!("Joined Server {} ({})", assigned.name, assigned.id);
    // The join is committed; a catch-up failure makes it partial.
    Ok(crate::output::emit_committed(
        serde_json::json!({ "server": super::server::server_json(&assigned), "founded": false }),
        catch_up.map_err(|error| crate::global_catch_up::joined_catch_up_error(error, &assigned)),
    ))
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
    initial_policy: ployz_core::InitialMachinePolicy,
    cluster_network: Ipv4Net,
    mode: InitializeMode,
    pairing: CloudPairing,
    storage: StorageChoice,
    cloud_url: &str,
    token: &CloudEnrollToken,
    install: &Install,
    report: &mut SetupReport,
) -> Setup
where
    Install: Fn(StorageChoice) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let state = match (mode, details.phase) {
        (InitializeMode::Resume, LocalMachinePhase::Participating) => FounderLocalState::Resume {
            machine: Box::new(details.machine.ok_or_else(|| {
                Error::conflict(
                    "the matching founding Server has no participating identity".to_owned(),
                )
            })?),
        },
        (InitializeMode::Resume, LocalMachinePhase::Uninitialized)
        | (InitializeMode::New, LocalMachinePhase::Uninitialized) => FounderLocalState::Initialize,
        (InitializeMode::New, phase) => {
            return Err(Error::conflict(format!(
                "a new founding claim requires an uninitialized Server, but its local phase is {}",
                phase.as_str().escape_debug()
            )));
        }
        (InitializeMode::Resume, phase) => {
            return Err(Error::conflict(format!(
                "the matching founding Server cannot resume from local phase {}",
                phase.as_str().escape_debug()
            )));
        }
    };
    if let FounderLocalState::Resume { machine } = &state
        && !initial_policy.matches(machine)
    {
        return Err(Error::conflict(
            "initial policy differs from the currently observed Server; enrollment does not edit an existing Server",
        ));
    }
    let accepts_ingress = match &state {
        FounderLocalState::Resume { machine } => machine.accepts_ingress,
        FounderLocalState::Initialize => initial_policy.accepts_ingress,
    };
    let ingress_image = matches.get_one::<String>("ingress-image").cloned();
    let ingress = if !accepts_ingress {
        None
    } else {
        Some(crate::ingress::service_spec(ingress_image, Default::default()).await?)
    };
    // A rerun on the founded Server finishes or repeats the enrollment; it founds nothing.
    let founding = matches!(state, FounderLocalState::Initialize);
    let (machine, mut ready) = match state {
        FounderLocalState::Resume { machine } => {
            report.step(Step::Join);
            (*machine, client)
        }
        FounderLocalState::Initialize => {
            client = ensure_uninitialized(
                matches,
                matches.get_flag("yes"),
                matches.get_flag("reset"),
                client,
            )
            .await?;
            client = provision_storage(matches, client, storage, install).await?;
            report.step(Step::Join);
            let initialized = crate::handlers::server::initialize(
                &mut client,
                InitializeRequest {
                    initial_policy,
                    name,
                    cluster_network,
                    public_ip: machine_token.public_ip,
                    advertised_endpoints: machine_token.advertised_endpoints,
                    wireguard_mtu: matches.get_one::<u32>("wg-mtu").copied(),
                },
            )
            .await?;
            let ready = wait_phase(
                matches,
                LocalMachinePhase::Participating,
                "the first Server did not become ready",
            )
            .await?;
            (initialized.machine, ready)
        }
    };

    if machine.accepts_ingress
        && let Some(requested) = ingress
    {
        // An interrupted Apply may have completed mutations. Do not replay it.
        let _ingress = crate::deploy::apply_requested(&mut ready, &requested, false, false, "default").await.map_err(|error| {
            Error::from(error).context("Server initialized; Ingress deployment incomplete. Rerun the same ployz server add command without --reset, keeping all other options, to reconcile it.")
        })?;
    }
    // Repeated Set stages a fresh capability; its first operational RPC completes rotation.
    let capability = set_cloud_management_client(matches, &mut ready).await
        .map_err(|error| error.context("Server initialized; Cloud Pairing publication incomplete. Rerun the same ployz server add command without --reset, keeping all other options."))?;
    cloud_enroll::publish(
        &cloud_enroll::callback_url(cloud_url, token),
        machine.id,
        &pairing.secret,
        &capability,
    )
    .await?;
    cloud_enroll::callback(
        &cloud_enroll::callback_url(cloud_url, token),
        machine.id,
        &pairing.secret,
    )
    .await?;
    Ok(crate::output::emit(&founder_result(&machine, founding)))
}

/// The first Server's result. Founding it, Cloud deploys the Organization's saved
/// Environments to it, as Deployments admitted after this returns.
fn founder_result(machine: &Machine, founding: bool) -> serde_json::Value {
    let next = founding.then_some("ployz deployment ls");
    if founding {
        crate::output::say!("Initialised Server {} ({})", machine.name, machine.id);
        crate::output::say!("Cloud now deploys any saved Environments to it.");
    } else {
        crate::output::say!("Server {} ({}) is enrolled", machine.name, machine.id);
    }
    if let Some(next) = next {
        crate::output::say!("next: {next}");
    }
    serde_json::json!({
        "server": super::server::server_json(machine),
        "founded": founding,
        "deploys_saved_environments": founding,
        "next": next,
    })
}

/// Take a fresh Management Capability for Cloud's `cloud` Management Client slot.
///
/// The Pairing Credential stays with the CLI; the daemon stores only public keys.
async fn set_cloud_management_client(
    matches: &ArgMatches,
    client: &mut Client,
) -> Result<ManagementCapability, Error> {
    let response = client
        .call_repeatable::<op::SetManagementClient>(
            SetManagementClientRequest::Set {
                label: ManagementClientLabel::parse("cloud")
                    .expect("`cloud` is a valid Management Client label"),
            },
            None,
        )
        .await?;
    let capability = response.capability.ok_or_else(|| {
        Error::coded(
            ployz_core::RpcErrorCode::Internal,
            "Machine set the `cloud` Management Client without a Management Capability",
        )
    })?;
    if matches!(client.connection().transport(), Transport::Management(_)) {
        crate::context::Config::load(config_path(matches)?)?
            .save_management_capability(&capability)?;
    }
    Ok(capability)
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
    if !installs_here(matches, &client) {
        return Err(Error::usage(format!(
            "zfs storage preparation requires running ployz server add on the Server itself; connected through {}",
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
    if !installs_here(matches, &client) {
        return Err(Error::usage(format!(
            "daemon version synchronization requires running ployz server add on the Server itself; connected through {}",
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
        return Err(Error::unavailable(format!(
            "daemon version remained {} after installing CLI version {}",
            daemon.daemon_version,
            env!("CARGO_PKG_VERSION")
        )));
    }
    Ok(client)
}

/// The installer reaches the enrolling host: it runs there, or provisioned `DESTINATION`.
fn installs_here(matches: &ArgMatches, client: &Client) -> bool {
    matches.get_one::<String>("destination").is_some()
        || matches!(client.connection().transport(), Transport::Unix(_))
}

/// Connect to the enrolling host: `DESTINATION`, else `--connect` or the local daemon.
async fn dial(matches: &ArgMatches) -> Result<Client, ConnectError> {
    if let Some(destination) = matches.get_one::<String>("destination") {
        let mut connection: Connection = destination.parse()?;
        if matches!(connection.transport(), Transport::Ssh { .. })
            && let Some(key) = matches.get_one::<String>("ssh-key")
        {
            connection = connection.with_ssh_key_file(key)?;
        }
        return super::server::connect_direct(matches, &connection).await;
    }
    crate::connect::connect_selected_with(
        local_selection(matches)?,
        std::sync::Arc::new(
            crate::connect::SystemConnector::default()
                .with_ssh_timeout(crate::cli::ssh_timeout(matches)),
        ),
    )
    .await
}

/// What a run with no destination dials: `--connect`, the current context or the local socket.
fn local_selection(matches: &ArgMatches) -> Result<SelectedConnections, ConnectError> {
    let config = crate::context::expand_home(std::path::Path::new(
        matches
            .get_one::<String>("ployz-config")
            .expect("ployz-config has a default"),
    ));
    crate::connect::resolve_connections(
        &config,
        matches.get_one::<String>("connect").map(String::as_str),
        None,
        std::path::Path::new(crate::connect::DEFAULT_LOCAL_SOCKET),
    )
}

/// Whether a run with no destination enrolls this host: it dials only the local socket, or
/// finds nothing to dial and so installs the daemon here (see `connect_machine`).
fn dials_this_host(matches: &ArgMatches) -> bool {
    match local_selection(matches) {
        Ok(selected) => selected
            .connections
            .iter()
            .all(|connection| matches!(connection.transport(), Transport::Unix(_))),
        Err(error) => matches!(error, ConnectError::Context(ContextError::NoConfig)),
    }
}

async fn connect_machine(matches: &ArgMatches) -> Result<Client, Error> {
    match dial(matches).await {
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
    crate::setup_retry::run(
        &mut (),
        "Waiting for local daemon",
        crate::setup_retry::WAIT,
        retry_local_connect,
        async |_| dial(matches).await,
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
        return Err(Error::conflict(
            "This Server is already initialised; rerun with --reset to reset it before enrollment"
                .to_owned(),
        ));
    }
    crate::handlers::server::confirm(yes, "Reset this Server before joining the Cluster?")?;
    crate::handlers::server::reset(&mut client).await?;
    wait_phase(
        matches,
        LocalMachinePhase::Uninitialized,
        "The Server did not reset.",
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
    crate::setup_retry::run(
        &mut (),
        &format!("Waiting for the {} phase", phase.as_str()),
        wait,
        ConnectError::is_setup_retryable,
        async |_| {
            let mut client = dial(matches).await?;
            if participating {
                crate::handlers::server::check_listed(&mut client).await?;
                return Ok(client);
            }
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
        Error::from(error).context(if participating {
            crate::handlers::server::readiness_timeout_message(timeout_message)
        } else {
            timeout_message.to_owned()
        })
    })
}

pub(crate) fn command() -> Command {
    Command::new("cloud")
        .about("Manage Cloud")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            base("reset", "Give up your Organization's founding that never finished")
                .long_about("Give up your Organization's founding that never finished, so the next ployz server add founds the Cluster again. Stop or erase the founding Server first. Needs `ployz login`.")
                .arg(
                    switch("yes", Some('y'))
                        .help("Confirm the founding Server is stopped or erased"),
                ),
        )
}

pub(super) fn handler(path: &str) -> Option<super::Handler> {
    Some(match path {
        "reset" => reset,
        _ => return None,
    })
}

fn reset(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    if !matches.get_flag("yes") {
        return Err(Error::usage(
            "cloud reset gives up the unfinished founding; stop or erase the founding Server, then confirm with --yes",
        )
        .hint(Hint::Retry("ployz cloud reset --yes".into())));
    }
    let store = crate::cloud_login::CredentialStore::beside(&config_path(matches)?);
    runtime()?.block_on(async {
        // PLOYZ_TOKEN or this device's sign-in, as every Cloud command: one Organization.
        let credential = crate::cloud_account::from_env(&store).await?;
        let organization = crate::cloud_account::acting_in(&credential).await?;
        let _: serde_json::Value = crate::cloud_account::call(
            &credential,
            reqwest::Method::POST,
            "cloud/reset",
            Some(&serde_json::json!({
                "organizationSlug": organization.slug,
                "confirmedFounderStoppedOrErased": true,
            })),
        )
        .await?;
        crate::output::say!("Reset the founding of {}", organization.slug);
        crate::output::emit(&serde_json::json!({ "reset": true, "organization": organization }))
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
    }
}
