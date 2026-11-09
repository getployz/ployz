//! Placing this Machine's Private Access router: the Tailscale container that
//! advertises the Cluster network to a tailnet.
//!
//! The router joins no holder and needs no Machine role, so every Machine runs one.
//! Like the Ingress doors, a router the route replaces is stopped first and removed
//! only once the new one runs, so a replacement that fails to start leaves the old one
//! to start again. A router that starts and then cannot join the tailnet stays in
//! place, and the answer says why, so one corrected key replaces it.
//!
//! The node state on the Machine says whether a router is wanted here. `on` keeps it
//! before placing the router, and `off` forgets it, with the key, before removing the
//! routers, so a removal that fails is answered as a failure and the next restore
//! finishes the removal rather than starting the router again.

use std::time::Duration;

use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, ContainerRuntimeObservation, DoorChange,
    DoorOutcome, Namespace, PrivateAccessFailure, PrivateAccessProblem, PrivateAccessSet,
    ResolvedServiceSpec, SetPrivateAccessRequest, private_access_sealed_key,
};
use zeroize::Zeroizing;

use super::ingress_doors::{Effects, Gate, Replacing, Retiree};
use super::{Error, LocalMachine};
use crate::docker::{ContainerRuntime, same_creation};
use crate::images::private_access_router;

/// How long a placed router has to join the tailnet before the route stops watching
/// it. Tailscale takes about 30 seconds to refuse a key, so a router that dies or is
/// refused in this time is reported, and a joined router ends the wait at once.
#[cfg(not(test))]
const ROUTER_SETTLE: Duration = Duration::from_secs(45);
#[cfg(test)]
const ROUTER_SETTLE: Duration = Duration::from_millis(300);
#[cfg(not(test))]
const ROUTER_POLL: Duration = Duration::from_secs(1);
#[cfg(test)]
const ROUTER_POLL: Duration = Duration::from_millis(200);
/// How far back the log of a router that already runs is read.
const RUNNING_LOOKBACK_SECONDS: i32 = 120;
/// What containerboot prints once the node is up.
const ROUTER_UP: &str = "Startup complete";
/// The longest detail an answer carries.
const DETAIL_LIMIT: usize = 300;

/// What the route does with the router.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum RouterStep {
    /// A canonical router runs.
    Keep(ContainerId),
    /// A canonical router exists at rest.
    Start(ContainerId),
    Create(Replacing),
}

/// The whole change, decided before any effect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RouterPlan {
    /// Routers that are not the canonical one, or every one when Private Access
    /// turns off.
    pub(super) retire: Vec<Retiree>,
    /// `None` when Private Access turns off.
    pub(super) step: Option<RouterStep>,
}

fn usable(router: &ContainerObservation) -> bool {
    !matches!(
        router.runtime,
        ContainerRuntimeObservation::Removing | ContainerRuntimeObservation::Dead
    )
}

fn running(router: &ContainerObservation) -> bool {
    matches!(router.runtime, ContainerRuntimeObservation::Running { .. })
}

/// Decide the change from the routers Docker has and the router wanted, if any.
pub(super) fn plan_router(
    routers: &[ContainerObservation],
    wanted: Option<&ResolvedServiceSpec>,
) -> RouterPlan {
    let Some(spec) = wanted else {
        return RouterPlan {
            retire: retirees(routers, None),
            step: None,
        };
    };
    let kept = routers
        .iter()
        .filter(|router| {
            usable(router)
                && same_creation(
                    router,
                    &Namespace::system(),
                    ContainerKind::ServiceContainer,
                    spec,
                )
        })
        .max_by_key(|router| {
            (
                running(router),
                router.created_at_unix_nanos,
                router.container_id,
            )
        });
    let kept_id = kept.map(|router| router.container_id);
    let step = match kept {
        Some(router)
            if running(router) || router.runtime == ContainerRuntimeObservation::Restarting =>
        {
            RouterStep::Keep(router.container_id)
        }
        Some(router) => RouterStep::Start(router.container_id),
        None => RouterStep::Create(
            routers
                .iter()
                .map(|router| (router.created_at_unix_nanos, router.container_id))
                .max()
                .map(|(_, id)| id),
        ),
    };
    RouterPlan {
        retire: retirees(routers, kept_id),
        step: Some(step),
    }
}

