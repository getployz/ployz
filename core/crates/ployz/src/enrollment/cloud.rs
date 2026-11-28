//! Cloud enrollment: what `ployz server add` with a token runs.

use crate::cloud;
use crate::cluster::{Call, Deadline, Wait};
use std::{future::Future, time::Duration};

use ployz_core::{
    CloudEnrollToken, DescribeContractRequest, EnrollAs, EnrollRequest, Hint, InspectRequest,
    LocalMachinePhase, Machine, MachineDetails, MachineName, MachineToken, ManagementCapability,
    ManagementClientLabel, PinnedAddresses, RecoveryCode, SetManagementClientRequest,
    StorageChoice, op,
};

use super::recovery::{KEEP_THE_CODE, RECOVERY_CODE_NOT_SAVED, wrap_sealing_key};
use super::{AskedPolicy, EnrollPlan, founding, machine};
use crate::cloud::enroll::{
    CloudPairing, EnrollIdentity, EnrollStage, Initialize, InitializeMode, Join, Outcome, Recovery,
};
use crate::cloud::origin::CloudOrigin;
use crate::connect::{Client, ConnectError};
use crate::context::{Connection, ContextError, SelectedConnections, Transport};
use crate::device::{self, JoinEntry};
use crate::failure::Failure as Error;
use crate::provisioning::Installation;
use crate::setup_report::{SetupReport, Step};
use crate::ui::{self, Fields};

/// An enrollment whose final callback succeeded: the Server is enrolled even when what
/// followed failed.
#[derive(Debug)]
pub(crate) struct Enrolled {
    /// Printing the committed result, or Global catch-up, failed after the join.
    pub(crate) follow_up: Option<Error>,
}

/// One Cloud enrollment: what it asks, the token Cloud issued, and
/// `install`, which installs this CLI's daemon version on the enrolling host.
pub(crate) struct Enrollment<'a, Install> {
    pub(crate) plan: &'a EnrollPlan,
    pub(crate) token: &'a CloudEnrollToken,
    pub(crate) cloud_url: &'a str,
    pub(crate) install: &'a Install,
    pub(crate) progress: &'a dyn Fn(EnrollStage),
}

/// The Server this enrollment asked Cloud for, as its daemon reported it.
struct Identified {
    details: MachineDetails,
    machine_token: MachineToken,
    name: MachineName,
    initial_policy: ployz_core::InitialMachinePolicy,
    identity: EnrollIdentity,
}

