//! Repair of the Ingress doors a scan finds stranded, and the holds that keep a requested
//! stop from being undone by it. The Ingress doors are the only Service Containers the
//! daemon starts unasked: it starts an existing holder or Caddy, or restarts a Caddy or
//! connector left in its owner's old network, and never creates, replaces, moves or
//! removes one. Docker's own restart policy covers a door that ran and died; this covers
//! the ones it does not.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use ployz_core::ContainerId;

use super::{
    Error, Lifecycle,
    doors::{self, Door, DoorFacts},
    lifecycle::{failed_start, idempotent_lifecycle_result, is_running},
};

/// Who may start which door, and who has asked for one to stay stopped.
#[derive(Clone, Default)]
pub(super) struct IngressRecovery {
    stranded: Arc<tokio::sync::Mutex<Stranded>>,
}

#[derive(Default)]
struct Stranded {
    /// Stopped on request, so never started unasked. Held in daemon memory, so a
    /// daemon restart forgets it.
    held: Holds,
    /// The last one whose failure was logged. Docker names a fresh endpoint in
    /// every failure, so comparing the messages would log every scan.
    reported: Option<ContainerId>,
    /// Taken through each start, so a stop that races it lands after it and holds,
    /// while other containers never wait on the Docker calls.
    gates: BTreeMap<ContainerId, Arc<tokio::sync::Mutex<()>>>,
}

/// What a stop of a Container owes the recovery: the hold it keeps once the stop goes
/// ahead. Dropped, it lifts the hold.
#[must_use = "the hold lifts when the plan drops unless the stop proceeds"]
pub(super) struct StopPlan(Held);

impl StopPlan {
    /// The stop goes ahead: the Container stays stopped until a start releases it.
    pub(super) fn proceed(self) {
        self.0.keep();
    }
}

impl IngressRecovery {
    async fn stranded(&self) -> tokio::sync::MutexGuard<'_, Stranded> {
        self.stranded.lock().await
    }

    /// Called before a stop of `container_id`: holds it against recovery while the stop
    /// decides, and waits for a recovery start of it already under way.
    pub(super) async fn before_stop(&self, container_id: &ContainerId) -> StopPlan {
        let (hold, gate) = {
            let stranded = self.stranded().await;
            let hold = stranded.held.hold(*container_id);
            (hold, stranded.gates.get(container_id).cloned())
        };
        if let Some(gate) = gate {
            drop(gate.lock().await);
        }
        StopPlan(hold)
    }

    /// A requested start lifts a stop's hold.
    pub(super) async fn release(&self, container_id: &ContainerId) {
        self.stranded().await.held.release(container_id);
    }

    /// Drops what recovery remembers about a container Docker no longer has.
    pub(super) async fn forget(&self, container_id: &ContainerId) {
        let mut stranded = self.stranded().await;
        stranded.held.forget(container_id);
        stranded.gates.remove(container_id);
    }

    #[cfg(test)]
    pub(super) async fn is_held(&self, container_id: &ContainerId) -> bool {
        self.stranded().await.held.contains(container_id)
    }

    /// Carry out one repair, unless a request stopped its Container or is about to, or a
    /// container was created since `creates` was read or is being created. Recovery only
    /// starts what exists; it never creates.
    async fn run(&self, runtime: &Lifecycle, repair: Repair, creates: u64) {
        // Under the doors lock so a route placing the doors never races it.
        let _doors = runtime.doors.hold().await;
        let container_id = *repair.target();
        let gate = self
            .stranded()
            .await
            .gates
            .entry(container_id)
            .or_default()
            .clone();
        let Ok(_gate) = gate.try_lock() else {
            return;
        };
        if self.stranded().await.held.contains(&container_id)
            || runtime.standing.is_withdrawn(&container_id)
        {
            return;
        }
        let result = repair_door(runtime, &repair, creates).await;
        let mut stranded = self.stranded().await;
        match result {
            Ok(acted) => {
                if acted {
                    tracing::info!(?repair, "{} an Ingress door", repair.verb());
                }
                stranded.reported = None;
            }
            Err(error) if stranded.reported != Some(container_id) => {
                eprintln!(
                    "Ingress door {container_id} is still not {}: {error}",
                    repair.verb(),
                    error = ployz_core::error_chain::inline(&error),
                );
                stranded.reported = Some(container_id);
            }
            Err(_) => {}
        }
    }
}

