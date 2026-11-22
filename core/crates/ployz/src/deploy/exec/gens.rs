//! The gen each new copy of a Deploy takes, from the live listings and the entry
//! Machine's store, and what those reads say about the Machines that did not answer.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use ployz_core::{
    ContainerKind, ContainerObservation, ExecutionError, Gen, MachineAction, MachineId,
    QualifiedService, RpcError,
};
use tokio_util::sync::CancellationToken;

use super::machine_ops::{MachineOperations, Unanswered};
use super::publish::gaps_still_routing;
use super::{machine_error, naming_machine};
use crate::deploy::cleanup;
use crate::deploy::progress::Progress;

/// How long a Deploy waits on the entry Machine's store for its gen floor.
pub(super) const STORED_GENS_WAIT: Duration = Duration::from_secs(5);

pub(super) struct Gens<'a> {
    gens: std::sync::Mutex<
        BTreeMap<QualifiedService, std::sync::Arc<tokio::sync::OnceCell<Numbered>>>,
    >,
    stored: tokio::sync::OnceCell<Result<Vec<ContainerObservation>, RpcError>>,
    /// Machines whose Containers the plan's snapshot did not list.
    unobserved: &'a BTreeSet<MachineId>,
    /// The Services this Deploy takes out wherever they run.
    retiring: &'a BTreeSet<QualifiedService>,
}

/// A Service's next gen, and the Machines that left its listing unanswered.
#[derive(Clone)]
struct Numbered {
    r#gen: Gen,
    silent: std::sync::Arc<Vec<Unanswered>>,
}

/// A removal's copies of its Service on the Machines this Deploy did not observe.
pub(super) enum Unreached {
    /// None keeps the route: every Machine was observed, the store lists no In or
    /// Pending copy there, or the Service stays and its other copies are its own.
    None,
    /// The entry Machine's store lists these there, each In or Pending.
    Copies(Vec<cleanup::Copy>),
    /// The store could not be read, so a copy there may keep the route.
    Unknown,
}

impl Unreached {
    /// Whether a proxy may keep routing the Service once its live copy is Out, so
    /// that the drain could never confirm.
    pub(super) fn may_keep_route(&self) -> bool {
        !matches!(self, Self::None)
    }

    pub(super) fn copies(&self) -> &[cleanup::Copy] {
        match self {
            Self::Copies(copies) => copies,
            Self::None | Self::Unknown => &[],
        }
    }
}

impl<'a> Gens<'a> {
    pub(super) fn new(
        unobserved: &'a BTreeSet<MachineId>,
        retiring: &'a BTreeSet<QualifiedService>,
    ) -> Self {
        Self {
            gens: std::sync::Mutex::new(BTreeMap::new()),
            stored: tokio::sync::OnceCell::new(),
            unobserved,
            retiring,
        }
    }

