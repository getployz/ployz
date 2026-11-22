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
//! finishes the removal rather than starting the router again. The router wanted
//! names the state's ID, so a router left from state `off` forgot is never kept.

use std::time::Duration;

use ployz_core::{
    ContainerKind, ContainerObservation, ContainerRuntimeObservation, DoorChange, DoorOutcome,
    Namespace, PrivateAccessFailure, PrivateAccessProblem, PrivateAccessSet, ResolvedServiceSpec,
    Sealing, SetPrivateAccessRequest,
    authority::{AuthorityRefusal, PrivateAccessFacts, Purpose, Timestamp, private_access},
    private_access_router_joined, private_access_sealed_key,
};
use zeroize::Zeroizing;

use super::ingress_doors::reserved::{self, Retiree, Step, newest, retirees, usable};
use super::ingress_doors::{Effects, Gate};
use super::{Error, LocalMachine, Scope};
use crate::docker::{ContainerRuntime, same_creation};
use crate::images::private_access_router;

/// The waits of the Private Access route. `Default` is production's.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Pacing {
    /// How long a placed router has to join the tailnet before the route stops watching
    /// it. Tailscale takes about 30 seconds to refuse a key, so a router that dies or is
    /// refused in this time is reported, and a joined router ends the wait at once.
    pub(crate) router_settle: Duration,
    /// How often a router that has not joined is looked at again.
    pub(crate) router_poll: Duration,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            router_settle: Duration::from_secs(45),
            router_poll: Duration::from_secs(1),
        }
    }
}
/// How far back the log of a router that already runs is read.
const RUNNING_LOOKBACK_SECONDS: i32 = 120;
/// What containerboot prints once the node is up.
const ROUTER_UP: &str = "Startup complete";
/// The longest detail an answer carries.
const DETAIL_LIMIT: usize = 300;

/// The whole change, decided before any effect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RouterPlan {
    /// Routers that are not the canonical one, or every one when Private Access
    /// turns off.
    pub(super) retire: Vec<Retiree>,
    /// `None` when Private Access turns off.
    pub(super) step: Option<Step>,
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
    let kept = newest(routers.iter().filter(|router| {
        usable(*router)
            && same_creation(
                router,
                &Namespace::system(),
                ContainerKind::ServiceContainer,
                spec,
            )
    }));
    RouterPlan {
        retire: retirees(routers, kept.map(|router| router.container_id)),
        step: Some(reserved::step(kept, routers)),
    }
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