impl Lifecycle {
    /// Carry out one repair a scan decided on. See [`IngressRecovery::run`].
    pub(super) async fn recover(&self, repair: Repair, creates: u64) {
        self.recovery.run(self, repair, creates).await;
    }
}

async fn repair_door(runtime: &Lifecycle, repair: &Repair, creates: u64) -> Result<bool, Error> {
    match repair {
        Repair::StartHolder(container_id)
        | Repair::StartCaddy {
            holder: None,
            caddy: container_id,
        } => {
            let inspected = runtime.observation.ensure_managed(container_id).await?;
            // A newer door may be about to take the ports; the next scan decides.
            if !failed_start(&inspected) || !runtime.docker.created_nothing_since(creates) {
                return Ok(false);
            }
            runtime.start_inspected(container_id, &inspected).await?;
            Ok(true)
        }
        Repair::StartCaddy {
            holder: Some(holder),
            caddy,
        } => {
            let inspected = runtime.observation.ensure_managed(caddy).await?;
            if !failed_start(&inspected)
                || !is_running(&runtime.observation.ensure_managed(holder).await?)
                || !runtime.docker.created_nothing_since(creates)
            {
                return Ok(false);
            }
            runtime.start_inspected(caddy, &inspected).await?;
            Ok(true)
        }
        Repair::Rejoin { owner, joined } => {
            let owner_now = runtime.observation.ensure_managed(owner).await?;
            let joined_now = runtime.observation.ensure_managed(joined).await?;
            let stale = match (doors::netns_of(&owner_now), doors::netns_of(&joined_now)) {
                (Some(owner_ns), Some(joined_ns)) => owner_ns != joined_ns,
                _ => false,
            };
            if !stale
                || doors::joined_to(&joined_now) != Some(*owner)
                || !runtime.docker.created_nothing_since(creates)
            {
                return Ok(false);
            }
            // A stop here is no request's, so it holds nothing: the start that
            // follows is the same repair.
            let result = runtime
                .docker
                .client
                .stop_container(joined.as_str(), None)
                .await;
            idempotent_lifecycle_result(joined, result)?;
            let inspected = runtime.observation.ensure_managed(joined).await?;
            runtime.start_inspected(joined, &inspected).await?;
            Ok(true)
        }
    }
}

/// One Ingress door repair a scan decided on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Repair {
    /// A holder at rest whose last start failed.
    StartHolder(ContainerId),
    /// A Caddy at rest whose last start failed: one joined to the running `holder`,
    /// or a Caddy from before the holder when none exists.
    StartCaddy {
        holder: Option<ContainerId>,
        caddy: ContainerId,
    },
    /// A running Caddy left in a network namespace the running holder no longer has,
    /// or a running connector left in one the running Tunnel Origin no longer has.
    Rejoin {
        owner: ContainerId,
        joined: ContainerId,
    },
}

impl Repair {
    /// The Container the recovery starts.
    fn target(&self) -> &ContainerId {
        match self {
            Self::StartHolder(id) => id,
            Self::StartCaddy { caddy, .. } => caddy,
            Self::Rejoin { joined, .. } => joined,
        }
    }

    fn verb(&self) -> &'static str {
        match self {
            Self::StartHolder(_) | Self::StartCaddy { .. } => "started",
            Self::Rejoin { .. } => "rejoined",
        }
    }
}

/// The one recovery this scan calls for, if any.
///
/// Docker's restart policy covers a door that ran and died, not one whose start
/// failed, such as when another process held port 80 at enrollment or at boot, and
/// it never moves a Caddy into the network of a holder that restarted. A scan that
/// could not inspect every container may have missed a running one, so it names
/// nothing. A Caddy is never selected while its holder is stopped, restarting or
/// unknown: it would join nothing. Unknown evidence does nothing.
pub(super) fn plan(doors: &[DoorFacts], complete: bool) -> Option<Repair> {
    if !complete {
        return None;
    }
    proxy_recovery(doors).or_else(|| tunnel_recovery(doors))
}