impl<Install, InstallFuture> Enrollment<'_, Install>
where
    Install: Fn(Installation) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    /// Enroll, reporting Joining only after the daemon accepts enrollment, then send Cloud
    /// the best-effort setup report for the outcome.
    pub(crate) async fn run(&self) -> Result<Enrolled, Error> {
        let mut report = SetupReport::start();
        let setup = self.steps(&mut report).await;
        report
            .send(
                &CloudOrigin::new(self.cloud_url).report_url(),
                self.token,
                setup.as_ref().err(),
            )
            .await;
        setup
    }

    async fn steps(&self, report: &mut SetupReport) -> Result<Enrolled, Error> {
        let plan = self.plan;
        // First, so a failed first install still reports what it ran on.
        if dials_this_host(plan) {
            report.read_host();
        }
        let asked = plan.shaping().policy()?;
        let requested_name = plan.shaping().name()?;
        let cluster_name = plan.shaping().cluster_name()?;
        let requested_storage = plan.shaping().storage();
        // `--public-ip` and `--wg-endpoint` shape the identity Cloud registers, and pin what the
        // daemon would otherwise rediscover, as a direct join does (#3515).
        let token_request = plan.shaping().token_request()?;
        let pinned = machine::pinned(&token_request);

        (self.progress)(EnrollStage::Installing);
        if let Some(remote) = plan.remote_target()
            && !plan.no_install()
        {
            // A remote host is provisioned once, up front, with this CLI's release.
            crate::provisioning::provision_target(
                remote,
                &plan.shaping().version(),
                &requested_storage,
            )
            .await?;
        }
        let mut client = connect_machine(plan, self.install).await?;
        client = synchronize_daemon(plan, client, self.install).await?;
        if let Some(remote) = plan.remote_target() {
            // A Cluster this founds trusts this computer's Device from the start. A Server
            // whose disk already holds a key keeps it. After the dial, which already
            // accepted the Server's host key.
            let line = ployz_core::first_key::LineKey::Device {
                key: crate::handlers::device::own_key(&plan.reach().config())?,
            };
            crate::provisioning::arm_first_key(remote, &line).await?;
        }
        if plan.shaping().reset() {
            client = ensure_uninitialized(plan, client).await?;
        }
        report.step(Step::Enroll);
        let (identified, outcome) = self
            .identify(
                &mut client,
                token_request,
                requested_name,
                requested_storage.choice(),
                &asked,
            )
            .await?;
        report.sizes_from(&identified.machine_token);
        report.step(Step::Storage);
        match outcome {
            Outcome::Join(join) => {
                report.enrolled(join.storage.choice(), false);
                if cluster_name.is_some() {
                    super::plan::joined_unnamed();
                }
                if plan.shaping().ingress_closed() {
                    super::plan::joined_open();
                }
                self.join(client, identified, *join, pinned, report).await
            }
            Outcome::Initialize(initialize) => {
                report.enrolled(initialize.storage.choice(), true);
                self.found(client, identified, pinned, initialize, report)
                    .await
            }
        }
    }

    async fn identify(
        &self,
        client: &mut Client,
        token_request: InspectRequest,
        requested_name: Option<MachineName>,
        requested_storage: StorageChoice,
        asked: &AskedPolicy,
    ) -> Result<(Identified, Outcome), Error> {
        let (plan, token, progress) = (self.plan, self.token, self.progress);
        let url = CloudOrigin::new(self.cloud_url).enroll_url();
        let (details, machine_token) = machine::inspect_token(client, token_request).await?;
        let initial_policy =
            asked.settle(existing(&details), |args| plan.reach().rerun(args).line())?;
        let name = machine::machine_name(requested_name, &machine_token);
        let recovery_supported = cloud::enroll::recovery_compat::supported(client).await?;
        let identity = EnrollIdentity::from_machine_token(
            name.clone(),
            &machine_token,
            requested_storage,
            initial_policy.clone(),
            recovery_supported,
        );
        progress(EnrollStage::Enrolling);
        let outcome = match cloud::enroll::enroll(&url, token, &identity, progress).await {
            Err(error)
                if error.capability().as_deref() == Some(ployz_core::JOIN_APPROVAL_CAPABILITY) =>
            {
                let config = plan.reach().config();
                if device::key::load(&device::key::path(&config))?.is_some() {
                    let mut entry = plan.reach().entry().await?;
                    let expected = ployz_core::RegisterRequest {
                        join_approval: None,
                        join_confirmation: None,
                        machine_id: machine_token.id,
                        assigned_subnet: None,
                        initial_policy: initial_policy.clone(),
                        name: name.clone(),
                        storage: requested_storage,
                        public_key: machine_token.public_key,
                        public_ip: machine_token.public_ip,
                        advertised_endpoints: machine_token.advertised_endpoints.clone(),
                        runtime: Default::default(),
                        capabilities: Vec::new(),
                        first_key: None,
                    };
                    // This join's pin is this computer's Device alone, whatever the route.
                    let from = if crate::sealing_pin::SealTo::direct(&entry) {
                        JoinEntry::Chosen {
                            pins_its_devices: false,
                        }
                    } else {
                        JoinEntry::relayed(&config).await?
                    };
                    if device::sign_waiting_join(&mut entry, &config, &expected, from).await? {
                        cloud::enroll::enroll(&url, token, &identity, progress).await?
                    } else {
                        return Err(error.into());
                    }
                } else if !dials_this_host(plan) {
                    return Err(unpinnable_join(&name));
                } else {
                    return Err(Error::from(error).hint(Hint::Next(format!(
                        "ployz device approve --join {}",
                        ployz_core::authority::join::code(
                            machine_token.id,
                            machine_token.public_key
                        ),
                    ))));
                }
            }
            result => result?,
        };
        Ok((
            Identified {
                details,
                machine_token,
                name,
                initial_policy,
                identity,
            },
            outcome,
        ))
    }

    async fn join(
        &self,
        mut client: Client,
        identified: Identified,
        join: Join,
        pinned: PinnedAddresses,
        report: &mut SetupReport,
    ) -> Result<Enrolled, Error> {
        let (plan, token, progress) = (self.plan, self.token, self.progress);
        let Identified {
            details,
            initial_policy,
            identity,
            ..
        } = identified;
        let callback_url = CloudOrigin::new(self.cloud_url).callback_url();
        let enroll_url = CloudOrigin::new(self.cloud_url).enroll_url();
        let assigned = join.registration.assigned_machine.clone();
        let local = already_assigned(&details, &assigned);
        // Cloud already listed this Server. Until its daemon takes the join, a failure leaves
        // that listing behind, so it names the command that takes it out. A daemon that already
        // runs the assigned Machine is that Server, never a leftover.
        let runs_assigned = details
            .machine
            .as_ref()
            .is_some_and(|machine| machine.id == assigned.id);
        let undo = (local.is_none() && !runs_assigned)
            .then(|| Hint::Undo(super::output::forget_unarrived(assigned.id)));
        let undoable = |error: Error| error.hint(undo.clone());
        if local.is_none()
            && unlisted_participation(&details, &assigned).is_err()
            && another_cluster(&details, &join.registration)
        {
            return Err(refuse_another_cluster(join));
        }
        if !initial_policy.matches(&assigned)
            || local.is_some_and(|machine| !initial_policy.matches(machine))
        {
            return Err(undoable(Error::conflict(
                "initial policy differs from the currently observed Server; enrollment does not edit an existing Server",
            )));
        }
        let pairing = join.pairing;
        // An approved join is published only once the joiner's confirmation comes back through
        // Cloud. A rerun that finds the Server still joining replays its Enroll to read it again.
        let relays =
            assigned.join_approval.is_some() && details.phase != LocalMachinePhase::Participating;
        let pin = cli_join_pin(plan, &assigned.name).map_err(undoable)?;
        let member = EnrollRequest {
            role: EnrollAs::Member {
                registration: join.registration,
                pin,
            },
            wireguard_mtu: plan.shaping().wg_mtu(),
            pinned,
        };
        let confirmation = if local.is_some() {
            report.step(Step::Join);
            if relays {
                machine::enroll_member(&mut client, member).await?.1
            } else {
                None
            }
        } else {
            unlisted_participation(&details, &assigned)?;
            client = ensure_uninitialized(plan, client).await.map_err(undoable)?;
            progress(EnrollStage::Storage);
            client = provision_storage(plan, client, &join.storage, self.install)
                .await
                .map_err(undoable)?;
            report.step(Step::Join);
            machine::enroll_member(&mut client, member).await?.1
        };
        if relays {
            let confirmation = confirmation.ok_or_else(machine::confirmation_missing)?;
            let answered = cloud::enroll::enroll(
                &enroll_url,
                token,
                &identity.confirming(confirmation),
                progress,
            )
            .await?;
            let claimed = identity.claims(ployz_core::JOIN_TRUST_CAPABILITY);
            match answered {
                Outcome::Join(join) => {
                    // The daemon restarts after it enrolls, so this dials the same connection again.
                    let via = client.connection().clone();
                    crate::enrollment::confirm_join(
                        async || {
                            machine::connect_direct_within(plan.reach().ssh_timeout(), &via).await
                        },
                        claimed,
                        &join.registration,
                    )
                    .await?;
                }
                Outcome::Initialize(_) if claimed => {
                    return Err(Error::conflict(
                        "Cloud answered this Server's join confirmation with a founding. Nothing joined; run the install command again.",
                    ));
                }
                Outcome::Initialize(_) => {}
            }
        }
        progress(EnrollStage::Joining);
        let mut ready = wait_phase(
            plan,
            Awaited::Enrolled {
                id: assigned.id,
                via: client.connection(),
            },
            "the joined Server did not become ready",
        )
        .await?;
        let sealing = sealing(&mut ready).await?;
        progress(EnrollStage::Publishing);
        // Mint a fresh capability; Cloud verifies replacements when enrollment resumes.
        let capability = set_cloud_management_client(plan, &mut ready).await?;
        let (unarrived, catch_up) =
            super::catch_up::follow_globals(&mut ready, &assigned, |args| plan.reach().rerun(args))
                .await;
        // Cloud may use the replacement after publication, revoking this key.
        // A committed join remains enrolled even when Global catch-up needs a separate retry.
        cloud::enroll::publish(
            &callback_url,
            token,
            assigned.id,
            &pairing.secret,
            &capability,
        )
        .await?;
        cloud::enroll::callback(
            &callback_url,
            token,
            assigned.id,
            &pairing.secret,
            sealing,
            None,
        )
        .await?;
        ui::stream(format_args!("Joined Server {}.", assigned.name));
        // The join is committed; a catch-up failure makes it partial.
        let printed = ui::emit_committed(
            super::catch_up::with_unarrived(
                serde_json::json!({ "server": super::output::server_json(&assigned), "founded": false }),
                &unarrived,
            ),
            catch_up.map_err(|error| {
                super::catch_up::joined_catch_up_error(error, &assigned, |args| {
                    plan.reach().rerun(args)
                })
            }),
        );
        Ok(Enrolled {
            follow_up: printed.err(),
        })
    }

    async fn found(
        &self,
        mut client: Client,
        identified: Identified,
        pinned: PinnedAddresses,
        initialize: Initialize,
        report: &mut SetupReport,
    ) -> Result<Enrolled, Error> {
        let (plan, token, progress) = (self.plan, self.token, self.progress);
        let cloud_url = self.cloud_url;
        let Identified {
            details,
            machine_token,
            name,
            initial_policy,
            ..
        } = identified;
        let Initialize {
            mode,
            pairing,
            storage,
            recovery,
        } = initialize;
        let claim = pairing.secret.founding_claim();
        let state = match (mode, details.phase) {
            (InitializeMode::Resume, LocalMachinePhase::Participating) => {
                if !resumes_founding(details.founding, claim) {
                    return Err(refuse_founding(
                        cloud_url,
                        token,
                        details.id,
                        &pairing,
                        "this Server is already in a Cluster this enrollment didn't found",
                        false,
                    )
                    .await);
                }
                FounderLocalState::Resume {
                    machine: Box::new(details.machine.ok_or_else(|| {
                        Error::conflict(
                            "the matching founding Server has no participating identity".to_owned(),
                        )
                    })?),
                }
            }
            (InitializeMode::Resume, LocalMachinePhase::Uninitialized)
            | (InitializeMode::New, LocalMachinePhase::Uninitialized) => {
                FounderLocalState::Initialize
            }
            (InitializeMode::New, phase) => {
                let found = format!(
                    "a new founding claim requires an uninitialized Server, but its local phase is {}",
                    phase.as_str().escape_debug()
                );
                return Err(
                    refuse_founding(cloud_url, token, details.id, &pairing, &found, true).await,
                );
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
        // A rerun on the founded Server finishes or repeats the enrollment; it founds nothing.
        let founding = matches!(state, FounderLocalState::Initialize);
        let machine = match state {
            FounderLocalState::Resume { machine } => {
                report.step(Step::Join);
                progress(EnrollStage::Joining);
                *machine
            }
            FounderLocalState::Initialize => {
                client = ensure_uninitialized(plan, client).await?;
                progress(EnrollStage::Storage);
                client = provision_storage(plan, client, &storage, self.install).await?;
                report.step(Step::Join);
                let request = EnrollRequest {
                    role: EnrollAs::Founder {
                        initial_policy,
                        name,
                        cluster_network: plan.shaping().network(),
                        public_ip: machine_token.public_ip,
                        advertised_endpoints: machine_token.advertised_endpoints,
                        grant: Some(ployz_core::FoundingGrant::Claim { claim }),
                    },
                    wireguard_mtu: plan.shaping().wg_mtu(),
                    pinned,
                };
                let enrolled = match &recovery {
                    Some(Recovery::Seed(seed)) => {
                        machine::enroll_for_recovery(&mut client, request, seed.clone()).await
                    }
                    Some(Recovery::BackUp) | None => machine::enroll(&mut client, request).await,
                };
                let machine = match enrolled {
                    Err(error) if let Some(unkeyed) = unkeyed(&error) => {
                        // The storage it set up stands; the enroll is what was refused.
                        progress(EnrollStage::Enrolling);
                        return Err(refuse_unkeyed(
                            unkeyed, cloud_url, token, details.id, &pairing,
                        )
                        .await);
                    }
                    enrolled => enrolled?,
                };
                progress(EnrollStage::Joining);
                machine
            }
        };
        let mut ready = wait_phase(
            plan,
            Awaited::Enrolled {
                id: machine.id,
                via: client.connection(),
            },
            "the first Server did not become ready",
        )
        .await?;

        let unnamed = founding::name_cluster(&ready, plan.shaping().cluster_name()?, |args| {
            plan.reach().rerun(args).line()
        })
        .await
        .and_then(Result::err);
        let mut partial = None;
        if let Err(error) =
            founding::open_ingress(&mut ready, &machine, plan.shaping().ingress_closed()).await
        {
            // An Ingress Proxy that failed only on other Servers, as a resumed founding finds,
            // does not hold back publication, as a joiner's catch-up failure does not.
            let elsewhere = error.only_elsewhere(&machine.id);
            let error = ingress_incomplete(
                plan,
                super::output::ingress_hints(error.into(), |args| plan.reach().rerun(args)),
            );
            if !elsewhere {
                return Err(error);
            }
            partial = Some(error);
        }
        if let Some(error) = unnamed {
            match &partial {
                Some(_) => ui::warn(error.to_string()),
                None => partial = Some(error),
            }
        }
        let sealing = sealing(&mut ready).await?;
        let shown = super::recovery::shows_code(
            plan.shows_recovery_code(),
            ui::json(),
            ui::stdout_is_terminal(),
        );
        let code = match recovery {
            Some(Recovery::BackUp) if shown => Some(RecoveryCode::generate()),
            Some(Recovery::BackUp) => {
                ui::note("Run ployz recovery rotate to get a recovery code.");
                None
            }
            Some(Recovery::Seed(_)) => {
                ui::note("This Server needs access to the Sealing Key; ployz recover restores it.");
                None
            }
            None => None,
        };
        let backup = match &code {
            Some(code) => Some(wrap_sealing_key(&mut ready, code).await?),
            None => None,
        };
        progress(EnrollStage::Publishing);
        // Repeated Set stages a fresh capability; its first operational RPC completes rotation.
        let capability = set_cloud_management_client(plan, &mut ready)
            .await
            .map_err(|error| {
                error.context(format!(
                    "Server initialized; Cloud Pairing publication incomplete. {}.",
                    cloud::enroll::AGAIN
                ))
            })?;
        let callback_url = CloudOrigin::new(cloud_url).callback_url();
        cloud::enroll::publish(
            &callback_url,
            token,
            machine.id,
            &pairing.secret,
            &capability,
        )
        .await?;
        let reply = cloud::enroll::callback(
            &callback_url,
            token,
            machine.id,
            &pairing.secret,
            sealing,
            backup.as_ref(),
        )
        .await
        .map_err(Error::from)
        .map_err(|error| match &backup {
            Some(_) => error.context(RECOVERY_CODE_NOT_SAVED),
            None => error,
        })?;
        if let Some(backup) = &backup
            && reply.recovery_backup_digest != Some(backup.digest())
        {
            return Err(Error::unavailable(RECOVERY_CODE_NOT_SAVED));
        }
        let printed = founder_result(&machine, founding, code)
            .and_then(|result| ui::emit_committed(result, partial.map_or(Ok(()), Err)));
        Ok(Enrolled {
            follow_up: printed.err(),
        })
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
        "Server {} already participates, but Cloud doesn't list it as joined. If an add of this Server just stopped partway, run the same command again in a minute. If the Server was forgotten, this add's token now belongs to its old identity, which --reset replaces: make a new token with `ployz server add --command --reset` where you're signed in, and run the line it prints on the Server.",
        assigned.name
    )))
}

