//! Cloud enrollment behind `ployz server add`, and `ployz cloud reset`.

use std::{future::Future, time::Duration};

use clap::{ArgMatches, Command};

use crate::cli::{base, switch};
use ipnet::Ipv4Net;
use ployz_core::{
    CloudEnrollToken, DescribeContractRequest, EnrollAs, EnrollRequest, InspectRequest,
    LocalMachinePhase, Machine, MachineDetails, MachineName, MachineToken, ManagementCapability,
    ManagementClientLabel, SetManagementClientRequest, StorageChoice, TokenRequest, op,
};

use super::{Error, config_path, leaf_matches, runtime};
use crate::cloud_enroll::{
    self, CloudPairing, EnrollIdentity, EnrollStage, InitializeMode, Join, Outcome,
};
use crate::connect::{Client, ConnectError};
use crate::context::{Connection, ContextError, SelectedConnections, Transport};
use crate::provisioning::Installation;
use crate::setup_report::{SetupReport, Step};
use crate::ui::{self, Hint};

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
    enroll_token(root, token, cloud_url, &|installation| async move {
        match installation {
            Installation::Host(storage) => {
                crate::provisioning::provision_local(env!("CARGO_PKG_VERSION"), &storage).await?;
            }
            Installation::Daemon => crate::provisioning::synchronize_local_daemon().await?,
        }
        Ok(())
    })
}

/// Run token enrollment, installing this CLI's daemon version with `install`.
///
/// # Errors
///
/// Returns the same CLI failure as `ployz server add` given an enrollment token.
#[doc(hidden)]
pub fn enroll_with_installer<Install, InstallFuture>(
    root: &ArgMatches,
    install: &Install,
) -> Result<(), Error>
where
    Install: Fn(Installation) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let matches = leaf_matches(root);
    let token = super::server::enroll_token(matches)?.ok_or_else(|| {
        Error::usage(
            "Pass the enrollment token with --token-stdin, --token-file or PLOYZ_ENROLL_TOKEN",
        )
    })?;
    let cloud_url = matches
        .get_one::<String>("cloud-url")
        .map_or("ployz.dev", String::as_str);
    enroll_token(root, token, cloud_url, install)
}

/// Setup's outcome: `Ok` once the final callback succeeded, holding what printing the
/// committed result returned.
pub(crate) type Setup = Result<Result<(), Error>, Error>;

/// Enroll, then send Cloud the best-effort setup report for the outcome.
fn enroll_token<Install, InstallFuture>(
    root: &ArgMatches,
    token: CloudEnrollToken,
    cloud_url: &str,
    install: &Install,
) -> Result<(), Error>
where
    Install: Fn(Installation) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    runtime()?
        .block_on(run_enrollment(root, &token, cloud_url, install, &|stage| {
            if stage == EnrollStage::Enrolling && !ui::json() {
                ui::note("Asking Cloud to enroll this Server…");
            }
        }))
        .and_then(|printed| printed)
}

/// Enroll as `server add` does, reporting Joining only after the daemon accepts
/// enrollment, then send Cloud the best-effort setup report for the outcome.
pub(crate) async fn run_enrollment<Install, InstallFuture>(
    root: &ArgMatches,
    token: &CloudEnrollToken,
    cloud_url: &str,
    install: &Install,
    progress: &dyn Fn(EnrollStage),
) -> Setup
where
    Install: Fn(Installation) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let mut report = SetupReport::start();
    let setup = enroll_steps(root, token, cloud_url, install, &mut report, progress).await;
    report
        .send(
            &cloud_enroll::report_url(cloud_url),
            token,
            setup.as_ref().err(),
        )
        .await;
    setup
}