fn retirees(routers: &[ContainerObservation], kept: Option<ContainerId>) -> Vec<Retiree> {
    routers
        .iter()
        .filter(|router| Some(router.container_id) != kept)
        .map(|router| Retiree {
            id: router.container_id,
            running: !crate::ingress::at_rest(&router.runtime),
        })
        .collect()
}

/// Why a router that has stopped or is still starting did not serve, from what it last
/// said. Tailscale words a refused key as `invalid key`.
fn classify(output: &str) -> Option<PrivateAccessProblem> {
    output
        .lines()
        .any(refusal)
        .then_some(PrivateAccessProblem::KeyRefused)
}

fn refusal(line: &str) -> bool {
    let lowered = line.to_ascii_lowercase();
    ["invalid key", "key expired", "key revoked"]
        .iter()
        .any(|refusal| lowered.contains(refusal))
}

/// The last line of `output` that refuses the key, or else the last that says
/// something, with `key` kept out of it.
fn detail(output: &str, key: &[u8]) -> String {
    let lines = || output.lines().rev().map(str::trim);
    let line = lines()
        .find(|line| refusal(line))
        .or_else(|| lines().find(|line| !line.is_empty()))
        .unwrap_or_default();
    let key = String::from_utf8_lossy(key);
    let mut line = if key.is_empty() {
        line.to_owned()
    } else {
        line.replace(key.as_ref(), "[key]")
    };
    if line.len() > DETAIL_LIMIT {
        let end = (0..=DETAIL_LIMIT)
            .rev()
            .find(|end| line.is_char_boundary(*end))
            .unwrap_or(0);
        line.truncate(end);
    }
    line
}

/// Whether a placement waits to see the router join the tailnet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Watch {
    /// A request is waiting for the answer.
    Settle,
    /// Nobody is waiting, so the admission turn is not held for it.
    Skip,
}

/// The router to place and the key it reads.
pub(super) struct Wanted {
    pub(super) spec: ResolvedServiceSpec,
    pub(super) key: Zeroizing<Vec<u8>>,
}

impl LocalMachine {
    /// The Cluster network, from this Machine's record or, once joined, from the
    /// replicated Cluster record.
    ///
    /// # Errors
    ///
    /// Returns when the Cluster has not published its network to this Machine.
    pub(crate) async fn cluster_network(&self) -> Result<ipnet::Ipv4Net, Error> {
        let record = self.record();
        if let Some(network) = record.cluster_network() {
            return Ok(network);
        }
        super::published_network(&record, self.replicated()?).await
    }

    /// Place this Machine's Private Access router, or remove it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PrivateAccessKeyMissing`] and [`Error::PrivateAccessKey`] for
    /// a request to turn on without an auth key this Cluster opens,
    /// [`Error::PrivateAccessRouterStays`] when a router turned off could not be
    /// removed, and the errors of a Docker stage that fails. A router that was placed
    /// and did not serve is answered in the result's `failure` instead.
    pub(crate) async fn set_private_access(
        &self,
        request: SetPrivateAccessRequest,
    ) -> Result<PrivateAccessSet, Error> {
        let containers = self.containers.clone().ok_or(Error::DockerUnavailable)?;
        let wanted = if request.enabled {
            let sealed = request
                .sealed_auth_key
                .ok_or(Error::PrivateAccessKeyMissing)?;
            let key = containers
                .open_private_access_key(&sealed)
                .map_err(Error::PrivateAccessKey)?;
            let network = self.cluster_network().await?;
            Some(Wanted {
                spec: private_access_router(network, &sealed),
                key,
            })
        } else {
            None
        };
        self.reserved_effect(Gate::Router)?;
        let local = self.clone();
        self.finish_installed(move |_installation| async move {
            let _doors = containers.door_lifecycle().await;
            local.reserved_effect(Gate::Router)?;
            let machine_id = local.record().id();
            let routers = containers.routers(&machine_id).await?;
            if let Some(wanted) = &wanted {
                containers
                    .prepare_pinned_image(&wanted.spec.container.image)
                    .await?;
                containers
                    .write_private_access_key(&wanted.key)
                    .map_err(Error::PrivateAccessKeyFile)?;
                containers
                    .keep_private_access_state()
                    .map_err(Error::PrivateAccessState)?;
            }
            let _turn = local.owner.admission_lock().write_owned().await;
            let plan = plan_router(&routers, wanted.as_ref().map(|wanted| &wanted.spec));
            local
                .converge_router(&containers, &plan, wanted.as_ref(), Watch::Settle)
                .await
        })
        .await
    }

