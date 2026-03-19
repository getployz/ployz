use crate::machine_liveness::machine_is_fresh;
use crate::mesh::probe::{TcpProbeResult, TcpProbeStatus, probe_overlay_ips_parallel};
use crate::mesh::tasks::{SelfRecordMutation, apply_self_record_mutation};
use crate::model::{MachineId, MachineRecord, Participation};
use ployz_runtime_api::{WireGuardDevice, WireguardDriver};
use ployz_store_api::MachineStore;
use ployz_store_api::StoreDriver;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

const PARTICIPATION_INTERVAL: Duration = Duration::from_secs(5);
const PARTICIPATION_HYSTERESIS_SAMPLES: u8 = 3;

#[derive(Debug, Default)]
struct ParticipationState {
    consecutive_good_samples: u8,
    consecutive_bad_samples: u8,
    forced_participation: Option<Participation>,
}

#[derive(Debug)]
pub enum ParticipationCommand {
    TickNow {
        done: oneshot::Sender<()>,
    },
    SetForced {
        participation: Option<Participation>,
        done: oneshot::Sender<()>,
    },
}

#[derive(Debug, PartialEq, Eq)]
struct RequiredPeerHealth {
    required_peer_ids: Vec<String>,
    unhealthy_required_peer_ids: Vec<String>,
}

impl RequiredPeerHealth {
    fn healthy(&self) -> bool {
        self.unhealthy_required_peer_ids.is_empty()
    }

    fn sample(&self) -> PeerHealthSample {
        if self.healthy() {
            PeerHealthSample::Healthy
        } else {
            PeerHealthSample::Unhealthy
        }
    }
}

pub(crate) async fn run_participation_task(
    machine_id: MachineId,
    authoritative_self: Arc<RwLock<MachineRecord>>,
    store: StoreDriver,
    network: WireguardDriver,
    self_record_tx: mpsc::Sender<crate::mesh::tasks::self_record::SelfRecordCommand>,
    mut commands: mpsc::Receiver<ParticipationCommand>,
    cancel: CancellationToken,
) {
    let mut interval = tokio::time::interval(PARTICIPATION_INTERVAL);
    let mut state = ParticipationState::default();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("participation task cancelled");
                break;
            }
            _ = interval.tick() => {
                participation_once(
                    &machine_id,
                    &authoritative_self,
                    &store,
                    &network,
                    &self_record_tx,
                    &mut state,
                ).await;
            }
            Some(command) = commands.recv() => {
                match command {
                    ParticipationCommand::TickNow { done } => {
                        participation_once(
                            &machine_id,
                            &authoritative_self,
                            &store,
                            &network,
                            &self_record_tx,
                            &mut state,
                        ).await;
                        let _ = done.send(());
                    }
                    ParticipationCommand::SetForced { participation, done } => {
                        state.forced_participation = participation;
                        participation_once(
                            &machine_id,
                            &authoritative_self,
                            &store,
                            &network,
                            &self_record_tx,
                            &mut state,
                        ).await;
                        let _ = done.send(());
                    }
                }
            }
        }
    }
}

async fn participation_once(
    machine_id: &MachineId,
    authoritative_self: &Arc<RwLock<MachineRecord>>,
    store: &StoreDriver,
    network: &WireguardDriver,
    self_record_tx: &mpsc::Sender<crate::mesh::tasks::self_record::SelfRecordCommand>,
    state: &mut ParticipationState,
) {
    let machines = match store.list_machines().await {
        Ok(machines) => machines,
        Err(error) => {
            warn!(?error, "failed to read machines for participation");
            return;
        }
    };

    let now = crate::time::now_unix_secs();
    let required_peers = required_peers_for_participation(&machines, machine_id, now);
    let current = authoritative_self.read().await.clone();
    let peer_health = required_peers_health(network, &required_peers).await;
    update_hysteresis(state, peer_health.sample());

    let next = state
        .forced_participation
        .unwrap_or_else(|| match current.participation {
            Participation::Disabled => {
                if state.consecutive_good_samples >= PARTICIPATION_HYSTERESIS_SAMPLES {
                    Participation::Enabled
                } else {
                    Participation::Disabled
                }
            }
            Participation::Enabled => {
                if state.consecutive_bad_samples >= PARTICIPATION_HYSTERESIS_SAMPLES {
                    Participation::Disabled
                } else {
                    Participation::Enabled
                }
            }
            Participation::Draining => Participation::Draining,
        });

    if next != current.participation {
        info!(
            machine_id = %machine_id,
            from = %current.participation,
            to = %next,
            good_samples = state.consecutive_good_samples,
            bad_samples = state.consecutive_bad_samples,
            required_peers = ?peer_health.required_peer_ids,
            unhealthy_required_peers = ?peer_health.unhealthy_required_peer_ids,
            "participation changed"
        );
    } else if !peer_health.healthy() {
        debug!(
            machine_id = %machine_id,
            participation = %current.participation,
            good_samples = state.consecutive_good_samples,
            bad_samples = state.consecutive_bad_samples,
            required_peers = ?peer_health.required_peer_ids,
            unhealthy_required_peers = ?peer_health.unhealthy_required_peer_ids,
            "participation waiting on required peers"
        );
    }

    if next != current.participation {
        let _ = apply_self_record_mutation(
            self_record_tx,
            SelfRecordMutation::SetParticipation {
                participation: next,
            },
        )
        .await;
    }
}