fn another_cluster(details: &MachineDetails, registration: &ployz_core::Registered) -> bool {
    let Some(local) = details.sealing.and_then(ployz_core::Sealing::public_key) else {
        return false;
    };
    let mut keys = registration.visible_peers.iter().filter_map(|peer| {
        peer.runtime
            .sealing
            .and_then(ployz_core::Sealing::public_key)
    });
    let Some(target) = keys.next() else {
        return false;
    };
    target != local && keys.all(|key| key == target)
}

/// The Register reply's hand-off is for a Server joining this Cluster, so it is dropped unused.
fn refuse_another_cluster(join: Join) -> Error {
    let ployz_core::Registered {
        assigned_machine: assigned,
        sealing_key,
        ..
    } = join.registration;
    drop(sealing_key);
    Error::conflict(format!(
        "Server {} belongs to another Cluster. Remove it from that Cluster first, then add it here with a new token: run `ployz server add --command` where you're signed in, and run the line it prints on the Server.",
        assigned.name
    ))
}

/// The Machine this host already is, which an add run again asks for as it stands. A host
/// with none, or resetting the one it had, becomes a new Server.
fn existing(details: &MachineDetails) -> Option<&Machine> {
    matches!(
        details.phase,
        LocalMachinePhase::Joining | LocalMachinePhase::Participating
    )
    .then_some(details.machine.as_ref())
    .flatten()
}

