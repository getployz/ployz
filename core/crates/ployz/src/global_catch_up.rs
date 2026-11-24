//! Global catch-up: give a joiner the Globals Ployz runs on every Server.
//!
//! It plans from a complete Live Observation, re-observing a partial one a few times
//! within the run before failing, and fails once at the end; nothing retries it in the
//! background. Nothing is copied: the joiner's own daemon builds the Ingress Proxy through
//! SetIngressDoors, once, for the observed proxy id, and the Private Access router through
//! SetPrivateAccess from the sealed auth key an observed router carries. A user Global
//! left from Compose before 0.1.2-beta.42 is only reported, since no Machine admits one.

use ployz_core::error_chain;
use ployz_core::{
    Ciphertext, EnvironmentValues, IngressDoors, IngressDoorsSet, LiveServices, Machine, MachineId,
    PartialResult, PrivateAccessSet, QualifiedService, RpcError, ServiceId, ServiceObservation,
    SetIngressDoorsRequest, SetPrivateAccessRequest,
};
use tokio_util::sync::CancellationToken;

use crate::global_slot::{Live, ServerNames, participating, retire_ingress_proxy};
use crate::ingress::{ClusterDoors, DoorMachine};
use crate::private_access::AccessMachine;
use crate::{
    connect::{Client, unreadable_answer},
    failure::Failure,
};

/// Catch-up failed after membership committed.
#[derive(Debug)]
pub(crate) struct CatchUpError {
    pub(crate) cause: Failure,
    pub(crate) unresolved: Vec<QualifiedService>,
}

impl CatchUpError {
    /// Record the failure and Globals whose eligibility or running slot is unresolved.
    pub(crate) fn new(cause: Failure, unresolved: Vec<QualifiedService>) -> Self {
        Self { cause, unresolved }
    }
}

impl std::fmt::Display for CatchUpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.cause.fmt(formatter)
    }
}

/// What every Server runs, with each Server's name.
pub(crate) struct Observed {
    live: Live,
    /// Omitted Servers that are Unarrived Machines, which hold no copy to wait on.
    unarrived: ServerNames,
    /// The Doors the Cluster declares, as the entry Machine's replica holds them.
    declared: Option<IngressDoors>,
}

pub(crate) trait CatchUpClient {
    async fn live_services(&mut self) -> Result<Observed, Failure>;
    /// Set `machine_id`'s Doors to the Cluster's for the Ingress Proxy `proxy_service`,
    /// or remove its Ingress Proxy when it doesn't hold the ingress role, answering `None`.
    async fn run_doors(
        &mut self,
        machine_id: &MachineId,
        proxy_service: ServiceId,
        doors: ClusterDoors,
        cancel: &CancellationToken,
    ) -> Result<Option<IngressDoorsSet>, RpcError>;
    /// Place `machine_id`'s Private Access router with the auth key `sealed`.
    async fn run_private_access(
        &mut self,
        machine_id: &MachineId,
        sealed: Ciphertext,
    ) -> Result<PrivateAccessSet, RpcError>;
}

impl CatchUpClient for Client {
    async fn live_services(&mut self) -> Result<Observed, Failure> {
        let list = self.machine_list().await.map_err(Failure::from)?;
        let declared = list.enrollment.and_then(|snapshot| snapshot.doors);
        let machines = list.machines;
        let services = self
            .live_services_from(&machines, EnvironmentValues::Included)
            .await
            .map_err(Failure::from)?;
        Ok(Observed {
            live: Live {
                services,
                names: ServerNames::of(&machines),
            },
            unarrived: ServerNames::unarrived(&machines),
            declared,
        })
    }

    async fn run_doors(
        &mut self,
        machine_id: &MachineId,
        proxy_service: ServiceId,
        doors: ClusterDoors,
        cancel: &CancellationToken,
    ) -> Result<Option<IngressDoorsSet>, RpcError> {
        if !participating(self, machine_id).await?.accepts_ingress {
            retire_ingress_proxy(&*self, machine_id, cancel).await?;
            return Ok(None);
        }
        self.set_ingress_doors(
            machine_id,
            SetIngressDoorsRequest {
                doors: doors.doors,
                proxy_service,
                sealed_token: doors.sealed_token,
            },
        )
        .await
        .map(Some)
    }

    async fn run_private_access(
        &mut self,
        machine_id: &MachineId,
        sealed: Ciphertext,
    ) -> Result<PrivateAccessSet, RpcError> {
        self.set_private_access(
            machine_id,
            SetPrivateAccessRequest {
                enabled: true,
                sealed_auth_key: Some(sealed),
                approval: None,
            },
        )
        .await
    }
}

/// A factual stage of target-only catch-up, adapted by the owning command.
pub(crate) enum CatchUpFact {
    Identified(Vec<QualifiedService>),
    Creating(QualifiedService),
    Excluded(QualifiedService),
    Running(QualifiedService),
    Failed(QualifiedService),
    /// Omitted Unarrived Machines the plan did not wait on.
    NotArrived(ServerNames),
    /// Globals of user Services, which the joiner gets no copy of.
    Legacy(Vec<QualifiedService>),
}

