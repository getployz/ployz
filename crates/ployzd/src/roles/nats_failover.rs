//! Shared NATS server-pool failover driven by the intent snapshot stream.
//!
//! One state machine for machine, gateway, and DNS (plan
//! 2026-07-08-001): before the mirror ever records a promotion the pool is
//! seed-first plus mirrored Reachable Machine endpoints; once the mirror holds
//! an epoch above the initial one, the current core's roster-derived endpoints
//! head the pool and the bootstrap seed survives only if the accepted mirror
//! still names it. Lower-epoch snapshots are rejected and the last good pool is
//! kept. Pool ordering is transport preference, never cluster truth.

use crate::intent::service::NatsIntentReader;
use crate::roles::machine::intent_mirror::{MachineIntentMirror, StoreOutcome};
use futures_util::StreamExt;
use ployz_core::state::{ControlPlaneEpoch, IntentSnapshot};
use ployz_core::subjects::INTENT_CHANGED;
use ployz_nats::connect::NatsClientUrl;
use ployz_nats::service_runtime::NatsClient;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

const INTENT_MIRROR_RESUBSCRIBE_DELAY: Duration = Duration::from_secs(5);
const EPOCH_ENFORCE_INTERVAL: Duration = Duration::from_secs(5);
const INTENT_REPAIR_TIMEOUT: Duration = Duration::from_secs(5);
/// The port a promoted core's `nats-server` listens on (Host Runner's
/// `DEFAULT_NATS_PORT`), used to build failover candidate URLs.
const CORE_NATS_PORT: u16 = 4222;

/// The role-local recovery context a failover-capable role client owns: its
/// machine-local intent mirror and the configured bootstrap seed URL.
#[derive(Debug, Clone)]
pub struct IntentFailover {
    pub mirror: MachineIntentMirror,
    pub seed: NatsClientUrl,
}

/// The NATS pool a role connects with at boot: the configured seed plus every
/// mirrored Reachable Machine endpoint while the mirror is at the initial
/// epoch, or the mirrored core-preferred pool once the mirror records a
/// promotion — consistent with the runtime state machine, so a machine
/// rebooting during a core outage dials the promoted core first.
pub(crate) fn mirrored_server_pool(
    mirror: &MachineIntentMirror,
    seed: &NatsClientUrl,
) -> Vec<String> {
    match mirror.load() {
        Some(snapshot) => snapshot_server_pool(&snapshot, seed),
        None => vec![seed.as_str().to_owned()],
    }
}

pub(crate) fn spawn_intent_failover_mirror(
    client: NatsClient,
    failover: IntentFailover,
    shutdown: broadcast::Receiver<()>,
) -> JoinHandle<()> {
    tokio::spawn(run_intent_failover_mirror(client, failover, shutdown))
}

async fn run_intent_failover_mirror(
    client: NatsClient,
    failover: IntentFailover,
    mut shutdown: broadcast::Receiver<()>,
) {
    let IntentFailover { mirror, seed } = failover;
    let reader = NatsIntentReader::new(client.clone()).with_request_timeout(INTENT_REPAIR_TIMEOUT);
    let mut run = IntentFailoverRun {
        applied_pool: mirrored_server_pool(&mirror, &seed),
        client,
        mirror,
        seed,
        last_enforced: Instant::now()
            .checked_sub(EPOCH_ENFORCE_INTERVAL)
            .unwrap_or_else(Instant::now),
        consecutive_failures: 0,
    };
    loop {
        let mut changed = match run.client.subscribe(INTENT_CHANGED).await {
            Ok(subscription) => subscription,
            Err(error) => {
                run.warn("subscribe", error);
                if run
                    .repair_from_intent_get_or_shutdown(&reader, &mut shutdown)
                    .await
                {
                    return;
                }
                tokio::select! {
                    () = tokio::time::sleep(INTENT_MIRROR_RESUBSCRIBE_DELAY) => continue,
                    _ = shutdown.recv() => return,
                }
            }
        };
        loop {
            tokio::select! {
                message = changed.next() => {
                    let Some(message) = message else {
                        // Repair the possibly missed snapshots before the
                        // outer loop resubscribes (spec U3).
                        if run
                            .repair_from_intent_get_or_shutdown(&reader, &mut shutdown)
                            .await
                        {
                            return;
                        }
                        break;
                    };
                    if message.payload.is_empty() {
                        continue;
                    }
                    match serde_json::from_slice::<IntentSnapshot>(&message.payload) {
                        Ok(snapshot) => run.apply_snapshot(&snapshot).await,
                        Err(error) => {
                            run.warn("decode-intent", error);
                            if run
                                .repair_from_intent_get_or_shutdown(&reader, &mut shutdown)
                                .await
                            {
                                return;
                            }
                        }
                    }
                }
                _ = shutdown.recv() => return,
            }
        }
    }
}

