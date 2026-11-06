//! Which peers' daemons answer, for the Runtime Watch.
//!
//! Corrosion runs in its own container, so a stopped daemon leaves gossip Up and its
//! rows as they last were. While a watch is open, the entry asks each peer it would call
//! a cheap read; a peer that misses `MISSES` probes in a row is named unanswered until it
//! answers again. One prober serves every open stream, since they share one producer.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    time::Duration,
};

use http::uri::PathAndQuery;
use ployz_core::{InspectRequest, MachineId, ManagementAddress, OpaquePayload, Rpc, op};
use tokio::sync::watch;
use tonic::{
    Code,
    codec::ProstCodec,
    transport::{Channel, Endpoint},
};

/// How often each peer is asked.
pub(crate) const PROBE_INTERVAL: Duration = Duration::from_secs(5);
/// How long one probe waits for an answer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Probes in a row a peer must miss before it is named.
const MISSES: u32 = 2;

/// Consecutive misses per peer.
#[derive(Default)]
pub(crate) struct Misses(BTreeMap<MachineId, u32>);

impl Misses {
    /// Record one round over `probed`, where `answered` holds those that answered, and
    /// return the peers that have now missed `MISSES` in a row. A peer no longer probed
    /// is forgotten, so it starts again from zero if it returns.
    pub(crate) fn round(
        &mut self,
        probed: &BTreeSet<MachineId>,
        answered: &BTreeSet<MachineId>,
    ) -> Vec<MachineId> {
        self.0.retain(|machine, _| probed.contains(machine));
        for machine in probed {
            let misses = self.0.entry(*machine).or_default();
            *misses = if answered.contains(machine) {
                0
            } else {
                misses.saturating_add(1)
            };
        }
        self.0
            .iter()
            .filter(|(_, misses)| **misses >= MISSES)
            .map(|(machine, _)| *machine)
            .collect()
    }
}

/// Probe the peers `peers` names every `PROBE_INTERVAL`, publishing the unanswered ones
/// on `unanswered` whenever that list changes. Runs until dropped.
pub(crate) async fn probe_peers<P, F>(
    mut peers: watch::Receiver<Vec<(MachineId, ManagementAddress)>>,
    unanswered: watch::Sender<Vec<MachineId>>,
    interval: Duration,
    answers: P,
) where
    P: Fn(ManagementAddress) -> F,
    F: Future<Output = bool>,
{
    let mut misses = Misses::default();
    let mut ticks = tokio::time::interval(interval);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let round = peers.borrow_and_update().clone();
        let probed: BTreeSet<MachineId> = round.iter().map(|(machine, _)| *machine).collect();
        let replies = futures_util::future::join_all(round.iter().map(|(machine, address)| {
            let reply = answers(*address);
            async move { (*machine, reply.await) }
        }))
        .await;
        let answered = replies
            .into_iter()
            .filter_map(|(machine, answered)| answered.then_some(machine))
            .collect();
        let named = misses.round(&probed, &answered);
        unanswered.send_if_modified(|current| {
            let changed = *current != named;
            if changed {
                current.clone_from(&named);
            }
            changed
        });
    }
}

/// Dials peers' management APIs, keeping one lazy channel per address.
pub(crate) struct Dialer {
    port: u16,
    channels: std::sync::Mutex<BTreeMap<ManagementAddress, Channel>>,
}

impl Dialer {
    pub(crate) fn new(port: u16) -> Self {
        Self {
            port,
            channels: std::sync::Mutex::default(),
        }
    }

    /// Whether the daemon at `address` answers an `Inspect` within `PROBE_TIMEOUT`. Any
    /// reply from it counts, a refusal included; only no reply is a miss.
    pub(crate) async fn answers(&self, address: ManagementAddress) -> bool {
        let Some(channel) = self.channel(address) else {
            return false;
        };
        let Ok(payload) = op::Inspect::into_request(InspectRequest::default()).encode() else {
            return false;
        };
        let call = async {
            let mut grpc = tonic::client::Grpc::new(channel);
            if grpc.ready().await.is_err() {
                return false;
            }
            match grpc
                .unary(
                    tonic::Request::new(payload),
                    PathAndQuery::from_static(op::Inspect::PATH),
                    ProstCodec::<OpaquePayload, OpaquePayload>::default(),
                )
                .await
            {
                Ok(_) => true,
                // The transport's own failures: no daemon replied.
                Err(status) => !matches!(
                    status.code(),
                    Code::Unavailable | Code::Unknown | Code::Cancelled | Code::DeadlineExceeded
                ),
            }
        };
        tokio::time::timeout(PROBE_TIMEOUT, call)
            .await
            .unwrap_or(false)
    }

    fn channel(&self, address: ManagementAddress) -> Option<Channel> {
        let mut channels = self.channels.lock().ok()?;
        if let Some(channel) = channels.get(&address) {
            return Some(channel.clone());
        }
        let channel = Endpoint::from_shared(format!("http://[{}]:{}", address.0, self.port))
            .ok()?
            .connect_timeout(PROBE_TIMEOUT)
            .connect_lazy();
        channels.insert(address, channel.clone());
        Some(channel)
    }
}

#[cfg(test)]
#[path = "runtime_watch_probe_tests.rs"]
mod tests;