async fn enroll_steps<Install, InstallFuture>(
    root: &ArgMatches,
    token: &CloudEnrollToken,
    cloud_url: &str,
    install: &Install,
    report: &mut SetupReport,
    progress: &dyn Fn(EnrollStage),
) -> Setup
where
    Install: Fn(Installation) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let matches = leaf_matches(root);
    let remote = matches.get_one::<String>("destination").is_some();
    // First, so a failed first install still reports what it ran on.
    if !remote && dials_this_host(matches) {
        report.read_host();
    }
    let initial_policy = super::server::enrollment_policy(matches)?;
    let url = cloud_enroll::enroll_url(cloud_url);
    let requested_name = matches
        .get_one::<String>("name")
        .map(MachineName::parse)
        .transpose()?;
    let requested_storage = super::server::requested_storage(matches);
    let cluster_network = *matches
        .get_one::<Ipv4Net>("network")
        .expect("Cluster network has a default");

    progress(EnrollStage::Installing);
    if remote && !matches.get_flag("no-install") {
        // A remote host is provisioned once, up front, with this CLI's release.
        crate::provisioning::provision(matches, &requested_storage).await?;
    }
    let mut client = connect_machine(matches, install).await?;
    client = synchronize_daemon(matches, client, install).await?;
    if matches.get_flag("reset") {
        client = ensure_uninitialized(matches, matches.get_flag("yes"), true, client).await?;
    }
    report.step(Step::Enroll);
    let (details, machine_token, name, outcome) = enroll_current_identity(
        &mut client,
        requested_name,
        requested_storage.choice(),
        &initial_policy,
        &url,
        token,
        progress,
    )
    .await?;
    report.sizes_from(&machine_token);
    report.step(Step::Storage);
    match outcome {
        Outcome::Join(join) => {
            report.enrolled(join.storage.choice(), false);
            enroll_join(
                matches,
                client,
                details,
                *join,
                &initial_policy,
                &cloud_enroll::callback_url(cloud_url),
                token,
                install,
                report,
                progress,
            )
            .await
        }
        Outcome::Initialize {
            mode,
            pairing,
            storage,
        } => {
            report.enrolled(storage.choice(), true);
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
                progress,
            )
            .await
        }
    }
}

/// The local Machine this enrollment already joined, if any. A participating Machine counts
/// only when Cloud found it listed as it published itself; Cloud registers a forgotten one
/// afresh, with no daemon version, and its participation predates this enrollment.
fn already_assigned<'a>(details: &'a MachineDetails, assigned: &Machine) -> Option<&'a Machine> {
    let resumable = match details.phase {
        LocalMachinePhase::Joining => true,
        LocalMachinePhase::Participating => !assigned.runtime.daemon_version.is_empty(),
        LocalMachinePhase::Uninitialized
        | LocalMachinePhase::Resetting
        | LocalMachinePhase::Unrecognized(_) => false,
    };
    resumable
        .then_some(details.machine.as_ref())
        .flatten()
        .filter(|machine| machine.id == assigned.id)
}

/// A local participant Cloud doesn't list as joined is a rerun that outran its own
/// publication, or a forgotten Server. Neither is proven, so the refusal names both.
fn unlisted_participation(details: &MachineDetails, assigned: &Machine) -> Result<(), Error> {
    let participating = details.phase == LocalMachinePhase::Participating
        && details
            .machine
            .as_ref()
            .is_some_and(|machine| machine.id == assigned.id);
    if !participating {
        return Ok(());
    }
    Err(Error::conflict(format!(
        "Server {} already participates, but Cloud doesn't list it as joined. If an add of this Server just stopped partway, run the same command again in a minute; if the Server was forgotten, rerun with --reset to reset it before enrollment.",
        assigned.name
    )))
}