/// Whether applying a pool should also drop the current connection so the
/// client re-dials in the new preference order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reconnect {
    KeepConnection,
    AwayFromCurrent,
}

struct IntentFailoverRun {
    client: NatsClient,
    mirror: MachineIntentMirror,
    seed: NatsClientUrl,
    /// The last pool successfully handed to the client, so stale-reject
    /// enforcement only forces a reconnect when the pool actually changes.
    applied_pool: Vec<String>,
    last_enforced: Instant,
    consecutive_failures: u64,
}

impl IntentFailoverRun {
    async fn apply_snapshot(&mut self, snapshot: &IntentSnapshot) {
        let outcome = match self.mirror.store(snapshot) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.warn("store-mirror", error);
                return;
            }
        };
        match outcome {
            StoreOutcome::AcceptedHigher => {
                let pool = snapshot_server_pool(snapshot, &self.seed);
                self.apply_pool(pool, Reconnect::AwayFromCurrent).await;
            }
            StoreOutcome::AcceptedCurrent => {
                let pool = snapshot_server_pool(snapshot, &self.seed);
                self.apply_pool(pool, Reconnect::KeepConnection).await;
            }
            StoreOutcome::RejectedStale => self.enforce_pool_over_stale(snapshot).await,
        }
    }

    async fn apply_pool(&mut self, pool: Vec<String>, reconnect: Reconnect) {
        if let Err(error) = self.client.set_server_pool(pool.clone()).await {
            self.warn("set-server-pool", error);
            return;
        }
        self.applied_pool = pool;
        match reconnect {
            Reconnect::KeepConnection => self.consecutive_failures = 0,
            Reconnect::AwayFromCurrent => match self.client.force_reconnect().await {
                Ok(()) => self.consecutive_failures = 0,
                Err(error) => self.warn("force-reconnect", error),
            },
        }
    }

    /// A stale (lower-epoch) snapshot was rejected: keep the last good pool
    /// and, if it is not already the applied one, reconnect away from the
    /// stale core. Rate-limited so a stale publisher's steady drumbeat does
    /// not spam warnings.
    async fn enforce_pool_over_stale(&mut self, rejected: &IntentSnapshot) {
        if self.last_enforced.elapsed() < EPOCH_ENFORCE_INTERVAL {
            return;
        }
        self.last_enforced = Instant::now();
        let Some(best) = self.mirror.load() else {
            self.warn(
                "load-best-mirror-stale-reject",
                "missing local mirror after stale snapshot rejection",
            );
            return;
        };
        eprintln!(
            "ployzd nats failover warning: phase=reject-stale-intent local_mirror_epoch={} rejected_epoch={}",
            best.epoch.get(),
            rejected.epoch.get()
        );
        let pool = snapshot_server_pool(&best, &self.seed);
        if pool != self.applied_pool {
            self.apply_pool(pool, Reconnect::AwayFromCurrent).await;
        }
    }

    /// One `intent.get` repair read (spec U3), taken on subscribe failure,
    /// decode failure, or subscription end, and applied through the same
    /// store path as streamed snapshots.
    async fn repair_from_intent_get(&mut self, reader: &NatsIntentReader) {
        match reader.intent().await {
            Ok(snapshot) => self.apply_snapshot(&snapshot).await,
            Err(error) => self.warn("intent-get-repair", error),
        }
    }

    async fn repair_from_intent_get_or_shutdown(
        &mut self,
        reader: &NatsIntentReader,
        shutdown: &mut broadcast::Receiver<()>,
    ) -> bool {
        tokio::select! {
            () = self.repair_from_intent_get(reader) => false,
            _ = shutdown.recv() => true,
        }
    }

    fn warn(&mut self, phase: &str, error: impl std::fmt::Display) {
        self.consecutive_failures += 1;
        eprintln!(
            "ployzd nats failover warning: phase={phase} consecutive_failures={} error={error}",
            self.consecutive_failures
        );
    }
}

