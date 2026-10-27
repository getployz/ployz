//! Path MTU of each mesh peer (#3406). A path that carries small packets but loses large
//! ones looks healthy to every check that only uses small ones: ping, a TCP connect,
//! WireGuard handshakes and Corrosion gossip all pass, while any RPC with a large reply
//! stalls until its caller gives up. This plane sends each peer's management address an
//! ICMPv6 echo the size of the full WireGuard MTU. When it is lost and a small one
//! crosses, it finds the largest size that crosses and the network plane sets that as
//! the MTU of the peer's routes, so the kernel sends smaller packets and advertises a
//! smaller TCP MSS to that peer. It probes the full size again now and then, to raise the
//! MTU back once the path heals.
//!
//! The peer's kernel answers the echo, not its daemon, so a peer on any release takes
//! part. An echo reply is as large as its request, so one round trip tests both ways.
//! No verdict is drawn from silence alone: a peer whose small echo is lost too keeps
//! the MTU it had (bet 8).
//!
//! Bound (bet 3): a Machine-local loop acting only on its own routes. Each round, every
//! [`PROBE_EVERY`], sends each peer one echo of its current MTU. Only a lost one adds
//! more: [`ATTEMPTS`] tries per size, one small echo and a binary search of about five
//! sizes between [`MIN_MTU`] and the MTU. A lowered peer is probed at full size every
//! [`RAISE_EVERY`].

use std::{
    collections::BTreeMap,
    future::Future,
    io,
    net::{Ipv6Addr, SocketAddrV6},
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::future::join_all;
use ployz_core::{LargePackets, MachineId, MachineName, PathObservation};
use rustix::net::{RecvFlags, SendFlags, sockopt::Ipv6PathMtuDiscovery};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use super::WIREGUARD_INTERFACE_NAME;

/// How often each peer's path is probed.
pub(super) const PROBE_EVERY: Duration = Duration::from_secs(60);
/// How often a peer whose MTU was lowered is probed at the full MTU again.
pub(super) const RAISE_EVERY: Duration = Duration::from_secs(10 * 60);
/// The smallest MTU IPv6 allows, and so the lowest the management route can take.
pub(super) const MIN_MTU: u32 = 1280;
/// Echoes sent at one size before it counts as lost.
const ATTEMPTS: usize = 3;
/// How long one echo waits for its reply.
const REPLY_WAIT: Duration = Duration::from_secs(1);
/// The search stops once the carried and lost sizes are this close.
const SEARCH_STEP: u32 = 8;
/// An echo small enough to cross any path that carries anything.
const SMALL: u32 = 128;
const IPV6_HEADER: u32 = 40;
const ICMP_HEADER: u32 = 8;
const ECHO_REQUEST: u8 = 128;
const ECHO_REPLY: u8 = 129;

/// This Machine's latest path observation of each peer, written by the network plane and
/// read by the Machine API.
#[derive(Clone, Debug, Default)]
pub(crate) struct PathBook(Arc<Mutex<BTreeMap<MachineId, PathObservation>>>);

impl PathBook {
    #[must_use]
    pub(crate) fn get(&self, machine_id: &MachineId) -> Option<PathObservation> {
        self.paths().get(machine_id).copied()
    }

    pub(super) fn set(&self, machine_id: MachineId, path: PathObservation) {
        self.paths().insert(machine_id, path);
    }

    /// Forget every peer the mesh no longer has.
    pub(super) fn retain(&self, keep: impl Fn(&MachineId) -> bool) {
        self.paths().retain(|machine_id, _| keep(machine_id));
    }

    fn paths(&self) -> std::sync::MutexGuard<'_, BTreeMap<MachineId, PathObservation>> {
        // A panic while holding the map leaves plain data behind; keep serving it.
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A peer whose path is probed: its management address carries the echoes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ProbedPeer {
    pub(super) machine_id: MachineId,
    pub(super) name: MachineName,
    pub(super) address: Ipv6Addr,
}

/// One peer's path as of the latest round that proved anything.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PathReport {
    pub(super) machine_id: MachineId,
    pub(super) path: PathObservation,
}

/// A round's finding on one path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Finding {
    /// Packets of this size cross; any larger size probed this round did not.
    Carries(u32),
    /// Small packets cross and even [`MIN_MTU`] is lost.
    Dropped,
    /// Not even a small echo came back, so the round proves nothing.
    Silent,
}

