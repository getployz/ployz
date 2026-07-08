//! Shared NATS server-pool failover driven by the intent snapshot stream.

use crate::roles::machine::intent_mirror::MachineIntentMirror;
use futures_util::StreamExt;
use ployz_core::state::IntentSnapshot;
use ployz_core::subjects::INTENT_CHANGED;
use ployz_nats::connect::NatsClientUrl;
use ployz_nats::service_runtime::NatsClient;
use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

const INTENT_MIRROR_RESUBSCRIBE_DELAY: Duration = Duration::from_secs(5);
const EPOCH_ENFORCE_INTERVAL: Duration = Duration::from_secs(5);
const CORE_NATS_PORT: u16 = 4222;

/// A background task owned by a role process: a shutdown signal and its join
/// handle.
pub(crate) struct RunningFailoverTask {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl RunningFailoverTask {
    pub(crate) async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// The NATS failover pool a role cycles on core loss: the currently configured
/// seed plus every usable core/roster endpoint in the local intent mirror.
pub(crate) fn mirrored_server_pool(seed_file: &Path, seed: &NatsClientUrl) -> Vec<String> {
    let mirror = MachineIntentMirror::new(seed_file.with_file_name("intent-mirror.json"));
    match mirror.load() {
        Some(snapshot) => candidate_server_pool(&snapshot, seed),
        None => vec![seed.as_str().to_owned()],
    }
}

pub(crate) fn start_intent_failover_mirror(
    client: NatsClient,
    mirror: MachineIntentMirror,
    seed: NatsClientUrl,
) -> RunningFailoverTask {
    let (shutdown, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(run_intent_failover_mirror(
        client,
        mirror,
        seed,
        Shutdown::Oneshot(shutdown_rx),
    ));
    RunningFailoverTask { shutdown, task }
}

pub(crate) fn spawn_intent_failover_mirror(
    client: NatsClient,
    mirror: MachineIntentMirror,
    seed: NatsClientUrl,
    shutdown: broadcast::Receiver<()>,
) -> JoinHandle<()> {
    tokio::spawn(run_intent_failover_mirror(
        client,
        mirror,
        seed,
        Shutdown::Broadcast(shutdown),
    ))
}

enum Shutdown {
    Oneshot(oneshot::Receiver<()>),
    Broadcast(broadcast::Receiver<()>),
}

impl Shutdown {
    async fn recv(&mut self) {
        match self {
            Self::Oneshot(receiver) => {
                let _ = receiver.await;
            }
            Self::Broadcast(receiver) => {
                let _ = receiver.recv().await;
            }
        }
    }
}

async fn run_intent_failover_mirror(
    client: NatsClient,
    mirror: MachineIntentMirror,
    seed: NatsClientUrl,
    mut shutdown: Shutdown,
) {
    let mut last_enforced = Instant::now()
        .checked_sub(EPOCH_ENFORCE_INTERVAL)
        .unwrap_or_else(Instant::now);
    loop {
        let mut changed = match client.subscribe(INTENT_CHANGED).await {
            Ok(subscription) => subscription,
            Err(_) => {
                tokio::select! {
                    () = tokio::time::sleep(INTENT_MIRROR_RESUBSCRIBE_DELAY) => continue,
                    () = shutdown.recv() => return,
                }
            }
        };
        loop {
            tokio::select! {
                message = changed.next() => {
                    let Some(message) = message else { break };
                    if message.payload.is_empty() {
                        continue;
                    }
                    if let Ok(snapshot) =
                        serde_json::from_slice::<IntentSnapshot>(&message.payload)
                    {
                        let previous_epoch = mirror.load().map(|current| current.epoch);
                        match mirror.store(&snapshot) {
                            Ok(true) => {
                                let higher_epoch = previous_epoch
                                    .is_some_and(|epoch| snapshot.epoch > epoch);
                                let pool = if higher_epoch {
                                    higher_epoch_server_pool(&snapshot, &seed)
                                } else {
                                    candidate_server_pool(&snapshot, &seed)
                                };
                                let _ = client
                                    .set_server_pool(pool)
                                    .await;
                                if higher_epoch {
                                    let _ = client.force_reconnect().await;
                                }
                            }
                            Ok(false) => {
                                if last_enforced.elapsed() >= EPOCH_ENFORCE_INTERVAL {
                                    last_enforced = Instant::now();
                                    if let Some(best) = mirror.load() {
                                        let _ = client
                                            .set_server_pool(higher_epoch_server_pool(&best, &seed))
                                            .await;
                                    }
                                    let _ = client.force_reconnect().await;
                                }
                            }
                            Err(_) => {}
                        }
                    }
                }
                () = shutdown.recv() => return,
            }
        }
    }
}

fn candidate_server_pool(snapshot: &IntentSnapshot, seed: &NatsClientUrl) -> Vec<String> {
    let mut pool = vec![seed.as_str().to_owned()];
    push_unique(&mut pool, snapshot.core_urls.iter().cloned());
    push_unique(&mut pool, reachable_machine_urls(snapshot));
    pool
}

fn higher_epoch_server_pool(snapshot: &IntentSnapshot, seed: &NatsClientUrl) -> Vec<String> {
    let seed = seed.as_str().to_owned();
    let mut pool = Vec::new();
    push_unique(&mut pool, snapshot.core_urls.iter().cloned());
    push_unique(&mut pool, reachable_machine_urls(snapshot));
    let seed_is_still_named = snapshot.core_urls.iter().any(|url| url == &seed)
        || snapshot
            .active_machines
            .iter()
            .flat_map(|machine| machine.control_endpoints.iter())
            .any(|endpoint| {
                format!("tls://{}", SocketAddr::new(*endpoint, CORE_NATS_PORT)) == seed
            });
    if seed_is_still_named {
        push_unique(&mut pool, std::iter::once(seed.clone()));
    }
    if pool.is_empty() {
        pool.push(seed);
    }
    pool
}

fn reachable_machine_urls(snapshot: &IntentSnapshot) -> Vec<String> {
    let mut urls = Vec::new();
    for machine in &snapshot.active_machines {
        for endpoint in &machine.control_endpoints {
            let url = format!("tls://{}", SocketAddr::new(*endpoint, CORE_NATS_PORT));
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    urls
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
    use ployz_test_support::ids::{machine_id, operation_id};

    fn active_machine_with(id: &str, endpoint: Option<&str>) -> ActiveMachineState {
        ActiveMachineState {
            machine_id: machine_id(id),
            name: ployz_core::machine::MachineName::try_new(id).expect("machine name"),
            activated_by: operation_id("op_activate"),
            lifecycle: MachineLifecycle::Active,
            control_endpoints: endpoint
                .map(|ip| vec![ip.parse().expect("ip")])
                .unwrap_or_default(),
            mesh_endpoints: Vec::new(),
        }
    }

    fn snapshot_with(core_urls: Vec<&str>, machines: Vec<ActiveMachineState>) -> IntentSnapshot {
        IntentSnapshot {
            epoch: ControlPlaneEpoch::initial(),
            core_urls: core_urls.into_iter().map(str::to_owned).collect(),
            active_machines: machines,
            route_bindings: Vec::new(),
            serving_target_entries: Vec::new(),
            authorized_users: Vec::new(),
        }
    }

    #[test]
    fn candidate_server_pool_keeps_seed_first_then_core_urls_then_reachable_machines() {
        let seed = NatsClientUrl::try_new("tls://10.0.0.1:4222").expect("seed url");
        let snapshot = snapshot_with(
            vec!["tls://203.0.113.9:4222"],
            vec![
                active_machine_with("machine_a", Some("203.0.113.5")),
                active_machine_with("machine_b", None),
                active_machine_with("machine_c", Some("203.0.113.9")),
            ],
        );
        assert_eq!(
            candidate_server_pool(&snapshot, &seed),
            vec![
                "tls://10.0.0.1:4222".to_owned(),
                "tls://203.0.113.9:4222".to_owned(),
                "tls://203.0.113.5:4222".to_owned(),
            ]
        );
    }

    #[test]
    fn higher_epoch_server_pool_drops_old_seed_when_mirror_no_longer_names_it() {
        let seed = NatsClientUrl::try_new("tls://203.0.113.5:4222").expect("seed url");
        let snapshot = snapshot_with(
            vec!["tls://203.0.113.9:4222"],
            vec![active_machine_with("promoted_core", Some("203.0.113.9"))],
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
            vec!["tls://203.0.113.9:4222"],
            vec![
                active_machine_with("old_core", Some("203.0.113.5")),
                active_machine_with("promoted_core", Some("203.0.113.9")),
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
    fn mirrored_server_pool_loads_reachable_machines_from_cached_intent() {
        let dir = tempfile::tempdir().expect("temp dir");
        let seed_file = dir.path().join("machine.seed");
        let mirror = MachineIntentMirror::new(dir.path().join("intent-mirror.json"));
        mirror
            .store(&snapshot_with(
                vec!["tls://203.0.113.9:4222"],
                vec![active_machine_with("promoted_core", Some("203.0.113.9"))],
            ))
            .expect("store mirror");
        let seed = NatsClientUrl::try_new("tls://203.0.113.5:4222").expect("seed url");

        assert_eq!(
            mirrored_server_pool(&seed_file, &seed),
            vec![
                "tls://203.0.113.5:4222".to_owned(),
                "tls://203.0.113.9:4222".to_owned(),
            ]
        );
    }
}