/// The pool a mirrored snapshot renders: seed-first before the cluster has
/// ever been promoted, core-preferred (spec R3: the seed survives only if the
/// mirror still names it) once the snapshot's epoch is above the initial one.
fn snapshot_server_pool(snapshot: &IntentSnapshot, seed: &NatsClientUrl) -> Vec<String> {
    if snapshot.epoch > ControlPlaneEpoch::initial() {
        higher_epoch_server_pool(snapshot, seed)
    } else {
        candidate_server_pool(snapshot, seed)
    }
}

fn candidate_server_pool(snapshot: &IntentSnapshot, seed: &NatsClientUrl) -> Vec<String> {
    let mut pool = vec![seed.as_str().to_owned()];
    push_unique(&mut pool, reachable_server_urls(snapshot));
    pool
}

fn higher_epoch_server_pool(snapshot: &IntentSnapshot, seed: &NatsClientUrl) -> Vec<String> {
    let mut pool = Vec::new();
    push_unique(&mut pool, core_server_urls(snapshot));
    push_unique(&mut pool, reachable_server_urls(snapshot));
    if pool.is_empty() {
        pool.push(seed.as_str().to_owned());
    }
    pool
}

fn core_server_urls(snapshot: &IntentSnapshot) -> Vec<String> {
    snapshot
        .control_endpoints_of(&snapshot.core_machine_id)
        .into_iter()
        .flatten()
        .map(control_endpoint_url)
        .collect()
}

fn reachable_server_urls(snapshot: &IntentSnapshot) -> Vec<String> {
    snapshot
        .active_machines
        .iter()
        .flat_map(|machine| machine.control_endpoints.iter())
        .map(control_endpoint_url)
        .collect()
}

fn control_endpoint_url(ip: &IpAddr) -> String {
    // SocketAddr's Display brackets IPv6 (`[::1]:4222`); a bare interpolation
    // would emit an invalid `tls://::1:4222`.
    format!("tls://{}", SocketAddr::new(*ip, CORE_NATS_PORT))
}