/// The pin a join run from this CLI carries: this computer's Device, so the new Server
/// refuses an approval Cloud forged under a Device of its own. Run over SSH from a computer
/// with no Device key, nothing could pin the join, so it is refused. Run on the Server itself
/// from Cloud's printed line, it is the install line's residual and goes unpinned.
fn cli_join_pin(
    plan: &EnrollPlan,
    name: &MachineName,
) -> Result<Option<ployz_core::JoinPin>, Error> {
    let own = device::key::load(&device::key::path(&plan.reach().config()))?
        .map(|secret| device::key::public(&secret));
    match own {
        Some(device) => Ok(Some(ployz_core::JoinPin {
            devices: vec![device],
        })),
        None if !dials_this_host(plan) => Err(unpinnable_join(name)),
        None => Ok(None),
    }
}

fn unpinnable_join(name: &MachineName) -> Error {
    Error::conflict(format!(
        "{name} can't join from here: this computer has no device key, so nothing tells the new Server which devices may approve its join. Run the same command from an approved device. Nothing was enrolled."
    ))
}

/// A rerun resumes a founding this claim made. The founding a daemon from before 0.3.15
/// never recorded resumes as it did then, and Cloud checks its Cluster before using it.
fn resumes_founding(founding: ployz_core::Founding, claim: ployz_core::FoundingClaim) -> bool {
    match founding {
        ployz_core::Founding::Claim { claim: founded } => founded == claim,
        ployz_core::Founding::Unrecorded => true,
        ployz_core::Founding::Joined | ployz_core::Founding::Standalone => false,
    }
}