async fn enroll_current_identity(
    client: &mut Client,
    requested_name: Option<MachineName>,
    requested_storage: StorageChoice,
    initial_policy: &ployz_core::InitialMachinePolicy,
    url: &str,
    token: &CloudEnrollToken,
    progress: &dyn Fn(EnrollStage),
) -> Result<(MachineDetails, MachineToken, MachineName, Outcome), Error> {
    let (details, machine_token) = crate::handlers::server::inspect_token(
        client,
        InspectRequest {
            token: Some(TokenRequest::default()),
            ..Default::default()
        },
    )
    .await?;
    let name = crate::handlers::server::machine_name(requested_name, &machine_token)?;
    let identity = EnrollIdentity::from_machine_token(
        name.clone(),
        &machine_token,
        requested_storage,
        initial_policy.clone(),
    );
    progress(EnrollStage::Enrolling);
    let outcome = cloud_enroll::enroll(url, token, &identity, progress).await?;
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
    token: &CloudEnrollToken,
    install: &Install,
    report: &mut SetupReport,
    progress: &dyn Fn(EnrollStage),
) -> Setup
where
    Install: Fn(Installation) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let assigned = join.registration.assigned_machine.clone();
    let local = already_assigned(&details, &assigned);
    if !initial_policy.matches(&assigned)
        || local.is_some_and(|machine| !initial_policy.matches(machine))
    {
        return Err(Error::conflict(
            "initial policy differs from the currently observed Server; enrollment does not edit an existing Server",
        ));
    }
    let pairing = join.pairing;
    let mut ready = if local.is_some() {
        report.step(Step::Join);
        progress(EnrollStage::Joining);
        if details.phase == LocalMachinePhase::Joining {
            wait_phase(
                matches,
                LocalMachinePhase::Participating,
                "the joined Server did not become ready",
            )
            .await?
        } else {
            client
        }
    } else {
        unlisted_participation(&details, &assigned)?;
        client = ensure_uninitialized(
            matches,
            matches.get_flag("yes"),
            matches.get_flag("reset"),
            client,
        )
        .await?;
        progress(EnrollStage::Storage);
        client = provision_storage(matches, client, &join.storage, install).await?;
        report.step(Step::Join);
        crate::handlers::server::enroll(
            &mut client,
            EnrollRequest {
                role: EnrollAs::Member {
                    registration: join.registration,
                },
                wireguard_mtu: matches.get_one::<u32>("wg-mtu").copied(),
                pinned: Default::default(),
            },
        )
        .await?;
        progress(EnrollStage::Joining);
        wait_phase(
            matches,
            LocalMachinePhase::Participating,
            "the joined Server did not become ready",
        )
        .await?
    };
    progress(EnrollStage::Publishing);
    // Mint a fresh capability; Cloud verifies replacements when enrollment resumes.
    let capability = set_cloud_management_client(matches, &mut ready).await?;
    let (unarrived, catch_up) =
        crate::global_catch_up::follow_globals(&mut ready, &assigned, |args| {
            super::rerun(matches, args)
        })
        .await;
    // Cloud may use the replacement after publication, revoking this key.
    // A committed join remains enrolled even when Global catch-up needs a separate retry.
    cloud_enroll::publish(
        callback_url,
        token,
        assigned.id,
        &pairing.secret,
        &capability,
    )
    .await?;
    cloud_enroll::callback(callback_url, token, assigned.id, &pairing.secret).await?;
    ui::stream(format_args!("Joined Server {}.", assigned.name));
    // The join is committed; a catch-up failure makes it partial.
    Ok(ui::emit_committed(
        crate::global_catch_up::with_unarrived(
            serde_json::json!({ "server": super::server::server_json(&assigned), "founded": false }),
            &unarrived,
        ),
        catch_up.map_err(|error| {
            crate::global_catch_up::joined_catch_up_error(error, &assigned, |args| {
                super::rerun(matches, args)
            })
        }),
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
    storage: ployz_core::StoragePreparation,
    cloud_url: &str,
    token: &CloudEnrollToken,
    install: &Install,
    report: &mut SetupReport,
    progress: &dyn Fn(EnrollStage),
) -> Setup
where
    Install: Fn(Installation) -> InstallFuture,
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
            progress(EnrollStage::Joining);
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
            progress(EnrollStage::Storage);
            client = provision_storage(matches, client, &storage, install).await?;
            report.step(Step::Join);
            let machine = crate::handlers::server::enroll(
                &mut client,
                EnrollRequest {
                    role: EnrollAs::Founder {
                        initial_policy,
                        name,
                        cluster_network,
                        public_ip: machine_token.public_ip,
                        advertised_endpoints: machine_token.advertised_endpoints,
                    },
                    wireguard_mtu: matches.get_one::<u32>("wg-mtu").copied(),
                    pinned: Default::default(),
                },
            )
            .await?;
            progress(EnrollStage::Joining);
            let ready = wait_phase(
                matches,
                LocalMachinePhase::Participating,
                "the first Server did not become ready",
            )
            .await?;
            (machine, ready)
        }
    };

    if machine.accepts_ingress
        && let Some(requested) = ingress
    {
        // An interrupted Apply may have completed mutations. Do not replay it.
        let _ingress =
            crate::deploy::apply_requested(&mut ready, &requested, false, false, "default")
                .await
                .map_err(|error| {
                    ingress_incomplete(super::ingress_hints(Error::from(error), |args| {
                        super::rerun(matches, args)
                    }))
                })?;
    }
    progress(EnrollStage::Publishing);
    // Repeated Set stages a fresh capability; its first operational RPC completes rotation.
    let capability = set_cloud_management_client(matches, &mut ready).await
        .map_err(|error| error.context("Server initialized; Cloud Pairing publication incomplete. Rerun the same ployz server add command without --reset, keeping all other options."))?;
    cloud_enroll::publish(
        &cloud_enroll::callback_url(cloud_url),
        token,
        machine.id,
        &pairing.secret,
        &capability,
    )
    .await?;
    cloud_enroll::callback(
        &cloud_enroll::callback_url(cloud_url),
        token,
        machine.id,
        &pairing.secret,
    )
    .await?;
    Ok(ui::emit(&founder_result(&machine, founding)))
}