    /// Place the router this Machine last held again, after a reboot or a restart.
    ///
    /// A reboot clears the opened auth key, and Docker starts the router before the
    /// daemon is up to write it again, so its start fails and Docker never retries.
    /// This opens the key sealed on the newest router, writes it, then starts that
    /// router if it is at rest. A router that runs stays as it is, so a daemon restart
    /// changes nothing, and a Machine with no router places none. The router keeps the
    /// spec it was created from, so a new image pin replaces nothing here, and nobody
    /// waits for it to join the tailnet.
    ///
    /// A router Docker still has on a Machine whose node state is gone was turned off
    /// and could not be removed then, so it is removed now and never started.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PrivateAccessKey`] when the router's key no longer opens,
    /// [`Error::PrivateAccessRouterStays`] when a router turned off still cannot be
    /// removed, and the errors of the Docker reads and the key write.
    pub(crate) async fn restore_private_access(&self) -> Result<Option<PrivateAccessSet>, Error> {
        let containers = self.containers.clone().ok_or(Error::DockerUnavailable)?;
        self.reserved_effect(Gate::Router)?;
        let local = self.clone();
        self.finish_installed(move |_installation| async move {
            let _doors = containers.door_lifecycle().await;
            local.reserved_effect(Gate::Router)?;
            let routers = containers.routers(&local.record().id()).await?;
            if routers.is_empty() {
                return Ok(None);
            }
            if !containers.private_access_state_kept() {
                tracing::info!(
                    "a Private Access router turned off is still here, so it is removed, not started"
                );
                let _turn = local.owner.admission_lock().write_owned().await;
                let plan = plan_router(&routers, None);
                return local
                    .converge_router(&containers, &plan, None, Watch::Skip)
                    .await
                    .map(Some);
            }
            let Some((spec, sealed)) = routers
                .iter()
                .rev()
                .filter(|router| usable(router))
                .find_map(|router| {
                    let sealed = private_access_sealed_key(&router.resolved_spec.container.labels)?;
                    Some((router.resolved_spec.clone(), sealed))
                })
            else {
                return Ok(None);
            };
            let key = containers
                .open_private_access_key(&sealed)
                .map_err(Error::PrivateAccessKey)?;
            containers
                .write_private_access_key(&key)
                .map_err(Error::PrivateAccessKeyFile)?;
            let wanted = Wanted { spec, key };
            let _turn = local.owner.admission_lock().write_owned().await;
            let plan = plan_router(&routers, Some(&wanted.spec));
            local
                .converge_router(&containers, &plan, Some(&wanted), Watch::Skip)
                .await
                .map(Some)
        })
        .await
    }

    /// Carry out the plan: stop what it retires, place the router, watch it join the
    /// tailnet, then remove what it retired. A router that cannot be created or
    /// started undoes the call, so the previous router starts again. With no router
    /// wanted, the key and the node state go before the routers do, and a router that
    /// stays is an error, so the caller knows this Machine is not off yet. A replaced
    /// router that stays is only logged: stopped, it serves nothing, and the next call
    /// retires it.
    async fn converge_router(
        &self,
        containers: &ContainerRuntime,
        plan: &RouterPlan,
        wanted: Option<&Wanted>,
        watch: Watch,
    ) -> Result<PrivateAccessSet, Error> {
        let mut set = PrivateAccessSet::default();
        let mut effects = Effects::default();
        let placed = self
            .place_router(containers, plan, wanted, &mut effects)
            .await;
        let router = match placed {
            Ok(router) => router,
            Err(error) => return Err(self.undo(containers, &effects, error).await),
        };
        if let (Some(router), Some(wanted), Watch::Settle) = (&router, wanted, watch) {
            set.failure = self.watch_router(containers, router, &wanted.key).await;
        }
        set.router = router;
        if wanted.is_none() {
            containers
                .remove_private_access_key()
                .map_err(Error::PrivateAccessKeyFile)?;
            containers
                .forget_private_access_state()
                .map_err(Error::PrivateAccessState)?;
        }
        let mut stays = None;
        for retiree in &plan.retire {
            match containers.remove(&retiree.id, true, true).await {
                Ok(()) => set.retired.push(retiree.id),
                Err(crate::docker::Error::ContainerNotFound(_)) => {}
                Err(source) if wanted.is_none() => {
                    stays.get_or_insert(Error::PrivateAccessRouterStays {
                        container_id: retiree.id,
                        source: Box::new(source),
                    });
                }
                Err(error) => tracing::warn!(
                    container = %retiree.id,
                    error = %ployz_core::error_chain::inline(&error),
                    "could not remove a replaced Private Access router"
                ),
            }
        }
        stays.map_or(Ok(set), Err)
    }