/// Refuse to found on this Server, first letting go of the Founding Claim so the Organization
/// can start with another Server, or with this one once it is reset when `resettable`.
async fn refuse_founding(
    cloud_url: &str,
    token: &CloudEnrollToken,
    machine_id: ployz_core::MachineId,
    pairing: &CloudPairing,
    found: &str,
    resettable: bool,
) -> Error {
    let next = release_founding(cloud_url, token, machine_id, pairing).await;
    let or_this = if resettable {
        ", or add this Server: make a new token with `ployz server add --command --reset` and run the line it prints on this Server. The reset takes it out of its Cluster and removes every container Ployz runs on it"
    } else {
        ""
    };
    Error::conflict(format!(
        "{found}. An Organization's first Server starts a new Cluster. {next}, then add a Server that isn't in a Cluster with a new token{or_this}."
    ))
}

/// Let go of the Founding Claim, and say what that leaves the person to do.
async fn release_founding(
    cloud_url: &str,
    token: &CloudEnrollToken,
    machine_id: ployz_core::MachineId,
    pairing: &CloudPairing,
) -> &'static str {
    let released = cloud::enroll::release(
        &CloudOrigin::new(cloud_url).callback_url(),
        token,
        machine_id,
        &pairing.secret,
    )
    .await;
    if released {
        "Ployz Cloud let go of this founding attempt"
    } else {
        "In Ployz Cloud, reset the founding attempt"
    }
}