/// The first Server is enrolled but its Ingress Proxy deploy failed.
pub(crate) fn ingress_incomplete(error: Error) -> Error {
    let headline =
        super::ingress_headline("Server initialized; Ingress deployment incomplete", &error);
    error.context(format!("{headline} Rerun the same ployz server add command without --reset, keeping all other options, to reconcile it."))
}

/// The first Server's result. Founding it, Cloud deploys the Organization's saved
/// Environments to it, as Deployments admitted after this returns.
fn founder_result(machine: &Machine, founding: bool) -> serde_json::Value {
    let next = founding.then_some("ployz deployment ls");
    if founding {
        ui::stream(format_args!(
            "Initialised Server {}; Cloud now deploys any saved Environments to it.",
            machine.name
        ));
    } else {
        ui::stream(format_args!("Enrolled Server {}.", machine.name));
    }
    if let Some(next) = next {
        ui::hint(&ui::Hint::Inspect(next.to_owned()));
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
    storage: &ployz_core::StoragePreparation,
    install: &Install,
) -> Result<Client, Error>
where
    Install: Fn(Installation) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let storage = cloud_enroll::storage_with_requested_device(
        storage,
        &super::server::requested_storage(matches),
    );
    crate::provisioning::announce_storage(&storage);
    if storage.choice() != StorageChoice::Zfs {
        return Ok(client);
    }
    if !installs_here(matches, &client) {
        return Err(Error::usage(format!(
            "zfs storage preparation requires running ployz server add on the Server itself; connected through {}",
            client.connection()
        )));
    }
    install(Installation::Host(storage)).await?;
    wait_matching_daemon(matches).await
}