fn push_unique(pool: &mut Vec<String>, urls: impl IntoIterator<Item = String>) {
    for url in urls {
        if !pool.contains(&url) {
            pool.push(url);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::state::{ActiveMachineState, ControlPlaneEpoch, MachineLifecycle};
    use ployz_core::subjects::INTENT_GET;
    use ployz_test_support::ids::{machine_id, operation_id};

    fn active_machine_with(id: &str, endpoints: &[&str]) -> ActiveMachineState {
        ActiveMachineState {
            machine_id: machine_id(id),
            name: ployz_core::machine::MachineName::try_new(id).expect("machine name"),
            activated_by: operation_id("op_activate"),
            roles: ployz_core::roles::InstallRolePolicy::install_all(),
            lifecycle: MachineLifecycle::Active,
            control_endpoints: endpoints.iter().map(|ip| ip.parse().expect("ip")).collect(),
            mesh_endpoints: Vec::new(),
            endpoint_subnet: ployz_core::dataplane::MachineEndpointSubnet::try_new("10.198.0.0/24")
                .expect("valid endpoint subnet"),
            wireguard_public_key: ployz_core::dataplane::WireGuardPublicKey::try_new(format!(
                "public-{id}"
            ))
            .expect("public key"),
        }
    }

    fn snapshot_with_epoch(
        epoch: ControlPlaneEpoch,
        core_machine_id: &str,
        machines: Vec<ActiveMachineState>,
    ) -> IntentSnapshot {
        IntentSnapshot {
            epoch,
            core_machine_id: machine_id(core_machine_id),
            active_machines: machines,
            dataplane_projection: ployz_core::dataplane::DataplaneProjection::try_new(
                Vec::new(),
                None,
            )
            .expect("empty projection"),
            route_bindings: Vec::new(),
            serving_target_entries: Vec::new(),
            volume_pins: Vec::new(),
            nats_authorizations: Vec::new(),
            public_url: ployz_core::state::IntentPublicUrl::Auto(Box::new(
                ployz_core::state::ManagedLeaseProjection::Unacquired,
            )),
            custom_certificates: Vec::new(),
            acme_http01_challenges: Vec::new(),
        }
    }

    fn snapshot_with(core_machine_id: &str, machines: Vec<ActiveMachineState>) -> IntentSnapshot {
        snapshot_with_epoch(ControlPlaneEpoch::initial(), core_machine_id, machines)
    }

    #[test]
    fn candidate_server_pool_keeps_seed_first_then_reachable_urls() {
        let seed = NatsClientUrl::try_new("tls://10.0.0.1:4222").expect("seed url");
        let snapshot = snapshot_with(
            "machine_c",
            vec![
                active_machine_with("machine_a", &["203.0.113.5"]),
                active_machine_with("machine_b", &[]),
                active_machine_with("machine_c", &["203.0.113.9"]),
            ],
        );
        assert_eq!(
            candidate_server_pool(&snapshot, &seed),
            vec![
                "tls://10.0.0.1:4222".to_owned(),
                "tls://203.0.113.5:4222".to_owned(),
                "tls://203.0.113.9:4222".to_owned(),
            ]
        );
    }

    #[test]
    fn higher_epoch_server_pool_drops_old_seed_when_mirror_no_longer_names_it() {
        let seed = NatsClientUrl::try_new("tls://203.0.113.5:4222").expect("seed url");
        let snapshot = snapshot_with(
            "promoted_core",
            vec![active_machine_with("promoted_core", &["203.0.113.9"])],
        );
        assert_eq!(
            higher_epoch_server_pool(&snapshot, &seed),
            vec!["tls://203.0.113.9:4222".to_owned()]
        );
    }

    #[test]
    fn higher_epoch_server_pool_keeps_seed_when_mirror_names_it() {
        let seed = NatsClientUrl::try_new("tls://203.0.113.5:4222").expect("seed url");
        let snapshot = snapshot_with(
            "promoted_core",
            vec![
                active_machine_with("old_core", &["203.0.113.5"]),
                active_machine_with("promoted_core", &["203.0.113.9"]),
            ],
        );
        assert_eq!(
            higher_epoch_server_pool(&snapshot, &seed),
            vec![
                "tls://203.0.113.9:4222".to_owned(),
                "tls://203.0.113.5:4222".to_owned(),
            ]
        );
    }

    #[test]
    fn snapshot_server_pool_is_seed_first_at_the_initial_epoch() {
        let seed = NatsClientUrl::try_new("tls://203.0.113.5:4222").expect("seed url");
        let snapshot = snapshot_with(
            "machine_a",
            vec![active_machine_with("machine_a", &["203.0.113.9"])],
        );
        assert_eq!(
            snapshot_server_pool(&snapshot, &seed),
            vec![
                "tls://203.0.113.5:4222".to_owned(),
                "tls://203.0.113.9:4222".to_owned(),
            ]
        );
    }

    #[test]
    fn snapshot_server_pool_is_core_preferred_on_every_post_promotion_drumbeat() {
        // R3: after a promotion, even a same-epoch rebroadcast must not put
        // the old seed back at the head of the pool.
        let seed = NatsClientUrl::try_new("tls://203.0.113.5:4222").expect("seed url");
        let snapshot = snapshot_with_epoch(
            ControlPlaneEpoch::initial().next(),
            "promoted_core",
            vec![active_machine_with("promoted_core", &["203.0.113.9"])],
        );
        assert_eq!(
            snapshot_server_pool(&snapshot, &seed),
            vec!["tls://203.0.113.9:4222".to_owned()]
        );
    }

    #[test]
    fn mirrored_server_pool_keeps_seed_first_while_the_mirror_is_unpromoted() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mirror = MachineIntentMirror::new(dir.path().join("intent-mirror.json"));
        mirror
            .store(&snapshot_with(
                "machine_a",
                vec![active_machine_with("machine_a", &["203.0.113.9"])],
            ))
            .expect("store mirror");
        let seed = NatsClientUrl::try_new("tls://203.0.113.5:4222").expect("seed url");

        assert_eq!(
            mirrored_server_pool(&mirror, &seed),
            vec![
                "tls://203.0.113.5:4222".to_owned(),
                "tls://203.0.113.9:4222".to_owned(),
            ]
        );
    }

    #[test]
    fn mirrored_server_pool_prefers_cached_core_endpoints_after_a_promotion() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mirror = MachineIntentMirror::new(dir.path().join("intent-mirror.json"));
        mirror
            .store(&snapshot_with_epoch(
                ControlPlaneEpoch::initial().next(),
                "promoted_core",
                vec![active_machine_with("promoted_core", &["203.0.113.9"])],
            ))
            .expect("store mirror");
        let seed = NatsClientUrl::try_new("tls://203.0.113.5:4222").expect("seed url");

        assert_eq!(
            mirrored_server_pool(&mirror, &seed),
            vec!["tls://203.0.113.9:4222".to_owned()]
        );
    }

    #[test]
    fn mirrored_server_pool_without_a_mirror_is_seed_only() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mirror = MachineIntentMirror::new(dir.path().join("intent-mirror.json"));
        let seed = NatsClientUrl::try_new("tls://203.0.113.5:4222").expect("seed url");

        assert_eq!(
            mirrored_server_pool(&mirror, &seed),
            vec!["tls://203.0.113.5:4222".to_owned()]
        );
    }

    #[tokio::test]
    async fn shutdown_cancels_blocked_intent_repair() {
        let nats =
            ployz_test_support::nats::TestNats::start_with_machines(&[machine_id("machine_a")])
                .await;
        let client = nats.machine_client(&machine_id("machine_a")).await;
        let mut blocked_intent = nats
            .controller
            .subscribe(INTENT_GET)
            .await
            .expect("subscribe without replying to intent requests");
        nats.controller
            .flush()
            .await
            .expect("blocked responder is ready");
        let dir = tempfile::tempdir().expect("temp dir");
        let (shutdown, shutdown_rx) = broadcast::channel(1);
        let task = spawn_intent_failover_mirror(
            client,
            IntentFailover {
                mirror: MachineIntentMirror::new(dir.path().join("intent-mirror.json")),
                seed: NatsClientUrl::try_new("tls://127.0.0.1:4222").expect("seed url"),
            },
            shutdown_rx,
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                nats.controller
                    .publish(INTENT_CHANGED, vec![b'{'].into())
                    .await
                    .expect("invalid intent publishes");
                nats.controller
                    .flush()
                    .await
                    .expect("intent publish flushes");
                if tokio::time::timeout(Duration::from_millis(20), blocked_intent.next())
                    .await
                    .is_ok_and(|request| request.is_some())
                {
                    break;
                }
            }
        })
        .await
        .expect("invalid intent triggers repair");

        shutdown.send(()).expect("shutdown sends");
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("shutdown cancels intent repair")
            .expect("failover task joins");
    }
}