/// Why the Server refused to found with no key to trust: it already used its First Key, or
/// it never had one.
fn unkeyed(error: &Error) -> Option<ployz_core::GateReason> {
    match error.connect_error() {
        Some(ConnectError::Remote(remote)) => {
            remote.gate().and_then(|gate| gate.reason).filter(|reason| {
                matches!(
                    reason,
                    ployz_core::GateReason::FirstKeySpent | ployz_core::GateReason::NoFirstKey
                )
            })
        }
        _ => None,
    }
}

/// Refuse a founding on a Server with no key to trust. A token alone can't found there, or
/// Cloud could start a Cluster that trusts nobody, or steer a reset disk into a Cluster of its
/// own, so the way on is a line that names the key, confirmed at the Server's terminal.
async fn refuse_unkeyed(
    reason: ployz_core::GateReason,
    cloud_url: &str,
    token: &CloudEnrollToken,
    machine_id: ployz_core::MachineId,
    pairing: &CloudPairing,
) -> Error {
    let next = release_founding(cloud_url, token, machine_id, pairing).await;
    let why = if reason == ployz_core::GateReason::FirstKeySpent {
        "This Server already used its First Key"
    } else {
        "This Server has no First Key to trust"
    };
    Error::conflict(format!(
        "{why}, so a token alone can't make it found a Cluster. {next}. Nothing was founded."
    ))
    .hint(Hint::Next(
        "run ployz server add --command, then run the line it prints at a terminal on the Server"
            .into(),
    ))
}

enum FounderLocalState {
    Initialize,
    Resume { machine: Box<Machine> },
}

/// Whether the enrolled Machine holds its Cluster's Sealing Key, for Cloud to record at completion.
async fn sealing(client: &mut Client) -> Result<Option<ployz_core::Sealing>, Error> {
    Ok(client
        .call::<op::Inspect>(
            InspectRequest::default(),
            Call::read().waiting(Wait::standard()),
        )
        .await?
        .sealing)
}

/// The first Server is enrolled but its Ingress Proxy deploy failed. `server add` finishes
/// when run again without `--reset`.
pub(crate) fn ingress_incomplete(plan: &EnrollPlan, error: Error) -> Error {
    super::output::ingress_incomplete(
        "Server initialized",
        error,
        plan.reach().again().without("reset").into(),
    )
}

/// The first Server's result. Founding it, Cloud deploys the Organization's saved
/// Environments to it, as Deployments admitted after this returns.
fn founder_result(
    machine: &Machine,
    founding: bool,
    code: Option<RecoveryCode>,
) -> Result<serde_json::Value, Error> {
    let next = founding.then_some("ployz deployment ls");
    if founding {
        ui::stream(format_args!(
            "Initialised Server {}; Cloud now deploys any saved Environments to it.",
            machine.name
        ));
    } else {
        ui::stream(format_args!("Enrolled Server {}.", machine.name));
    }
    let mut result = serde_json::json!({
        "server": super::output::server_json(machine),
        "founded": founding,
        "deploys_saved_environments": founding,
        "next": next,
    });
    if let Some(code) = code {
        let code = code.reveal();
        if ui::json() {
            if let Some(object) = result.as_object_mut() {
                object.insert(
                    "recoveryCode".to_owned(),
                    serde_json::Value::String(code.to_string()),
                );
            }
        } else {
            ui::write_fields(&Fields::new().field("Recovery code", code.as_str())).map_err(
                |error| {
                    Error::caused(
                        ployz_core::RpcErrorCode::Unavailable,
                        RECOVERY_CODE_NOT_SAVED,
                        error,
                    )
                },
            )?;
            ui::note(KEEP_THE_CODE);
        }
    }
    if let Some(next) = next {
        ui::hint(&Hint::Inspect(next.to_owned()));
    }
    Ok(result)
}