    async fn place_router(
        &self,
        containers: &ContainerRuntime,
        plan: &RouterPlan,
        wanted: Option<&Wanted>,
        effects: &mut Effects,
    ) -> Result<Option<DoorOutcome>, Error> {
        for retiree in &plan.retire {
            self.reserved_effect(Gate::Router)?;
            match containers.stop(&retiree.id, None, None).await {
                Ok(()) => {
                    if retiree.running {
                        effects.stopped.push(retiree.id);
                    }
                }
                Err(crate::docker::Error::ContainerNotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let (Some(step), Some(wanted)) = (&plan.step, wanted) else {
            return Ok(None);
        };
        let (id, change) = match step {
            RouterStep::Keep(id) => (*id, DoorChange::Unchanged),
            RouterStep::Start(id) => {
                effects.started.push(*id);
                self.start_door(containers, id, Gate::Router).await?;
                (*id, DoorChange::Started)
            }
            RouterStep::Create(replacing) => {
                let id = self
                    .create_door(&wanted.spec, *replacing, effects, Gate::Router)
                    .await?;
                self.start_door(containers, &id, Gate::Router).await?;
                (id, DoorChange::Created)
            }
        };
        let now = containers.inspect_managed(&id, &self.record().id()).await?;
        Ok(Some(DoorOutcome { now, change }))
    }

    /// Watch a placed router until it has joined the tailnet, has died, or the settle
    /// time ends with it running and saying nothing wrong. A router that already ran is
    /// read once, from the last two minutes, and not waited on.
    async fn watch_router(
        &self,
        containers: &ContainerRuntime,
        router: &DoorOutcome,
        key: &[u8],
    ) -> Option<PrivateAccessFailure> {
        let id = router.now.container_id;
        let machine_id = self.record().id();
        let settles = router.change != DoorChange::Unchanged;
        let lookback = if settles { 1 } else { RUNNING_LOOKBACK_SECONDS };
        let since = unix_seconds().saturating_sub(lookback);
        let deadline = tokio::time::Instant::now() + ROUTER_SETTLE;
        loop {
            let runtime = containers
                .inspect_managed(&id, &machine_id)
                .await
                .map(|observed| observed.runtime.clone())
                .ok()?;
            let output = containers.router_output(&id, since).await;
            let alive = matches!(runtime, ContainerRuntimeObservation::Running { .. });
            if let Some(problem) = classify(&output) {
                return Some(PrivateAccessFailure {
                    problem,
                    detail: detail(&output, key),
                });
            }
            if !alive {
                return Some(PrivateAccessFailure {
                    problem: PrivateAccessProblem::NotRunning,
                    detail: format!("the router is {runtime}: {}", detail(&output, key)),
                });
            }
            if !settles || output.contains(ROUTER_UP) || tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(ROUTER_POLL).await;
        }
    }
}

fn unix_seconds() -> i32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i32::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_key_is_told_from_a_router_that_merely_stopped() {
        assert_eq!(
            classify("backend error: invalid key: unable to validate API key"),
            Some(PrivateAccessProblem::KeyRefused)
        );
        assert_eq!(classify("tailscaled: shutting down"), None);
        assert_eq!(
            classify("boot: Running 'tailscale up' --auth-key=file:/run/ployz-auth/auth-key"),
            None
        );
        assert_eq!(classify(""), None);
    }

    #[test]
    fn the_detail_is_one_short_line_without_the_key() {
        let output = "boot: starting\nfailed with tskey-auth-abc\n\n";
        assert_eq!(detail(output, b"tskey-auth-abc"), "failed with [key]");
        assert_eq!(detail(&"x".repeat(900), b"").len(), DETAIL_LIMIT);
        assert_eq!(detail("", b"k"), "");
        let looping =
            "backend error: invalid key: unable to validate API key\nboot: Starting tailscaled\n";
        assert_eq!(
            detail(looping, b"tskey-auth-abc"),
            "backend error: invalid key: unable to validate API key"
        );
    }
}