/// One round on one path. `passes(size)` says whether an echo of `size` bytes, IPv6
/// header included, came back. `try_full` probes `full` before `current`.
pub(super) async fn check<P, F>(mut passes: P, full: u32, current: u32, try_full: bool) -> Finding
where
    P: FnMut(u32) -> F,
    F: Future<Output = bool>,
{
    if try_full && passes(full).await {
        return Finding::Carries(full);
    }
    if passes(current).await {
        return Finding::Carries(current);
    }
    if !passes(SMALL).await {
        return Finding::Silent;
    }
    if current <= MIN_MTU || !passes(MIN_MTU).await {
        return Finding::Dropped;
    }
    let (mut carried, mut lost) = (MIN_MTU, current);
    while lost - carried > SEARCH_STEP {
        let size = carried + (lost - carried) / 2;
        if passes(size).await {
            carried = size;
        } else {
            lost = size;
        }
    }
    Finding::Carries(carried)
}

/// What the prober holds for one peer between rounds.
#[derive(Clone, Copy, Debug)]
struct PeerState {
    path: Option<PathObservation>,
    /// When a lowered path is next probed at the full MTU.
    raise_at: Instant,
}

impl PeerState {
    fn mtu(&self, full: u32) -> u32 {
        self.path.map_or(full, |path| path.mtu)
    }

    /// Whether this round probes the full MTU of a lowered path. Asking schedules the next.
    fn raise_due(&mut self, full: u32, now: Instant) -> bool {
        let due = self.mtu(full) < full && self.raise_at <= now;
        if due {
            self.raise_at = now + RAISE_EVERY;
        }
        due
    }

    /// Take `finding` in. Silence keeps the path the last round proved.
    fn after(&mut self, finding: Finding, full: u32, now: Instant) {
        let next = match finding {
            Finding::Silent => return,
            Finding::Carries(mtu) => PathObservation {
                mtu,
                large_packets: LargePackets::Carried,
            },
            Finding::Dropped => PathObservation {
                mtu: self.mtu(full),
                large_packets: LargePackets::Dropped,
            },
        };
        if next.mtu < full && self.mtu(full) == full {
            self.raise_at = now + RAISE_EVERY;
        }
        self.path = Some(next);
    }
}

/// Probe every peer's path each [`PROBE_EVERY`] and report each known path to `reports`
/// every round, not only on change: the plane applies a report idempotently, so one it
/// could not place, as during a join, is simply placed by the next round.
pub(super) async fn probe_paths(
    mut peers: watch::Receiver<Vec<ProbedPeer>>,
    full: u32,
    reports: mpsc::Sender<PathReport>,
    shutdown: CancellationToken,
) {
    let mut states = BTreeMap::<MachineId, PeerState>::new();
    let mut ticker = tokio::time::interval(PROBE_EVERY);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => return,
            _ = ticker.tick() => {}
        }
        let round = peers.borrow_and_update().clone();
        states.retain(|machine_id, _| round.iter().any(|peer| peer.machine_id == *machine_id));
        let now = Instant::now();
        let checks = round.iter().map(|peer| {
            let state = states.entry(peer.machine_id).or_insert(PeerState {
                path: None,
                raise_at: now,
            });
            let current = state.mtu(full);
            let try_full = state.raise_due(full, now);
            let address = peer.address;
            async move {
                let finding = check(|size| passes(address, size), full, current, try_full).await;
                (peer, finding)
            }
        });
        for (peer, finding) in join_all(checks).await {
            let Some(state) = states.get_mut(&peer.machine_id) else {
                continue;
            };
            state.after(finding, full, now);
            let Some(path) = state.path else {
                continue;
            };
            let report = PathReport {
                machine_id: peer.machine_id,
                path,
            };
            if reports.send(report).await.is_err() {
                return;
            }
        }
    }
}