async fn synchronize_daemon<Install, InstallFuture>(
    matches: &ArgMatches,
    mut client: Client,
    install: &Install,
) -> Result<Client, Error>
where
    Install: Fn(Installation) -> InstallFuture,
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
    install(Installation::Daemon).await?;
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
    crate::connect::connect_selected_in_order(
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

async fn connect_machine<Install, InstallFuture>(
    matches: &ArgMatches,
    install: &Install,
) -> Result<Client, Error>
where
    Install: Fn(Installation) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    match dial(matches).await {
        Ok(client) => Ok(client),
        Err(ConnectError::Context(ContextError::NoConfig)) => {
            install(Installation::Host(StorageChoice::None.into())).await?;
            wait_client(matches).await
        }
        Err(error) if error.is_setup_retryable() && dials_this_host(matches) => {
            if local_selection(matches)?.connections.iter().any(|connection| {
                matches!(connection.transport(), Transport::Unix(path) if !path.exists())
            }) {
                install(Installation::Host(StorageChoice::None.into())).await?;
            }
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
    crate::handlers::server::leave(&mut client).await?;
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

pub(super) fn group(command: Command) -> Command {
    command.about("Manage Cloud")
}

pub(super) fn reset_command(command: Command) -> Command {
    base(command, "Give up your Organization's founding that never finished")
                .long_about("Give up your Organization's founding that never finished, so the next ployz server add founds the Cluster again. Stop or erase the founding Server first. Needs `ployz login`.")
                .arg(
                    switch("yes", Some('y'))
                        .help("Confirm the founding Server is stopped or erased"),
                )
}

pub(super) fn reset(root: &ArgMatches) -> Result<(), Error> {
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
                "organizationId": organization.id,
                "confirmedFounderStoppedOrErased": true,
            })),
        )
        .await?;
        ui::stream(format_args!("Reset the founding of {}.", organization.slug));

        crate::ui::emit(&serde_json::json!({ "reset": true, "organization": organization }))
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
            selection: ConnectionSource::LocalSocket,
            attempts: 1,
            server: None,
            retry: ployz_core::Retry::Never,
            reached: false,
            last: None,
        }));
        assert!(!retry_local_connect(&ConnectError::Context(
            ContextError::NoCurrentContext(PathBuf::from("config.yaml"))
        )));
    }

    #[tokio::test]
    async fn unacknowledged_enroll_never_reports_committed_joining() {
        use ployz_core::{OpaquePayload, RpcRequestBody, RpcResponse};
        use std::cell::RefCell;
        use tonic::{Request, Response};

        for founder in [false, true] {
            let mut machine = crate::global_fixtures::machine('a', "web-3");
            machine.accepts_ingress = false;
            let details = MachineDetails {
                id: machine.id,
                phase: LocalMachinePhase::Uninitialized,
                public_key: machine.public_key,
                advertised_endpoints: Vec::new(),
                machine: None,
                store_version: Default::default(),
                rtts: Vec::new(),
                management_clients: Vec::new(),
                telemetry: None,
                storage: None,
                upgrade: None,
                token: None,
            };
            let observed = details.clone();
            let reached = std::sync::Arc::new(tokio::sync::Notify::new());
            let received = std::sync::Arc::clone(&reached);
            let (client, server) =
                crate::connect::test_support::rpc_client(move |request: Request<OpaquePayload>| {
                    let details = observed.clone();
                    let received = std::sync::Arc::clone(&received);
                    async move {
                        let request = request.into_inner().decode_request().unwrap().body;
                        if matches!(request, RpcRequestBody::Inspect(_)) {
                            return Ok(Response::new(RpcResponse::from(details).encode().unwrap()));
                        }
                        assert!(
                            matches!(request, RpcRequestBody::Enroll(_)),
                            "unexpected RPC: {request:?}"
                        );
                        received.notify_one();
                        std::future::pending().await
                    }
                })
                .await;
            let root = crate::cli::command()
                .try_get_matches_from(["ployz", "server", "add", "--yes"])
                .unwrap();
            let matches = leaf_matches(&root);
            let token = CloudEnrollToken::parse("pmet_unacknowledged_enroll").unwrap();
            let pairing = CloudPairing {
                secret: cloud_enroll::PairingCredential::parse("pairing").unwrap(),
            };
            let policy = ployz_core::InitialMachinePolicy {
                accepts_ingress: false,
                ..Default::default()
            };
            let mut report = SetupReport::start();
            let stages = RefCell::new(Vec::new());
            let progress = |stage| stages.borrow_mut().push(stage);
            let install = |_| async { Ok(()) };
            let enrolling = async {
                if founder {
                    enroll_founder(
                        matches,
                        client,
                        details,
                        MachineToken {
                            id: machine.id,
                            public_key: machine.public_key,
                            public_ip: None,
                            advertised_endpoints: Vec::new(),
                            runtime: Default::default(),
                            memory_total_bytes: None,
                            disk_total_bytes: None,
                            disk_available_bytes: None,
                        },
                        machine.name.clone(),
                        policy,
                        "10.210.0.0/16".parse().unwrap(),
                        InitializeMode::New,
                        pairing,
                        StorageChoice::None.into(),
                        "http://unused.invalid",
                        &token,
                        &install,
                        &mut report,
                        &progress,
                    )
                    .await
                } else {
                    enroll_join(
                        matches,
                        client,
                        details,
                        Join {
                            pairing,
                            storage: StorageChoice::None.into(),
                            registration: ployz_core::Registered {
                                assigned_machine: machine,
                                visible_peers: Vec::new(),
                                target_versions: Default::default(),
                            },
                        },
                        &policy,
                        "http://unused.invalid",
                        &token,
                        &install,
                        &mut report,
                        &progress,
                    )
                    .await
                }
            };
            tokio::pin!(enrolling);
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    outcome = &mut enrolling => panic!("unacknowledged enrollment finished: {outcome:?}"),
                    () = reached.notified() => {}
                }
            })
            .await
            .expect("the RPC fixture receives enrollment");
            server.abort();
            assert_eq!(
                *stages.borrow(),
                vec![EnrollStage::Storage],
                "founder={founder}: an unacknowledged RPC must leave a cancellable saved stage"
            );
        }
    }

    #[test]
    fn a_forgotten_server_rejoining_through_cloud_is_reset_first() {
        let listed = {
            let mut machine = crate::global_fixtures::machine('a', "web-3");
            machine.runtime.daemon_version = "0.3.0".into();
            machine
        };
        let registered = crate::global_fixtures::machine('a', "web-3");
        let details = |phase| MachineDetails {
            id: listed.id,
            phase,
            public_key: listed.public_key,
            advertised_endpoints: Vec::new(),
            machine: Some(listed.clone()),
            store_version: Default::default(),
            rtts: Vec::new(),
            management_clients: Vec::new(),
            telemetry: None,
            storage: None,
            upgrade: None,
            token: None,
        };
        assert_eq!(
            already_assigned(&details(LocalMachinePhase::Participating), &listed),
            Some(&listed),
            "the Cluster lists the Machine as it published itself"
        );
        assert_eq!(
            already_assigned(&details(LocalMachinePhase::Participating), &registered),
            None,
            "Cloud registered it afresh, so its participation predates this enrollment"
        );
        assert_eq!(
            already_assigned(&details(LocalMachinePhase::Joining), &registered),
            Some(&listed),
            "a join under way hasn't published itself yet"
        );
        let refusal =
            unlisted_participation(&details(LocalMachinePhase::Participating), &registered)
                .unwrap_err()
                .to_string();
        assert!(
            refusal.contains("run the same command again") && refusal.contains("--reset"),
            "an unlisted participant may be a rerun that outran its publication or a forgotten Server: {refusal}"
        );
        assert!(
            unlisted_participation(&details(LocalMachinePhase::Uninitialized), &registered).is_ok()
        );
    }

    #[test]
    fn a_rerun_resumes_a_join_already_under_way_for_the_assigned_machine() {
        let mut assigned = crate::global_fixtures::machine('a', "web-3");
        assigned.runtime.daemon_version = "0.3.0".into();
        let other = crate::global_fixtures::machine('b', "web-4");
        let details = |phase, machine: Option<&Machine>| MachineDetails {
            id: assigned.id,
            phase,
            public_key: assigned.public_key,
            advertised_endpoints: Vec::new(),
            machine: machine.cloned(),
            store_version: Default::default(),
            rtts: Vec::new(),
            management_clients: Vec::new(),
            telemetry: None,
            storage: None,
            upgrade: None,
            token: None,
        };
        for phase in [LocalMachinePhase::Joining, LocalMachinePhase::Participating] {
            assert_eq!(
                already_assigned(&details(phase.clone(), Some(&assigned)), &assigned),
                Some(&assigned),
                "{phase:?}"
            );
            assert_eq!(
                already_assigned(&details(phase, Some(&other)), &assigned),
                None
            );
        }
        assert_eq!(
            already_assigned(&details(LocalMachinePhase::Uninitialized, None), &assigned),
            None
        );
        assert_eq!(
            already_assigned(
                &details(LocalMachinePhase::Resetting, Some(&assigned)),
                &assigned
            ),
            None
        );
    }
}