const OBSERVATION_ATTEMPTS: u32 = 3;
const OBSERVATION_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

struct Source {
    identity: QualifiedService,
    work: CatchUpWork,
}

/// What catch-up places for one observed Global.
enum CatchUpWork {
    /// The holder, Caddy and Tunnel Connector, which the joiner's daemon builds for this
    /// Ingress Proxy Service with the Cluster's Doors. No door identity is ever copied.
    Ingress(ServiceId, ClusterDoors),
    /// The Private Access router, which the joiner's daemon builds from this sealed auth
    /// key. The router is never copied.
    PrivateAccess(Ciphertext),
}

/// One source for the Ingress Proxy's door identities and one for the Private Access
/// router as its sealed auth key, and the Globals left over from user Services. A joiner
/// with the ingress role gets the Ingress Proxy even when none runs anywhere.
fn sources(
    services: &[ServiceObservation],
    takes_ingress: bool,
    declared: Option<IngressDoors>,
) -> (Vec<Source>, Vec<QualifiedService>) {
    let mut sources = Vec::new();
    // Compose made user Globals until 0.1.2-beta.42. No Machine admits one now, so the
    // joiner gets no copy and its Deploy with a replica count is the way out.
    let legacy = services
        .iter()
        .filter(|service| service.identity.reserved().is_none())
        .filter(|service| service.observed_global_slot_spec().is_some())
        .map(|service| service.identity.clone())
        .collect();
    let doors_run = services
        .iter()
        .any(|service| service.identity.is_ingress_door() && !service.containers.is_empty());
    if doors_run || takes_ingress {
        sources.push(Source {
            identity: QualifiedService::system_ingress(),
            work: CatchUpWork::Ingress(
                crate::ingress::proxy_service(services),
                crate::ingress::cluster_doors(declared, services),
            ),
        });
    }
    if let Some(sealed) = crate::private_access::cluster_sealed_key(services) {
        sources.push(Source {
            identity: QualifiedService::system_tailscale(),
            work: CatchUpWork::PrivateAccess(sealed),
        });
    }
    (sources, legacy)
}

/// What fixes a partial listing. An answer this ployz can't read comes from a newer
/// release, so connectivity is named only for the Servers that didn't answer.
fn partial_next_step<T>(
    result: &PartialResult<T, RpcError>,
    server: impl Fn(&MachineId) -> String,
) -> String {
    let (unreadable, unanswered): (Vec<_>, Vec<_>) = result
        .failures
        .iter()
        .partition(|failure| unreadable_answer(&failure.error));
    let reconnect = "restore peer connectivity and redeploy";
    if unreadable.is_empty() {
        return reconnect.to_owned();
    }
    let names: Vec<_> = unreadable
        .iter()
        .map(|failure| server(&failure.machine_id))
        .collect();
    let update = format!(
        "this ployz can't read {}'s answer, so update ployz on this host",
        names.join(", ")
    );
    if unanswered.is_empty() && result.omissions.is_empty() {
        format!("{update} and redeploy")
    } else {
        format!("{update}; {reconnect}")
    }
}

async fn observe_complete<C: CatchUpClient>(
    client: &mut C,
    this_machine: &MachineId,
    cancel: &CancellationToken,
    observe: &mut impl FnMut(CatchUpFact),
) -> Result<(LiveServices<RpcError>, Option<IngressDoors>), CatchUpError> {
    let cancelled = || CatchUpError::new(Failure::cancelled(), Vec::new());
    for attempt in 1..=OBSERVATION_ATTEMPTS {
        let Observed {
            mut live,
            mut unarrived,
            declared,
        } = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(cancelled()),
            live = client.live_services() => {
                live.map_err(|error| CatchUpError::new(error, Vec::new()))?
            }
        };
        // The joiner's own row may still be its Register row; it is arriving now.
        unarrived.without(this_machine);
        live.services
            .containers
            .omissions
            .retain(|id| !unarrived.contains(id));
        let Some(details) = live.partial() else {
            observe(CatchUpFact::NotArrived(unarrived));
            return Ok((live.services, declared));
        };
        if attempt == OBSERVATION_ATTEMPTS {
            observe(CatchUpFact::NotArrived(unarrived));
            let next = partial_next_step(&live.services.containers, |id| live.names.name(id));
            return Err(CatchUpError::new(
                Failure::unavailable(format!(
                    "Global catch-up cannot plan from partial Service observations: {details}; \
                     {next}"
                )),
                Vec::new(),
            ));
        }
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(cancelled()),
            () = tokio::time::sleep(OBSERVATION_RETRY) => {}
        }
    }
    unreachable!("the last attempt returns")
}

/// What one slot came to.
enum SlotOutcome {
    Resolved,
    /// This Machine takes no part in the slot.
    Excluded,
    Failed(SlotFailure),
}

/// Why a slot is unresolved: each cause, as the sentence a person reads.
struct SlotFailure {
    chains: Vec<String>,
}