fn required_peers_for_participation(
    machines: &[MachineRecord],
    machine_id: &MachineId,
    now: u64,
) -> Vec<MachineRecord> {
    machines
        .iter()
        .filter(|machine| machine.id != *machine_id)
        .filter(|machine| match machine.participation {
            Participation::Disabled => false,
            Participation::Enabled | Participation::Draining => true,
        })
        .filter(|machine| machine_is_fresh(machine, now))
        .cloned()
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeerHealthSample {
    Healthy,
    Unhealthy,
}

fn update_hysteresis(state: &mut ParticipationState, sample: PeerHealthSample) {
    if sample == PeerHealthSample::Healthy {
        state.consecutive_bad_samples = 0;
        state.consecutive_good_samples = state
            .consecutive_good_samples
            .saturating_add(1)
            .min(PARTICIPATION_HYSTERESIS_SAMPLES);
        return;
    }

    state.consecutive_good_samples = 0;
    state.consecutive_bad_samples = state
        .consecutive_bad_samples
        .saturating_add(1)
        .min(PARTICIPATION_HYSTERESIS_SAMPLES);
}

async fn required_peers_health(
    network: &WireguardDriver,
    required_peers: &[MachineRecord],
) -> RequiredPeerHealth {
    let required_peer_ids = required_peers
        .iter()
        .map(|peer| peer.id.0.clone())
        .collect::<Vec<_>>();
    if required_peers.is_empty() {
        return RequiredPeerHealth {
            required_peer_ids,
            unhealthy_required_peer_ids: Vec::new(),
        };
    }

    if let Err(error) = network.read_peers().await {
        warn!(
            ?error,
            "failed to read direct wireguard peers for participation"
        );
        return RequiredPeerHealth {
            required_peer_ids: required_peer_ids.clone(),
            unhealthy_required_peer_ids: required_peer_ids,
        };
    }

    let overlay_probe_results = probe_overlay_ips_parallel(
        &required_peers
            .iter()
            .map(|peer| peer.overlay_ip)
            .collect::<Vec<_>>(),
    )
    .await;
    let unhealthy_required_peer_ids = required_peers
        .iter()
        .filter(|peer| !required_peer_is_healthy(peer, &overlay_probe_results))
        .map(|peer| peer.id.0.clone())
        .collect();

    RequiredPeerHealth {
        required_peer_ids,
        unhealthy_required_peer_ids,
    }
}

fn required_peer_is_healthy(
    peer: &MachineRecord,
    overlay_probe_results: &HashMap<crate::model::OverlayIp, TcpProbeResult>,
) -> bool {
    matches!(
        overlay_probe_results
            .get(&peer.overlay_ip)
            .map(|result| result.status),
        Some(TcpProbeStatus::Reachable)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::probe::run_probe_listener_task;
    use crate::mesh::tasks::run_self_record_writer_task;
    use crate::model::{MachineStatus, OverlayIp, PublicKey};
    use ployz_runtime_api::{DevicePeer, MemoryServiceRuntime, MemoryWireGuard, WireguardDriver};
    use ployz_store_api::memory::MemoryStore;
    use std::collections::BTreeMap;
    use std::net::Ipv6Addr;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tokio::sync::RwLock;

    fn test_machine(
        id: &str,
        participation: Participation,
        public_key: PublicKey,
        last_heartbeat: u64,
    ) -> MachineRecord {
        MachineRecord {
            id: MachineId(id.into()),
            public_key,
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            subnet: None,
            bridge_ip: None,
            endpoints: vec!["127.0.0.1:51820".into()],
            status: MachineStatus::Unknown,
            participation,
            last_heartbeat,
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }

    async fn test_runtime(
        self_participation: Participation,
        peer_participation: Participation,
    ) -> (
        Arc<MemoryStore>,
        Arc<MemoryServiceRuntime>,
        Arc<MemoryWireGuard>,
        Arc<RwLock<MachineRecord>>,
        mpsc::Sender<crate::mesh::tasks::self_record::SelfRecordCommand>,
        MachineId,
        PublicKey,
        CancellationToken,
        tokio::task::JoinHandle<()>,
    ) {
        let store = Arc::new(MemoryStore::new());
        let service = Arc::new(MemoryServiceRuntime::new());
        let network = Arc::new(MemoryWireGuard::new());
        let self_id = MachineId("self".into());
        let peer_key = PublicKey([2; 32]);
        let now = crate::time::now_unix_secs();

        let self_record = test_machine("self", self_participation, PublicKey([1; 32]), now);
        let peer_record = test_machine("peer", peer_participation, peer_key.clone(), now);
        store
            .upsert_self_machine(&self_record)
            .await
            .expect("upsert self");
        store
            .upsert_self_machine(&peer_record)
            .await
            .expect("upsert peer");

        let authoritative_self = Arc::new(RwLock::new(self_record));
        let store_driver = StoreDriver::memory_with(store.clone());
        let (self_record_tx, self_record_rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let writer_authoritative_self = authoritative_self.clone();
        let writer_handle = tokio::spawn(async move {
            run_self_record_writer_task(
                writer_authoritative_self,
                store_driver,
                self_record_rx,
                task_cancel,
            )
            .await;
        });

        (
            store,
            service,
            network,
            authoritative_self,
            self_record_tx,
            self_id,
            peer_key,
            cancel,
            writer_handle,
        )
    }

    #[test]
    fn update_hysteresis_tracks_good_and_bad_samples() {
        let mut state = ParticipationState::default();
        update_hysteresis(&mut state, PeerHealthSample::Healthy);
        update_hysteresis(&mut state, PeerHealthSample::Healthy);
        assert_eq!(state.consecutive_good_samples, 2);
        assert_eq!(state.consecutive_bad_samples, 0);

        update_hysteresis(&mut state, PeerHealthSample::Unhealthy);
        assert_eq!(state.consecutive_good_samples, 0);
        assert_eq!(state.consecutive_bad_samples, 1);
    }

    #[test]
    fn required_peer_is_healthy_when_overlay_probe_is_reachable() {
        let peer = test_machine("peer", Participation::Enabled, PublicKey([2; 32]), 100);
        let overlay_probe_results = HashMap::from([(
            peer.overlay_ip,
            TcpProbeResult {
                status: TcpProbeStatus::Reachable,
                rtt: Some(Duration::from_millis(10)),
            },
        )]);

        assert!(required_peer_is_healthy(&peer, &overlay_probe_results));
    }

    #[test]
    fn required_peer_is_unhealthy_when_overlay_probe_is_unreachable() {
        let peer = test_machine("peer", Participation::Enabled, PublicKey([2; 32]), 100);
        let overlay_probe_results = HashMap::from([(
            peer.overlay_ip,
            TcpProbeResult {
                status: TcpProbeStatus::Unreachable,
                rtt: None,
            },
        )]);

        assert!(!required_peer_is_healthy(&peer, &overlay_probe_results));
    }

    #[test]
    fn required_peer_filter_ignores_disabled_and_stale_peers() {
        let machine_id = MachineId("self".into());
        let machines = [
            test_machine("self", Participation::Disabled, PublicKey([0; 32]), 100),
            test_machine("disabled", Participation::Disabled, PublicKey([1; 32]), 100),
            test_machine("enabled", Participation::Enabled, PublicKey([2; 32]), 100),
            test_machine("draining", Participation::Draining, PublicKey([3; 32]), 100),
            test_machine("stale", Participation::Enabled, PublicKey([4; 32]), 69),
        ];

        let required = required_peers_for_participation(&machines, &machine_id, 100);
        assert_eq!(
            required
                .iter()
                .map(|machine| machine.id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["enabled", "draining"]
        );
    }

    #[tokio::test]
    async fn participation_promotes_disabled_after_three_healthy_samples() {
        let (_probe_guard, probe_cancel, probe_task) = start_test_probe_listener();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let (
            store,
            _service,
            network,
            authoritative_self,
            self_record_tx,
            self_id,
            peer_key,
            cancel,
            writer_handle,
        ) = test_runtime(Participation::Disabled, Participation::Enabled).await;
        network.set_device_peers(vec![DevicePeer {
            public_key: peer_key,
            endpoint: Some("127.0.0.1:51820".into()),
            last_handshake: Some(tokio::time::Instant::now()),
        }]);

        let mut state = ParticipationState::default();
        let store_driver = StoreDriver::memory_with(store.clone());
        let network_driver = WireguardDriver::memory_with(network);

        for _ in 0..3 {
            participation_once(
                &self_id,
                &authoritative_self,
                &store_driver,
                &network_driver,
                &self_record_tx,
                &mut state,
            )
            .await;
        }

        cancel.cancel();
        writer_handle.await.expect("writer exits");

        let machines = store.list_machines().await.expect("list machines");
        let self_record = machines
            .into_iter()
            .find(|machine| machine.id == self_id)
            .expect("self record");
        assert_eq!(self_record.participation, Participation::Enabled);
        stop_test_probe_listener(probe_cancel, probe_task).await;
    }

    #[tokio::test]
    async fn participation_never_overwrites_draining() {
        let (
            store,
            _service,
            network,
            authoritative_self,
            self_record_tx,
            self_id,
            peer_key,
            cancel,
            writer_handle,
        ) = test_runtime(Participation::Draining, Participation::Enabled).await;
        network.set_device_peers(vec![DevicePeer {
            public_key: peer_key,
            endpoint: Some("127.0.0.1:51820".into()),
            last_handshake: Some(tokio::time::Instant::now()),
        }]);

        let mut state = ParticipationState::default();
        let store_driver = StoreDriver::memory_with(store.clone());
        let network_driver = WireguardDriver::memory_with(network);

        for _ in 0..3 {
            participation_once(
                &self_id,
                &authoritative_self,
                &store_driver,
                &network_driver,
                &self_record_tx,
                &mut state,
            )
            .await;
        }

        cancel.cancel();
        writer_handle.await.expect("writer exits");

        let machines = store.list_machines().await.expect("list machines");
        let self_record = machines
            .into_iter()
            .find(|machine| machine.id == self_id)
            .expect("self record");
        assert_eq!(self_record.participation, Participation::Draining);
    }

    #[tokio::test]
    async fn forced_disabled_override_blocks_reenable() {
        let (_probe_guard, probe_cancel, probe_task) = start_test_probe_listener();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let (
            store,
            _service,
            network,
            authoritative_self,
            self_record_tx,
            self_id,
            peer_key,
            cancel,
            writer_handle,
        ) = test_runtime(Participation::Disabled, Participation::Enabled).await;
        network.set_device_peers(vec![DevicePeer {
            public_key: peer_key,
            endpoint: Some("127.0.0.1:51820".into()),
            last_handshake: Some(tokio::time::Instant::now()),
        }]);

        let mut state = ParticipationState {
            forced_participation: Some(Participation::Disabled),
            ..ParticipationState::default()
        };
        let store_driver = StoreDriver::memory_with(store.clone());
        let network_driver = WireguardDriver::memory_with(network);

        for _ in 0..3 {
            participation_once(
                &self_id,
                &authoritative_self,
                &store_driver,
                &network_driver,
                &self_record_tx,
                &mut state,
            )
            .await;
        }

        cancel.cancel();
        writer_handle.await.expect("writer exits");

        let machines = store.list_machines().await.expect("list machines");
        let self_record = machines
            .into_iter()
            .find(|machine| machine.id == self_id)
            .expect("self record");
        assert_eq!(self_record.participation, Participation::Disabled);
        stop_test_probe_listener(probe_cancel, probe_task).await;
    }

    fn start_test_probe_listener() -> (
        MutexGuard<'static, ()>,
        CancellationToken,
        tokio::task::JoinHandle<()>,
    ) {
        static PROBE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let probe_guard = PROBE_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("lock probe listener");
        let probe_cancel = CancellationToken::new();
        let task_cancel = probe_cancel.clone();
        let probe_task = tokio::spawn(async move {
            run_probe_listener_task(task_cancel).await;
        });
        (probe_guard, probe_cancel, probe_task)
    }

    async fn stop_test_probe_listener(
        probe_cancel: CancellationToken,
        probe_task: tokio::task::JoinHandle<()>,
    ) {
        probe_cancel.cancel();
        let _ = probe_task.await;
    }
}