/// The holder's or its Caddy's recovery, if any.
fn proxy_recovery(doors: &[DoorFacts]) -> Option<Repair> {
    let newest = |facts: &[&DoorFacts]| {
        facts
            .iter()
            .copied()
            .max_by_key(|facts| (facts.created_at, facts.id))
            .cloned()
    };
    let holders = doors
        .iter()
        .filter(|facts| facts.door == Door::Holder)
        .collect::<Vec<_>>();
    let caddys = doors
        .iter()
        .filter(|facts| facts.door == Door::Proxy)
        .collect::<Vec<_>>();
    if holders.is_empty() {
        let legacy = caddys
            .iter()
            .copied()
            .filter(|facts| facts.joined_to.is_none())
            .collect::<Vec<_>>();
        if !caddys.iter().all(|facts| facts.at_rest()) {
            return None;
        }
        let caddy = newest(&legacy)?;
        return caddy.failed_start.then_some(Repair::StartCaddy {
            holder: None,
            caddy: caddy.id,
        });
    }
    if holders.iter().all(|facts| facts.at_rest()) {
        let holder = newest(&holders)?;
        return holder
            .failed_start
            .then_some(Repair::StartHolder(holder.id));
    }
    let mut running = holders.iter().copied().filter(|facts| facts.running());
    let (Some(holder), None) = (running.next(), running.next()) else {
        return None;
    };
    if holders
        .iter()
        .any(|facts| !facts.running() && !facts.at_rest())
    {
        return None;
    }
    let joined = caddys
        .iter()
        .copied()
        .filter(|facts| facts.joined_to == Some(holder.id))
        .collect::<Vec<_>>();
    if let Some(caddy) = joined.iter().find(|facts| facts.running()) {
        return match (holder.netns, caddy.netns) {
            (Some(holder_ns), Some(caddy_ns)) if holder_ns != caddy_ns => Some(Repair::Rejoin {
                owner: holder.id,
                joined: caddy.id,
            }),
            _ => None,
        };
    }
    if !joined.iter().all(|facts| facts.at_rest()) {
        return None;
    }
    let caddy = newest(&joined)?;
    caddy.failed_start.then_some(Repair::StartCaddy {
        holder: Some(holder.id),
        caddy: caddy.id,
    })
}

/// A running connector to move into the running Tunnel Origin's network, if any.
///
/// Only that: a door at rest stays at rest, since the route starts the Tunnel Door
/// only once it has written the token and the Cluster still declares it. Docker
/// restarts a connector that exits into the origin's current network, so only a
/// connector that keeps running after its origin restarted is left behind.
fn tunnel_recovery(doors: &[DoorFacts]) -> Option<Repair> {
    let origins = doors
        .iter()
        .filter(|facts| facts.door == Door::TunnelOrigin)
        .collect::<Vec<_>>();
    let mut running = origins.iter().copied().filter(|facts| facts.running());
    let (Some(origin), None) = (running.next(), running.next()) else {
        return None;
    };
    if origins
        .iter()
        .any(|facts| !facts.running() && !facts.at_rest())
    {
        return None;
    }
    let origin_ns = origin.netns?;
    doors
        .iter()
        .find(|facts| {
            facts.door == Door::Tunnel
                && facts.running()
                && facts.joined_to == Some(origin.id)
                && facts.netns.is_some_and(|ns| ns != origin_ns)
        })
        .map(|connector| Repair::Rejoin {
            owner: origin.id,
            joined: connector.id,
        })
}

/// Sync inside, so a [`Held`] dropped mid-await still lifts its hold.
#[derive(Clone, Default)]
struct Holds(Arc<std::sync::Mutex<HoldTable>>);

#[derive(Default)]
struct HoldTable {
    stopped: BTreeSet<ContainerId>,
    /// Stops still deciding whether to proceed, one ticket each, so a hold that
    /// `forget` voided can never lift or keep a later stop's hold.
    deciding: BTreeMap<ContainerId, BTreeSet<u64>>,
    next_ticket: u64,
}

impl Holds {
    fn hold(&self, container_id: ContainerId) -> Held {
        let mut table = self.table();
        let ticket = table.next_ticket;
        table.next_ticket += 1;
        table
            .deciding
            .entry(container_id)
            .or_default()
            .insert(ticket);
        Held {
            holds: self.clone(),
            container_id,
            ticket,
        }
    }

    fn contains(&self, container_id: &ContainerId) -> bool {
        let table = self.table();
        table.stopped.contains(container_id) || table.deciding.contains_key(container_id)
    }

    fn release(&self, container_id: &ContainerId) {
        self.table().stopped.remove(container_id);
    }

    fn forget(&self, container_id: &ContainerId) {
        let mut table = self.table();
        table.stopped.remove(container_id);
        table.deciding.remove(container_id);
    }