/// Whether any of [`ATTEMPTS`] echoes of `size` bytes to `address` came back.
async fn passes(address: Ipv6Addr, size: u32) -> bool {
    for _ in 0..ATTEMPTS {
        match tokio::task::spawn_blocking(move || echo(address, size, REPLY_WAIT)).await {
            Ok(Ok(true)) => return true,
            Ok(Ok(false)) => {}
            Ok(Err(error)) => {
                eprintln!(
                    "path probe to {address} could not send: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
                return false;
            }
            Err(_) => return false,
        }
    }
    false
}

/// Send one ICMPv6 echo of `size` bytes over the WireGuard interface and wait up to `wait`
/// for its reply. The socket probes past the route MTU this plane may have lowered, and
/// never fragments, so the echo crosses at exactly `size` or not at all.
fn echo(address: Ipv6Addr, size: u32, wait: Duration) -> io::Result<bool> {
    let socket = Socket::new(Domain::IPV6, Type::RAW, Some(Protocol::ICMPV6))?;
    socket.bind_device(Some(WIREGUARD_INTERFACE_NAME.as_bytes()))?;
    rustix::net::sockopt::set_ipv6_mtu_discover(&socket, Ipv6PathMtuDiscovery::PROBE)?;
    let identifier = fastrand::u16(..);
    let sequence = fastrand::u16(..);
    let request = echo_request(identifier, sequence, size);
    rustix::net::sendto(
        &socket,
        &request,
        SendFlags::empty(),
        &SocketAddrV6::new(address, 0, 0, 0),
    )?;
    let deadline = std::time::Instant::now() + wait;
    let mut reply = vec![0_u8; request.len().max(64)];
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Ok(false);
        }
        socket.set_read_timeout(Some(left))?;
        let (received, _, from) =
            match rustix::net::recvfrom(&socket, &mut reply[..], RecvFlags::empty()) {
                Ok(received) => received,
                Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            };
        let from = from.and_then(|from| std::net::SocketAddr::try_from(from).ok());
        if from.is_some_and(|from| from.ip() == address)
            && reply
                .get(..received)
                .is_some_and(|packet| is_reply(packet, identifier, sequence))
        {
            return Ok(true);
        }
    }
}

/// An echo request whose IPv6 packet is `size` bytes. The kernel fills in the checksum.
fn echo_request(identifier: u16, sequence: u16, size: u32) -> Vec<u8> {
    let payload = size.saturating_sub(IPV6_HEADER + ICMP_HEADER) as usize;
    let mut packet = Vec::with_capacity(ICMP_HEADER as usize + payload);
    packet.extend_from_slice(&[ECHO_REQUEST, 0, 0, 0]);
    packet.extend_from_slice(&identifier.to_be_bytes());
    packet.extend_from_slice(&sequence.to_be_bytes());
    packet.resize(ICMP_HEADER as usize + payload, 0xa5);
    packet
}