/// Whether the router has joined the tailnet: it said so and its probe agrees. The
/// start-up line comes before the first probe passes, and a success answered between
/// the two would read as stopped to the next look.
fn has_joined(output: &str, runtime: &ContainerRuntimeObservation) -> bool {
    output.contains(ROUTER_UP) && private_access_router_joined(runtime)
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
    /// An Operator's `SetPrivateAccess` turns Private Access on only with an approval, by a
    /// trusted Device or a trusted passkey's own prompt, of the sealed auth key it carries.
    /// The key decides which tailnet reaches the whole Cluster network. Turning off needs
    /// none, and neither does a Full caller.
    ///
    /// # Errors
    /// Returns [`Error::PrivateAccessRefused`] naming what the approval lacks.
    fn admit_operator_private_access(
        &self,
        request: &SetPrivateAccessRequest,
    ) -> Result<(), Error> {
        let record = self.record();
        if !request.enabled {
            return Ok(());
        }
        let approval = request
            .approval
            .as_ref()
            .ok_or(Error::PrivateAccessRefused(AuthorityRefusal::Unsigned {
                purpose: Purpose::Workflow,
            }))?;
        let sealed = request
            .sealed_auth_key
            .as_ref()
            .ok_or(Error::PrivateAccessKeyMissing)?;
        let cluster = self
            .held_sealing(record.phase())
            .and_then(Sealing::held_key);
        let approver = private_access::verify(
            approval,
            &PrivateAccessFacts {
                cluster,
                trusted: &record.trusted_keys(),
                sealed,
                now: Timestamp::now(),
            },
        )
        .map_err(Error::PrivateAccessRefused)?;
        tracing::info!(%approver, "Private Access approved");
        Ok(())
    }

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

    /// Place this Machine's Private Access router, or remove it. An Operator's turn on
    /// needs an approval, as [`Self::admit_operator_private_access`] decides.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PrivateAccessRefused`] for an Operator's turn on without an
    /// approval, [`Error::PrivateAccessKeyMissing`] and [`Error::PrivateAccessKey`] for
    /// a request to turn on without an auth key this Cluster opens,
    /// [`Error::PrivateAccessRouterStays`] when a router turned off could not be
    /// removed, and the errors of a Docker stage that fails. A router that was placed
    /// and did not serve is answered in the result's `failure` instead.
    pub(crate) async fn set_private_access(
        &self,
        request: SetPrivateAccessRequest,
    ) -> Result<PrivateAccessSet, Error> {
        if let Scope::Operator = self.scope() {
            self.admit_operator_private_access(&request)?;
        }
        let containers = self.docker()?.clone();
        let opened = if request.enabled {
            let sealed = request
                .sealed_auth_key
                .ok_or(Error::PrivateAccessKeyMissing)?;
            let key = containers
                .open_private_access_key(&sealed)
                .map_err(Error::PrivateAccessKey)?;
            Some((sealed, key, self.cluster_network().await?))
        } else {
            None
        };
        self.reserved_effect(Gate::Router)?;
        self.finish_installed(move |local| async move {
            let _doors = containers.doors().hold().await;
            local.reserved_effect(Gate::Router)?;
            let machine_id = local.record().id();
            let routers = containers.routers(&machine_id).await?;
            let wanted = match opened {
                Some((sealed, key, network)) => {
                    let state = containers
                        .machine_files()
                        .keep_private_access_state()
                        .map_err(Error::PrivateAccessState)?;
                    let spec = private_access_router(network, &sealed, state);
                    containers
                        .doors()
                        .prepare_pinned_image(&spec.container.image)
                        .await?;
                    containers
                        .machine_files()
                        .write_private_access_key(&key)
                        .map_err(Error::PrivateAccessKeyFile)?;
                    Some(Wanted { spec, key })
                }
                None => None,
            };
            let _turn = local.exclusive_turn().await;
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
    /// and could not be removed then, so it is removed now and never started. A newest
    /// router that names other state than the state on disk mounts state `off` forgot,
    /// so it is replaced by one built on the state there now.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PrivateAccessKey`] when the router's key no longer opens,
    /// [`Error::PrivateAccessRouterStays`] when a router turned off still cannot be
    /// removed, and the errors of the Docker reads and the key write.
    pub(crate) async fn restore_private_access(&self) -> Result<Option<PrivateAccessSet>, Error> {
        let containers = self.docker()?.clone();
        self.reserved_effect(Gate::Router)?;
        self.finish_installed(move |local| async move {
            let _doors = containers.doors().hold().await;
            local.reserved_effect(Gate::Router)?;
            let routers = containers.routers(&local.record().id()).await?;
            if routers.is_empty() {
                return Ok(None);
            }
            if !containers.machine_files().private_access_state_kept() {
                tracing::info!(
                    "a Private Access router turned off is still here, so it is removed, not started"
                );
                let _turn = local.exclusive_turn().await;
                let plan = plan_router(&routers, None);
                return local
                    .converge_router(&containers, &plan, None, Watch::Skip)
                    .await
                    .map(Some);
            }
            let Some((spec, sealed)) = routers
                .iter()
                .rev()
                .filter(|router| usable(*router))
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
            let state = containers
                .machine_files().keep_private_access_state()
                .map_err(Error::PrivateAccessState)?;
            let pinned = private_access_router(local.cluster_network().await?, &sealed, state);
            if spec != pinned {
                tracing::warn!(
                    "the Private Access router here was created from a spec this daemon doesn't build; it is replaced with the pinned one, not started"
                );
            }
            containers
                .doors().prepare_pinned_image(&pinned.container.image)
                .await?;
            let spec = pinned;
            containers
                .machine_files().write_private_access_key(&key)
                .map_err(Error::PrivateAccessKeyFile)?;
            let wanted = Wanted { spec, key };
            let _turn = local.exclusive_turn().await;
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
    /// wanted, the node state goes before any router stops and the key before any
    /// router is removed, and a router that
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
        if wanted.is_none() {
            containers
                .machine_files()
                .forget_private_access_state()
                .map_err(Error::PrivateAccessState)?;
        }
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
                .machine_files()
                .remove_private_access_key()
                .map_err(Error::PrivateAccessKeyFile)?;
        }
        let mut stays = None;
        for retiree in &plan.retire {
            match containers.lifecycle().remove(&retiree.id, true, true).await {
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
            match containers.lifecycle().stop(&retiree.id, None, None).await {
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
            Step::Keep(id) => (*id, DoorChange::Unchanged),
            Step::Start(id) => {
                effects.started.push(*id);
                self.start_door(containers, id, Gate::Router).await?;
                (*id, DoorChange::Started)
            }
            Step::Create(replacing) => {
                let id = self
                    .create_door(&wanted.spec, *replacing, effects, Gate::Router)
                    .await?;
                self.start_door(containers, &id, Gate::Router).await?;
                (id, DoorChange::Created)
            }
        };
        let now = containers
            .observation()
            .inspect_managed(&id, &self.record().id())
            .await?;
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
        let deadline = tokio::time::Instant::now() + self.pacing.router_settle;
        loop {
            let runtime = containers
                .observation()
                .inspect_managed(&id, &machine_id)
                .await
                .map(|observed| observed.runtime.clone())
                .ok()?;
            let output = containers.router_output(&id, since).await;
            let alive = runtime.is_running();
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
            if !settles || has_joined(&output, &runtime) || tokio::time::Instant::now() >= deadline
            {
                return None;
            }
            tokio::time::sleep(self.pacing.router_poll).await;
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
    use ployz_core::{ContainerId, HealthObservation};
    use serde_json::json;

    #[test]
    fn the_router_gets_forty_five_seconds_to_join_and_is_looked_at_each_second() {
        let pacing = Pacing::default();
        assert_eq!(pacing.router_settle, Duration::from_secs(45));
        assert_eq!(pacing.router_poll, Duration::from_secs(1));
    }

    fn id(id: char) -> ContainerId {
        ContainerId::parse(id.to_string().repeat(64)).unwrap()
    }

    fn wanted() -> ResolvedServiceSpec {
        private_access_router(
            "10.210.0.0/16".parse().unwrap(),
            &ployz_core::Ciphertext::from_bytes(vec![9; 48]),
            ployz_core::PrivateAccessStateId::random(),
        )
    }

    fn router(
        id_char: char,
        spec: &ResolvedServiceSpec,
        created_at: i64,
        runtime: serde_json::Value,
    ) -> ContainerObservation {
        serde_json::from_value(json!({
            "container_id": id_char.to_string().repeat(64),
            "display_name": format!("{}-{id_char}", spec.name),
            "created_at_unix_nanos": created_at,
            "machine_id": "b".repeat(32),
            "namespace": "ployz-system",
            "kind": "service_container",
            "runtime": runtime,
            "resolved_spec": serde_json::to_value(spec).unwrap(),
        }))
        .unwrap()
    }

    fn running() -> serde_json::Value {
        json!({ "state": "running", "health": "healthy" })
    }

    fn created() -> serde_json::Value {
        json!({ "state": "created" })
    }

    fn retiree(id_char: char, running: bool) -> Retiree {
        Retiree {
            id: id(id_char),
            running,
        }
    }

    // Catches the router placer choosing differently from the door placers it copies:
    // the newest running canonical router is kept, a stopped or restarting one is
    // started or left to restart, a dead one is replaced, and every other router retires.
    #[test]
    fn the_router_plan_keeps_starts_or_creates_and_retires_the_rest() {
        let spec = wanted();
        let plan = plan_router(&[], Some(&spec));
        assert_eq!(plan.step, Some(Step::Create(None)));
        assert!(plan.retire.is_empty());

        let plan = plan_router(&[router('a', &spec, 1, created())], Some(&spec));
        assert_eq!(plan.step, Some(Step::Start(id('a'))));

        let restarting = router('a', &spec, 1, json!({ "state": "restarting" }));
        let plan = plan_router(&[restarting], Some(&spec));
        assert_eq!(plan.step, Some(Step::Keep(id('a'))));

        let mut other = spec.clone();
        other.container.image = "tailscale/tailscale:old".into();
        let routers = [
            router('a', &spec, 1, running()),
            router('b', &spec, 2, created()),
            router('c', &other, 4, running()),
            router('d', &spec, 3, json!({ "state": "dead" })),
        ];
        let plan = plan_router(&routers, Some(&spec));
        assert_eq!(plan.step, Some(Step::Keep(id('a'))));
        assert_eq!(
            plan.retire,
            vec![retiree('b', false), retiree('c', true), retiree('d', false)]
        );

        let routers = [
            router('a', &spec, 1, json!({ "state": "dead" })),
            router('c', &other, 4, running()),
            router('d', &other, 3, created()),
        ];
        let plan = plan_router(&routers, Some(&spec));
        assert_eq!(plan.step, Some(Step::Create(Some(id('c')))));
        assert_eq!(
            plan.retire,
            vec![retiree('a', false), retiree('c', true), retiree('d', false)]
        );

        let plan = plan_router(&routers, None);
        assert_eq!(plan.step, None);
        assert_eq!(plan.retire.len(), 3);
    }

    // Catches a placement answering success on the start-up line before the probe
    // passes, which reads as stopped to the next look.
    #[test]
    fn a_router_has_joined_only_once_it_said_so_and_its_probe_passes() {
        let running = |health| ContainerRuntimeObservation::Running { health };
        assert!(!has_joined(
            "Startup complete",
            &running(HealthObservation::Starting)
        ));
        assert!(!has_joined(
            "Startup complete",
            &running(HealthObservation::Unhealthy)
        ));
        assert!(!has_joined("booting", &running(HealthObservation::Healthy)));
        assert!(has_joined(
            "Startup complete",
            &running(HealthObservation::Healthy)
        ));
        assert!(has_joined(
            "Startup complete",
            &running(HealthObservation::NotConfigured)
        ));
    }

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