    fn table(&self) -> std::sync::MutexGuard<'_, HoldTable> {
        self.0.lock().expect("stranded holds are never poisoned")
    }
}

/// One stop's hold while it decides whether to proceed.
#[must_use = "the hold lifts when it drops unless kept"]
struct Held {
    holds: Holds,
    container_id: ContainerId,
    ticket: u64,
}

impl Held {
    fn keep(self) {
        let mut table = self.holds.table();
        if table
            .deciding
            .get(&self.container_id)
            .is_some_and(|tickets| tickets.contains(&self.ticket))
        {
            table.stopped.insert(self.container_id);
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let mut table = self.holds.table();
        let Some(tickets) = table.deciding.get_mut(&self.container_id) else {
            return;
        };
        tickets.remove(&self.ticket);
        if tickets.is_empty() {
            table.deciding.remove(&self.container_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::ContainerId;
    use serde_json::json;

    use super::{Holds, Repair, plan};
    use crate::docker::{Door, DoorFacts, NetnsId};

    fn id(id: char) -> ContainerId {
        ContainerId::parse(id.to_string().repeat(64)).unwrap()
    }

    fn door(id_char: char, door: Door, created_at: i64, runtime: serde_json::Value) -> DoorFacts {
        DoorFacts {
            id: id(id_char),
            door,
            created_at,
            runtime: serde_json::from_value(runtime).unwrap(),
            failed_start: false,
            joined_to: None,
            netns: None,
        }
    }

    fn holder(id: char, created_at: i64, runtime: serde_json::Value) -> DoorFacts {
        door(id, Door::Holder, created_at, runtime)
    }

    fn caddy(id: char, created_at: i64, runtime: serde_json::Value) -> DoorFacts {
        door(id, Door::Proxy, created_at, runtime)
    }

    fn joined(mut facts: DoorFacts, holder: char) -> DoorFacts {
        facts.joined_to = Some(id(holder));
        facts
    }

    fn failed(mut facts: DoorFacts) -> DoorFacts {
        facts.failed_start = true;
        facts
    }

    fn in_netns(mut facts: DoorFacts, ino: u64) -> DoorFacts {
        facts.netns = Some(NetnsId::new(ino));
        facts
    }

    fn created() -> serde_json::Value {
        json!({ "state": "created" })
    }

    fn running() -> serde_json::Value {
        json!({ "state": "running", "health": "healthy" })
    }

    #[test]
    fn a_holder_whose_start_failed_is_started_again() {
        assert_eq!(
            plan(&[failed(holder('a', 1, created()))], true),
            Some(Repair::StartHolder(id('a')))
        );
        assert_eq!(
            plan(&[holder('a', 1, created())], true),
            None,
            "a holder Docker never tried to start is left alone"
        );
        assert_eq!(
            plan(
                &[
                    failed(holder('a', 1, created())),
                    failed(holder('d', 2, json!({ "state": "exited", "code": 128 }))),
                ],
                true
            ),
            Some(Repair::StartHolder(id('d'))),
            "the newest holder at rest is the candidate"
        );
    }

    #[test]
    fn a_caddy_is_never_selected_while_its_holder_is_not_running() {
        let caddy = failed(joined(caddy('c', 2, created()), 'a'));
        assert_eq!(
            plan(&[holder('a', 1, created()), caddy.clone()], true),
            None,
            "a stopped holder with no failed start starts nothing"
        );
        assert_eq!(
            plan(
                &[
                    holder('a', 1, json!({ "state": "restarting" })),
                    caddy.clone()
                ],
                true
            ),
            None
        );
        assert_eq!(
            plan(
                &[
                    holder('a', 1, json!({ "unrecognized": { "raw": "odd" } })),
                    caddy
                ],
                true
            ),
            None
        );
    }

    #[test]
    fn a_joined_caddy_whose_start_failed_is_started_once_the_holder_runs() {
        assert_eq!(
            plan(
                &[
                    holder('a', 1, running()),
                    failed(joined(caddy('c', 2, created()), 'a'))
                ],
                true
            ),
            Some(Repair::StartCaddy {
                holder: Some(id('a')),
                caddy: id('c'),
            })
        );
        assert_eq!(
            plan(
                &[
                    holder('a', 1, running()),
                    failed(joined(caddy('c', 2, created()), 'b'))
                ],
                true
            ),
            None,
            "a Caddy joined to another holder is not this holder's"
        );
        assert_eq!(
            plan(
                &[
                    holder('a', 1, running()),
                    joined(caddy('c', 2, running()), 'a'),
                    failed(joined(caddy('d', 3, created()), 'a')),
                ],
                true
            ),
            None,
            "one Caddy runs, so none is started"
        );
    }

    fn tunnel_origin(id: char, created_at: i64, runtime: serde_json::Value) -> DoorFacts {
        door(id, Door::TunnelOrigin, created_at, runtime)
    }

    fn connector(id: char, created_at: i64, runtime: serde_json::Value) -> DoorFacts {
        door(id, Door::Tunnel, created_at, runtime)
    }

    #[test]
    fn a_running_connector_in_the_origins_old_namespace_is_rejoined() {
        let doors = |connector_ns| {
            [
                in_netns(holder('a', 1, running()), 1),
                in_netns(joined(caddy('b', 2, running()), 'a'), 1),
                in_netns(tunnel_origin('e', 3, running()), 10),
                in_netns(joined(connector('f', 4, running()), 'e'), connector_ns),
            ]
        };
        assert_eq!(
            plan(&doors(11), true),
            Some(Repair::Rejoin {
                owner: id('e'),
                joined: id('f'),
            })
        );
        assert_eq!(plan(&doors(10), true), None);
        assert_eq!(
            plan(
                &[
                    in_netns(tunnel_origin('e', 3, running()), 10),
                    failed(joined(connector('f', 4, created()), 'e')),
                ],
                true
            ),
            None,
            "a connector at rest waits for the route, which writes its token first"
        );
    }

    #[test]
    fn a_running_caddy_in_the_holders_old_namespace_is_rejoined() {
        assert_eq!(
            plan(
                &[
                    in_netns(holder('a', 1, running()), 10),
                    in_netns(joined(caddy('c', 2, running()), 'a'), 11),
                ],
                true
            ),
            Some(Repair::Rejoin {
                owner: id('a'),
                joined: id('c'),
            })
        );
        assert_eq!(
            plan(
                &[
                    in_netns(holder('a', 1, running()), 10),
                    in_netns(joined(caddy('c', 2, running()), 'a'), 10),
                ],
                true
            ),
            None,
            "a Caddy in the holder's namespace is where it belongs"
        );
        assert_eq!(
            plan(
                &[
                    in_netns(holder('a', 1, running()), 10),
                    joined(caddy('c', 2, running()), 'a'),
                ],
                true
            ),
            None,
            "an unknown namespace is no evidence"
        );
    }

    #[test]
    fn without_a_holder_the_legacy_caddy_rule_holds() {
        assert_eq!(
            plan(&[failed(caddy('a', 1, created()))], true),
            Some(Repair::StartCaddy {
                holder: None,
                caddy: id('a'),
            })
        );
        assert_eq!(
            plan(
                &[
                    failed(caddy('a', 1, json!({ "state": "exited", "code": 0 }))),
                    failed(caddy('d', 2, created())),
                ],
                true
            ),
            Some(Repair::StartCaddy {
                holder: None,
                caddy: id('d'),
            })
        );
        assert_eq!(
            plan(
                &[caddy('a', 1, running()), failed(caddy('d', 2, created()))],
                true
            ),
            None
        );
        assert_eq!(
            plan(&[failed(joined(caddy('a', 1, created()), 'e'))], true),
            None,
            "a Caddy joined to a holder that is gone has no network to start in"
        );
        assert_eq!(plan(&[], true), None);
    }

    #[test]
    fn a_scan_that_could_not_inspect_every_container_recovers_nothing() {
        assert_eq!(plan(&[failed(holder('a', 1, created()))], false), None);
    }

    #[test]
    fn a_hold_forget_voided_never_touches_a_later_hold() {
        let container_id = id('a');
        let holds = Holds::default();

        let voided = holds.hold(container_id);
        holds.forget(&container_id);
        let later = holds.hold(container_id);
        voided.keep();
        drop(later);
        assert!(
            !holds.contains(&container_id),
            "a voided keep must not hold"
        );

        let voided = holds.hold(container_id);
        holds.forget(&container_id);
        let later = holds.hold(container_id);
        drop(voided);
        assert!(holds.contains(&container_id), "a voided drop must not lift");
        later.keep();
        assert!(holds.contains(&container_id), "the later stop is kept");
    }
}