fn is_reply(packet: &[u8], identifier: u16, sequence: u16) -> bool {
    let [kind, _code, _checksum @ .., i0, i1, s0, s1] = packet.get(..8).unwrap_or_default() else {
        return false;
    };
    *kind == ECHO_REPLY
        && [*i0, *i1] == identifier.to_be_bytes()
        && [*s0, *s1] == sequence.to_be_bytes()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    const FULL: u32 = 1420;

    /// Runs `check` against a path that carries packets up to `limit` bytes, or none.
    fn round(limit: Option<u32>, current: u32, try_full: bool) -> (Finding, Vec<u32>) {
        let sent = RefCell::new(Vec::new());
        let finding = futures_util::FutureExt::now_or_never(check(
            |size| {
                sent.borrow_mut().push(size);
                std::future::ready(limit.is_some_and(|limit| size <= limit))
            },
            FULL,
            current,
            try_full,
        ))
        .expect("the scripted probes are ready at once");
        (finding, sent.into_inner())
    }

    #[test]
    fn a_healthy_path_costs_one_echo() {
        assert_eq!(
            round(Some(FULL), FULL, false),
            (Finding::Carries(FULL), vec![FULL])
        );
    }

    #[test]
    fn a_black_hole_finds_the_largest_size_that_crosses() {
        let (finding, sent) = round(Some(1320), FULL, false);
        let Finding::Carries(mtu) = finding else {
            panic!("expected a lowered MTU, got {finding:?}");
        };
        assert!(
            (1320 - SEARCH_STEP..=1320).contains(&mtu),
            "{mtu} is not within one step of 1320"
        );
        assert!(sent.len() <= 9, "the search sent {sent:?}");
    }

    #[test]
    fn silence_draws_no_verdict() {
        assert_eq!(round(None, FULL, false).0, Finding::Silent);
        assert_eq!(round(None, 1320, true).0, Finding::Silent);
    }

    #[test]
    fn a_path_that_loses_even_the_ipv6_minimum_is_dropped() {
        assert_eq!(round(Some(1000), FULL, false).0, Finding::Dropped);
    }

    #[test]
    fn a_healed_path_is_raised_and_a_still_broken_one_kept() {
        assert_eq!(round(Some(FULL), 1320, true).0, Finding::Carries(FULL));
        assert_eq!(
            round(Some(1330), 1320, true),
            (Finding::Carries(1320), vec![FULL, 1320])
        );
    }

    #[test]
    fn a_lowered_path_that_narrows_again_searches_below_its_mtu() {
        let Finding::Carries(mtu) = round(Some(1300), 1320, false).0 else {
            panic!("expected a lower MTU");
        };
        assert!(mtu <= 1300 && mtu > 1300 - SEARCH_STEP, "{mtu}");
    }

    #[test]
    fn state_keeps_the_last_proven_path_and_paces_the_raise() {
        let now = Instant::now();
        let mut state = PeerState {
            path: None,
            raise_at: now,
        };
        let carried = |mtu| PathObservation {
            mtu,
            large_packets: LargePackets::Carried,
        };
        assert!(
            !state.raise_due(FULL, now),
            "a full path has nothing to raise"
        );
        state.after(Finding::Silent, FULL, now);
        assert_eq!(state.path, None);
        state.after(Finding::Carries(FULL), FULL, now);
        assert_eq!(state.path, Some(carried(FULL)));
        state.after(Finding::Carries(1320), FULL, now);
        state.after(Finding::Silent, FULL, now);
        assert_eq!(
            state.path,
            Some(carried(1320)),
            "silence keeps what was proven"
        );
        assert!(!state.raise_due(FULL, now + PROBE_EVERY));
        assert!(state.raise_due(FULL, now + RAISE_EVERY));
        assert!(!state.raise_due(FULL, now + RAISE_EVERY + PROBE_EVERY));
        state.after(Finding::Dropped, FULL, now);
        assert_eq!(
            state.path,
            Some(PathObservation {
                mtu: 1320,
                large_packets: LargePackets::Dropped,
            })
        );
        state.after(Finding::Carries(FULL), FULL, now);
        assert_eq!(state.path, Some(carried(FULL)));
    }

    #[test]
    fn an_echo_request_is_the_size_asked_for_and_its_reply_is_matched() {
        let request = echo_request(7, 9, 1420);
        assert_eq!(request.len() as u32 + IPV6_HEADER, 1420);
        let mut reply = request.clone();
        if let Some(kind) = reply.first_mut() {
            *kind = ECHO_REPLY;
        }
        assert!(is_reply(&reply, 7, 9));
        assert!(!is_reply(&reply, 7, 10));
        assert!(!is_reply(&request, 7, 9));
    }
}