impl SlotOutcome {
    fn failed(chain: String) -> Self {
        Self::Failed(SlotFailure {
            chains: vec![chain],
        })
    }
}

/// Every slot's outcome, folded into the facts catch-up reports and what is left to say
/// when it ends.
struct Tally<F> {
    observe: F,
    unresolved: Vec<QualifiedService>,
    failures: Vec<(QualifiedService, String)>,
}

impl<F: FnMut(CatchUpFact)> Tally<F> {
    fn new(identities: Vec<QualifiedService>, mut observe: F) -> Self {
        observe(CatchUpFact::Identified(identities.clone()));
        Self {
            observe,
            unresolved: identities,
            failures: Vec::new(),
        }
    }

    fn creating(&mut self, identity: &QualifiedService) {
        (self.observe)(CatchUpFact::Creating(identity.clone()));
    }

    fn record(&mut self, identity: QualifiedService, outcome: SlotOutcome) {
        let fact = match outcome {
            SlotOutcome::Resolved => {
                self.unresolved.retain(|service| service != &identity);
                CatchUpFact::Running(identity)
            }
            SlotOutcome::Excluded => {
                self.unresolved.retain(|service| service != &identity);
                CatchUpFact::Excluded(identity)
            }
            SlotOutcome::Failed(failure) => {
                for chain in failure.chains {
                    self.failures.push((identity.clone(), chain));
                }
                CatchUpFact::Failed(identity)
            }
        };
        (self.observe)(fact);
    }

    fn finish(self, cancel: &CancellationToken) -> Result<(), CatchUpError> {
        if cancel.is_cancelled() {
            return Err(CatchUpError::new(Failure::cancelled(), self.unresolved));
        }
        if self.unresolved.is_empty() {
            return Ok(());
        }
        let details = self
            .failures
            .iter()
            .map(|(identity, error)| format!("{identity}: {error}"))
            .collect::<Vec<_>>()
            .join("; ");
        let cause = if details.is_empty() {
            Failure::unavailable("eligible Globals are not running after catch-up")
        } else {
            Failure::unavailable(format!("Global catch-up incomplete: {details}"))
        };
        Err(CatchUpError::new(cause, self.unresolved))
    }
}

async fn place_private_access<C: CatchUpClient>(
    client: &mut C,
    this_machine: &Machine,
    sealed: Ciphertext,
) -> SlotOutcome {
    let answer = match AccessMachine::of(this_machine).refusal() {
        Some(refusal) => Err(refusal),
        None => client.run_private_access(&this_machine.id, sealed).await,
    };
    let failure = match answer {
        Ok(set) => crate::private_access::failure_of(&set),
        Err(error) => Some(error_chain::row(&error)),
    };
    failure.map_or(SlotOutcome::Resolved, SlotOutcome::failed)
}

async fn place_ingress<C: CatchUpClient>(
    client: &mut C,
    this_machine: &Machine,
    proxy_service: ServiceId,
    doors: ClusterDoors,
    cancel: &CancellationToken,
) -> SlotOutcome {
    let refusal = this_machine
        .accepts_ingress
        .then(|| DoorMachine::of(this_machine).refusal(&doors.doors))
        .flatten();
    let answer = match refusal {
        Some(refusal) => Err(refusal),
        None => {
            client
                .run_doors(&this_machine.id, proxy_service, doors, cancel)
                .await
        }
    };
    let failure = match answer {
        Ok(None) => return SlotOutcome::Excluded,
        // The daemon answers once both run, so nothing is left to wait on.
        Ok(Some(set)) => crate::ingress::failure_of(&set).map(|failure| failure.cause().to_owned()),
        Err(error) => Some(error_chain::row(&error)),
    };
    failure.map_or(SlotOutcome::Resolved, SlotOutcome::failed)
}

pub(crate) async fn catch_up_globals<C: CatchUpClient>(
    client: &mut C,
    this_machine: &Machine,
    cancel: &CancellationToken,
    mut observe: impl FnMut(CatchUpFact),
) -> Result<(), CatchUpError> {
    let (live, declared) = observe_complete(client, &this_machine.id, cancel, &mut observe).await?;
    let services = live.services();
    let (sources, legacy) = sources(&services, this_machine.accepts_ingress, declared);
    if !legacy.is_empty() {
        observe(CatchUpFact::Legacy(legacy));
    }
    let mut tally = Tally::new(
        sources
            .iter()
            .map(|source| source.identity.clone())
            .collect(),
        observe,
    );
    for source in sources {
        if cancel.is_cancelled() {
            break;
        }
        let identity = source.identity;
        tally.creating(&identity);
        let outcome = match source.work {
            CatchUpWork::PrivateAccess(sealed) => {
                place_private_access(client, this_machine, sealed).await
            }
            CatchUpWork::Ingress(proxy_service, doors) => {
                place_ingress(client, this_machine, proxy_service, doors, cancel).await
            }
        };
        tally.record(identity, outcome);
    }
    tally.finish(cancel)
}

#[cfg(test)]
#[path = "global_catch_up_tests.rs"]
mod tests;