/// Take a fresh Management Capability for Cloud's `cloud` Management Client slot.
///
/// The Pairing Credential stays with the CLI; the daemon stores only public keys.
async fn set_cloud_management_client(
    plan: &EnrollPlan,
    client: &mut Client,
) -> Result<ManagementCapability, Error> {
    let response = client
        .call::<op::SetManagementClient>(
            SetManagementClientRequest::Set {
                label: ManagementClientLabel::cloud(),
            },
            Call::keyed().waiting(Wait::standard()),
        )
        .await?;
    let capability = response.capability.ok_or_else(|| {
        Error::coded(
            ployz_core::RpcErrorCode::Internal,
            "Machine set the `cloud` Management Client without a Management Capability",
        )
    })?;
    if matches!(client.connection().transport(), Transport::Management(_)) {
        crate::context::Config::load(plan.reach().config())?
            .save_management_capability(&capability)?;
    }
    Ok(capability)
}

async fn provision_storage<Install, InstallFuture>(
    plan: &EnrollPlan,
    client: Client,
    storage: &ployz_core::StoragePreparation,
    install: &Install,
) -> Result<Client, Error>
where
    Install: Fn(Installation) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let storage = cloud::enroll::storage_with_requested_device(storage, &plan.shaping().storage());
    crate::provisioning::announce_storage(&storage);
    if storage.choice() != StorageChoice::Zfs {
        return Ok(client);
    }
    if !installs_here(plan, &client) {
        return Err(Error::usage(format!(
            "zfs storage preparation requires running ployz server add on the Server itself; connected through {}",
            client.connection()
        )));
    }
    install(Installation::Host(storage)).await?;
    wait_matching_daemon(plan).await
}

async fn synchronize_daemon<Install, InstallFuture>(
    plan: &EnrollPlan,
    mut client: Client,
    install: &Install,
) -> Result<Client, Error>
where
    Install: Fn(Installation) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    let daemon = client
        .call::<op::DescribeContract>(
            DescribeContractRequest {},
            Call::read().waiting(Wait::standard()),
        )
        .await?;
    if daemon.daemon_version == env!("CARGO_PKG_VERSION") {
        return Ok(client);
    }
    if !installs_here(plan, &client) {
        return Err(Error::usage(format!(
            "daemon version synchronization requires running ployz server add on the Server itself; connected through {}",
            client.connection()
        )));
    }
    install(Installation::Daemon).await?;
    wait_matching_daemon(plan).await
}

async fn wait_matching_daemon(plan: &EnrollPlan) -> Result<Client, Error> {
    let mut client = wait_client(plan).await?;
    let daemon = client
        .call::<op::DescribeContract>(
            DescribeContractRequest {},
            Call::read().waiting(Wait::standard()),
        )
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
fn installs_here(plan: &EnrollPlan, client: &Client) -> bool {
    plan.destination().is_some() || matches!(client.connection().transport(), Transport::Unix(_))
}

/// Connect to the enrolling host: `DESTINATION`, else `--connect` or the local daemon.
async fn dial(plan: &EnrollPlan) -> Result<Client, ConnectError> {
    if let Some(destination) = plan.destination() {
        let mut connection: Connection = destination.parse()?;
        if matches!(connection.transport(), Transport::Ssh { .. })
            && let Some(key) = plan.ssh_key()
        {
            connection = connection.with_ssh_key_file(key)?;
        }
        return machine::connect_direct_within(plan.reach().ssh_timeout(), &connection).await;
    }
    crate::connect::connect_selected_in_order(
        local_selection(plan)?,
        std::sync::Arc::new(
            crate::connect::SystemConnector::default().with_ssh_timeout(plan.reach().ssh_timeout()),
        ),
    )
    .await
}

/// What a run with no destination dials: `--connect`, the current context or the local socket.
fn local_selection(plan: &EnrollPlan) -> Result<SelectedConnections, ConnectError> {
    let reach = plan.reach();
    crate::connect::resolve_connections(
        &reach.config(),
        reach.connect().as_deref(),
        None,
        &ployz_core::MachineLayout::production().socket(),
    )
}

/// Whether a run enrolls this host: it has no destination, and it dials only the local
/// socket or finds nothing to dial and so installs the daemon here (see `connect_machine`).
fn dials_this_host(plan: &EnrollPlan) -> bool {
    if plan.destination().is_some() {
        return false;
    }
    match local_selection(plan) {
        Ok(selected) => selected
            .connections
            .iter()
            .all(|connection| matches!(connection.transport(), Transport::Unix(_))),
        Err(error) => matches!(error, ConnectError::Context(ContextError::NoConfig)),
    }
}

async fn connect_machine<Install, InstallFuture>(
    plan: &EnrollPlan,
    install: &Install,
) -> Result<Client, Error>
where
    Install: Fn(Installation) -> InstallFuture,
    InstallFuture: Future<Output = Result<(), Error>>,
{
    match dial(plan).await {
        Ok(client) => Ok(client),
        Err(ConnectError::Context(ContextError::NoConfig)) => {
            install(Installation::Host(StorageChoice::None.into())).await?;
            wait_client(plan).await
        }
        Err(error) if error.is_setup_retryable() && dials_this_host(plan) => {
            if local_selection(plan)?.connections.iter().any(|connection| {
                matches!(connection.transport(), Transport::Unix(path) if !path.exists())
            }) {
                install(Installation::Host(StorageChoice::None.into())).await?;
            }
            wait_client(plan).await
        }
        Err(error) => Err(error.into()),
    }
}

fn retry_local_connect(error: &ConnectError) -> bool {
    matches!(error, ConnectError::Context(ContextError::NoConfig)) || error.is_setup_retryable()
}

async fn wait_client(plan: &EnrollPlan) -> Result<Client, Error> {
    crate::setup_retry::run(
        &mut (),
        &crate::ui::WaitNotices,
        "Waiting for local daemon",
        crate::setup_retry::WAIT,
        retry_local_connect,
        async |_| dial(plan).await,
    )
    .await
    .map_err(Into::into)
}

async fn ensure_uninitialized(plan: &EnrollPlan, mut client: Client) -> Result<Client, Error> {
    let details = client
        .call::<op::Inspect>(
            InspectRequest::default(),
            Call::read().waiting(Wait::standard()),
        )
        .await?;
    if details.phase == LocalMachinePhase::Uninitialized {
        return Ok(client);
    }
    if !plan.shaping().reset() {
        return Err(Error::conflict(
            "This Server is already initialised. This add's token now belongs to the Server's current identity, which --reset replaces: make a new token with `ployz server add --command --reset` where you're signed in, and run the line it prints on the Server, or run a signed-in `ployz server add USER@HOST --reset`, which makes its own token."
                .to_owned(),
        ));
    }
    machine::confirm(
        (!plan.shaping().yes()).then(|| plan.reach().again().switch("yes")),
        "Reset this Server before joining the Cluster?",
    )?;
    machine::leave(&mut client, &details).await?;
    wait_phase(plan, Awaited::Uninitialized, "The Server did not reset.").await
}

/// What an enrollment waits for after leaving or joining.
#[derive(Clone, Copy)]
enum Awaited<'a> {
    /// The Server has left, ready to enroll afresh.
    Uninitialized,
    /// This enrollment's Machine participates and lists itself, reached again
    /// through the connection the enrollment used: a re-dial of the whole
    /// selection would stop at whichever earlier entry answers first.
    Enrolled {
        id: ployz_core::MachineId,
        via: &'a Connection,
    },
}

