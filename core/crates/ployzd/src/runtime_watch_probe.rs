//! Which peers' daemons answer, for the Runtime Watch.
//!
//! Corrosion runs in its own container, so a stopped daemon leaves gossip Up and its
//! rows as they last were. While a watch is open, the entry asks each peer it would call
//! a cheap read; a peer that misses `MISSES` probes in a row, an interval apart, is named
//! unanswered until it answers again. One prober serves every open stream, since they
//! share one producer.

use std::{collections::BTreeMap, future::Future, time::Duration};

use ployz_core::{DescribeContractRequest, MachineId, MachineRpcClient, ManagementAddress, op};
use tokio::{sync::watch, time::Instant};
use tonic::transport::{Channel, Endpoint};

/// How often each peer is asked.
pub(crate) const PROBE_INTERVAL: Duration = Duration::from_secs(5);
/// How long one probe waits for an answer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Probes in a row a peer must miss before it is named.
const MISSES: u32 = 2;

/// Each probed peer's address and its run of counted misses.
#[derive(Default)]
pub(crate) struct Misses(BTreeMap<MachineId, Run>);

struct Run {
    address: ManagementAddress,
    misses: u32,
    counted: Option<Instant>,
}

impl Misses {
    /// Record one round of `replies`, each a peer, the address probed and whether it
    /// answered, made at `at`. Returns the peers that have now missed `MISSES` probes in a
    /// row. A miss counts only once `interval` has passed since that peer's last counted
    /// one, so a round brought forward by a new peer list can't hurry a name. A peer not
    /// in `replies`, or probed at a new address, starts again from zero.
    pub(crate) fn round(
        &mut self,
        replies: &[(MachineId, ManagementAddress, bool)],
        at: Instant,
        interval: Duration,
    ) -> Vec<MachineId> {
        self.0.retain(|machine, run| {
            replies
                .iter()
                .any(|(probed, address, _)| probed == machine && *address == run.address)
        });
        for (machine, address, answered) in replies {
            let run = self.0.entry(*machine).or_insert(Run {
                address: *address,
                misses: 0,
                counted: None,
            });
            if *answered {
                run.misses = 0;
                run.counted = None;
            } else if run
                .counted
                .is_none_or(|counted| at.saturating_duration_since(counted) >= interval)
            {
                run.misses = run.misses.saturating_add(1);
                run.counted = Some(at);
            }
        }
        self.0
            .iter()
            .filter(|(_, run)| run.misses >= MISSES)
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
        // A new peer list is probed at once, so a watch's first answer needn't wait a tick.
        let at = tokio::select! {
            at = ticks.tick() => at,
            Ok(()) = peers.changed() => {
                let at = Instant::now();
                ticks.reset();
                at
            }
        };
        let round = peers.borrow_and_update().clone();
        let replies = futures_util::future::join_all(round.iter().map(|(machine, address)| {
            let reply = answers(*address);
            async move { (*machine, *address, reply.await) }
        }))
        .await;
        let named = misses.round(&replies, at, interval);
        unanswered.send_if_modified(|current| {
            let changed = *current != named;
            if changed {
                current.clone_from(&named);
            }
            changed
        });
    }
}

/// Dials peers' Machine APIs over the mesh, keeping one lazy channel per address until a
/// probe on it misses.
pub(crate) struct Dialer {
    port: u16,
    timeout: Duration,
    channels: std::sync::Mutex<BTreeMap<ManagementAddress, Channel>>,
}

impl Dialer {
    pub(crate) fn new(port: u16) -> Self {
        Self::with_timeout(port, PROBE_TIMEOUT)
    }

    fn with_timeout(port: u16, timeout: Duration) -> Self {
        Self {
            port,
            timeout,
            channels: std::sync::Mutex::default(),
        }
    }

    /// Whether the daemon at `address` replies to a `DescribeContract` within the
    /// timeout. Any reply from it counts, a refusal included; only a call that never
    /// reached a daemon, or no reply in time, is a miss.
    pub(crate) async fn answers(&self, address: ManagementAddress) -> bool {
        let Some(channel) = self.channel(address) else {
            return false;
        };
        let Ok(payload) = op::DescribeContract::into_request(DescribeContractRequest {}).encode()
        else {
            return false;
        };
        let call = async {
            match MachineRpcClient::new(channel)
                .describe_contract(payload)
                .await
            {
                Ok(_) => true,
                // A status the daemon sent carries no source; the client's own
                // transport failures (refused, reset, unreachable) do.
                Err(status) if std::error::Error::source(&status).is_none() => true,
                Err(status) => {
                    tracing::debug!(address = %address.0, %status, "runtime watch probe missed");
                    false
                }
            }
        };
        let answered = tokio::time::timeout(self.timeout, call)
            .await
            .unwrap_or_else(|_| {
                tracing::debug!(address = %address.0, "runtime watch probe timed out");
                false
            });
        if !answered {
            // A connection that stalled stays stalled behind its retransmits; dial afresh.
            self.forget(address);
        }
        answered
    }

    fn forget(&self, address: ManagementAddress) {
        if let Ok(mut channels) = self.channels.lock() {
            channels.remove(&address);
        }
    }

    fn channel(&self, address: ManagementAddress) -> Option<Channel> {
        let mut channels = self.channels.lock().ok()?;
        if let Some(channel) = channels.get(&address) {
            return Some(channel.clone());
        }
        let channel = Endpoint::from_shared(format!("http://[{}]:{}", address.0, self.port))
            .ok()?
            .connect_timeout(self.timeout)
            .connect_lazy();
        channels.insert(address, channel.clone());
        Some(channel)
    }
}

#[cfg(test)]
#[path = "runtime_watch_probe_tests.rs"]
mod tests;