    /// The gen a new copy of `service` on `destination` takes. A Machine that left the
    /// listing unanswered is a gap the Deploy goes on without, never a destination.
    pub(super) async fn gen_for<C: MachineOperations>(
        &self,
        client: &C,
        progress: &Progress,
        service: &QualifiedService,
        destination: &MachineId,
        cancellation: &CancellationToken,
    ) -> Result<Gen, ExecutionError> {
        let numbered = self
            .numbered(client, progress, service, cancellation)
            .await?;
        match numbered
            .silent
            .iter()
            .find(|silent| silent.failure.machine_id == *destination)
        {
            Some(silent) => Err(machine_error(
                MachineAction::ListContainers,
                naming_machine(
                    crate::cluster::unanswered_by(&silent.name, silent.failure.error.clone()),
                    destination,
                ),
            )),
            None => Ok(numbered.r#gen),
        }
    }

    async fn numbered<C: MachineOperations>(
        &self,
        client: &C,
        progress: &Progress,
        service: &QualifiedService,
        cancellation: &CancellationToken,
    ) -> Result<Numbered, ExecutionError> {
        // One cell per Service, so a silent Machine's deadline is paid by each
        // Service at once rather than one Service after another.
        let cell = std::sync::Arc::clone(
            self.gens
                .lock()
                .expect("gens lock")
                .entry(service.clone())
                .or_default(),
        );
        cell.get_or_try_init(|| async {
            let listed = client
                .listed_copies(service)
                .await
                .map_err(|error| machine_error(MachineAction::ListContainers, error))?;
            for silent in &listed.unanswered {
                silent.warn(progress);
            }
            // A silent Machine's copies count only through the store's rows, so a new
            // copy that could not be numbered above them would never replace them.
            let stored = match (
                self.stored(client, cancellation).await?,
                listed.unanswered.first(),
            ) {
                (Ok(stored), _) => stored.as_slice(),
                (Err(_), None) => &[],
                (Err(unread), Some(silent)) => return Err(unfloored(silent, unread)),
            };
            let highest = listed
                .copies
                .iter()
                .filter(|copy| copy.kind == ContainerKind::ServiceContainer)
                .chain(stored.iter().filter(|copy| copy.identity() == *service))
                .map(|copy| copy.copy_gen())
                .max();
            Ok(Numbered {
                r#gen: highest.map_or(Gen::FIRST, |highest| Gen::new(highest.get() + 1)),
                silent: std::sync::Arc::new(listed.unanswered),
            })
        })
        .await
        .cloned()
    }

    /// What a removal knows of `service`'s copies on the Machines this Deploy did
    /// not observe. Every proxy reads the entry Machine's store too, so while a row
    /// there is In or Pending, a copy sent Out beside it stays routed and its drain
    /// never confirms. Only a Service this Deploy removes must reach them. One it
    /// keeps owns its other copies, so they are [`Unreached::None`] to its cleanup.
    pub(super) async fn unreached<C: MachineOperations>(
        &self,
        client: &C,
        service: &QualifiedService,
        cancellation: &CancellationToken,
    ) -> Result<Unreached, ExecutionError> {
        if self.unobserved.is_empty() || !self.retiring.contains(service) {
            return Ok(Unreached::None);
        }
        let Ok(stored) = self.stored(client, cancellation).await? else {
            return Ok(Unreached::Unknown);
        };
        let copies: Vec<_> = stored
            .iter()
            .filter(|copy| {
                copy.identity() == *service
                    && self.unobserved.contains(&copy.machine_id)
                    && copy.rotation().keeps_route()
            })
            .map(cleanup::Copy::observed)
            .collect();
        Ok(if copies.is_empty() {
            Unreached::None
        } else {
            Unreached::Copies(copies)
        })
    }

    /// The Service Containers in the entry Machine's store, read once per Deploy,
    /// or why the read failed or outlasted [`STORED_GENS_WAIT`]. The store keeps the
    /// rows of a Machine that did not answer, and a new copy at or below one of those
    /// gens is refused In or never replaces it. Without the read, gens and removals
    /// follow the live answers.
    async fn stored<C: MachineOperations>(
        &self,
        client: &C,
        cancellation: &CancellationToken,
    ) -> Result<&Result<Vec<ContainerObservation>, RpcError>, ExecutionError> {
        self.stored
            .get_or_try_init(|| async {
                let read = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return Err(ExecutionError::Cancelled),
                    read = tokio::time::timeout(STORED_GENS_WAIT, client.replicated_containers()) => read,
                };
                let containers = match read {
                    Ok(Ok(containers)) => containers,
                    Ok(Err(error)) => {
                        tracing::warn!(
                            code = error.code.as_str(),
                            error = error.message.as_str(),
                            "couldn't read the entry Machine's store; new gens follow the live answers only"
                        );
                        return Ok(Err(error));
                    }
                    Err(_) => {
                        tracing::warn!(
                            wait = ?STORED_GENS_WAIT,
                            "the entry Machine's store didn't answer in time; new gens follow the live answers only"
                        );
                        return Ok(Err(RpcError::unavailable(format!("the store didn't answer in {STORED_GENS_WAIT:?}"))));
                    }
                };
                Ok(Ok(containers
                    .into_iter()
                    .filter(|copy| copy.kind == ContainerKind::ServiceContainer)
                    .collect()))
            })
            .await
    }

    /// The Machines `service`'s listing left unanswered whose proxy acks nothing, so
    /// no publish waits on them. A Suspect Machine still turns Down, and one behind a
    /// path dropping large packets still acks.
    pub(super) fn gaps(&self, service: &QualifiedService) -> BTreeSet<MachineId> {
        let cell = self.gens.lock().expect("gens lock").get(service).cloned();
        cell.and_then(|cell| {
            cell.get().map(|numbered| {
                numbered
                    .silent
                    .iter()
                    .filter(|silent| silent.acks_unwritten)
                    .map(|silent| silent.failure.machine_id)
                    .collect()
            })
        })
        .unwrap_or_default()
    }

    /// The gap Servers, as in [`Self::gaps`], whose proxy may still route `copy`, as
    /// in [`gaps_still_routing`]. None when `copy`'s Service could not be listed, so the
    /// removal goes on as it would without a gap.
    pub(super) async fn gaps_routing<C: MachineOperations>(
        &self,
        client: &C,
        progress: &Progress,
        copy: &cleanup::Copy,
        cancellation: &CancellationToken,
    ) -> Option<cleanup::Unconfirmed> {
        let numbered = self
            .numbered(client, progress, &copy.service, cancellation)
            .await
            .ok()?;
        let gaps = numbered
            .silent
            .iter()
            .filter(|silent| silent.acks_unwritten)
            .map(|silent| silent.failure.machine_id);
        gaps_still_routing(client, copy, gaps).await
    }
}

/// A new copy cannot start while `silent` may run copies the store could not number.
fn unfloored(silent: &Unanswered, unread: &RpcError) -> ExecutionError {
    let error = RpcError::unavailable(format!(
        "Machine '{}' did not answer and the entry Machine's store could not be read, \
             so a new copy might not replace that Machine's copies",
        silent.name
    ))
    .with_causes(
        std::iter::once(unread.message.clone())
            .chain(unread.cause.iter().cloned())
            .collect(),
    );
    machine_error(
        MachineAction::ListContainers,
        naming_machine(error, &silent.failure.machine_id),
    )
}