async fn wait_phase(
    plan: &EnrollPlan,
    awaited: Awaited<'_>,
    timeout_message: &str,
) -> Result<Client, Error> {
    let (phase, expected, wait) = match awaited {
        Awaited::Uninitialized => (
            LocalMachinePhase::Uninitialized,
            None,
            Duration::from_secs(60),
        ),
        // The daemon restarts after it enrolls (#3581).
        Awaited::Enrolled { .. } => (
            LocalMachinePhase::Participating,
            Some(machine::STARTING),
            ployz_core::MACHINE_START_WAIT,
        ),
    };
    let mut joining = machine::Joining::default();
    crate::setup_retry::run_expecting(
        &mut joining,
        &crate::ui::WaitNotices,
        &format!("Waiting for the {} phase", phase.as_str()),
        expected,
        wait,
        ConnectError::is_start_retryable,
        async |joining| {
            if let Awaited::Enrolled { id, via } = awaited {
                let mut client =
                    machine::connect_direct_within(plan.reach().ssh_timeout(), via).await?;
                check_enrolled(&mut client, id, joining).await?;
                return Ok(client);
            }
            let mut client = dial(plan).await?;
            let details = client
                .call::<op::Inspect>(
                    InspectRequest::default(),
                    Call::read().waiting(Wait::standard()),
                )
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
    .map_err(|error| match awaited {
        Awaited::Enrolled { .. } => joining.failed(error, timeout_message),
        Awaited::Uninitialized => Error::from(error).context(timeout_message.to_owned()),
    })
}

/// One readiness attempt pinned to the enrolled Machine. A re-dial can fall
/// through to another Machine of the selection, which is not this enrollment's
/// Server even when it lists itself.
async fn check_enrolled(
    client: &mut Client,
    expected: ployz_core::MachineId,
    joining: &mut machine::Joining,
) -> Result<(), ConnectError> {
    let details = client
        .call::<op::Inspect>(
            InspectRequest::default(),
            Call::once().within(Deadline::SetupRead),
        )
        .await?;
    let reached = details.machine.map_or(details.id, |machine| machine.id);
    if reached != expected {
        return Err(ConnectError::Attempt(
            format!("reached Machine {reached}, not the enrolled Machine {expected}").into(),
        ));
    }
    machine::check_listed(client, joining).await
}

#[cfg(test)]
#[path = "cloud_tests.rs"]
mod tests;
