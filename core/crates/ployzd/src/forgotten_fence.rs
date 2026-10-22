//! A Server removed with `ployz server rm` or `forget` while it was unreachable never
//! hears of it, so its services keep running on its own copy of each Volume (#2927).
//! This plane lets such a Machine fence itself. It watches, per peer, whether
//! Corrosion still hears that peer's gossip over the mesh. Once a peer has been silent
//! for a minute it asks that peer over the management relay whether the Cluster forgot
//! this Machine's Management Identity, and when one says so and none of the asked says
//! it is still listed, it departs as a leave would, keeping every Volume copy on disk.
//!
//! Only silent peers are asked. A peer the mesh still carries is a member this Machine
//! is still in a Cluster with, or a Machine forgotten alongside it whose stale store
//! would still list it, so its word is neither needed nor wanted. A healthy Cluster
//! asks nothing, a Machine with no peers never asks, and silence alone never fences: a
//! partition is not an error. A Listed answer from an asked peer wins over Forgotten,
//! and the fence itself is a leave, so the worst a wrong answer costs is a reset that
//! keeps every copy. A peer that answers is trusted to be a member, which it proves
//! only by its Management Identity being listed in this Machine's own view: a malicious
//! former member that kept its key could tell this Machine it was forgotten, costing it
//! a reset and nothing else. That is accepted for 0.3.1.
//!
//! Bound (bet 3): a Machine-local loop acting only on itself, at most once per daemon
//! life. Each round is one read of the local Corrosion admin socket. An ask is one QUIC
//! handshake per due peer, bounded by [`ASK_TIMEOUT`] and backed off per peer from
//! [`ASK_BACKOFF_MIN`] to [`ASK_BACKOFF_MAX`] while that peer stays silent.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    time::Duration,
};

use futures_util::future::join_all;
use iroh::{Endpoint, RelayUrl};
use ployz_core::{ForgottenAnswer, MachineId, MachineName, ManagementIdentity, MembershipEvidence};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    corrosion::{AdminClient, MachineView, membership_states_by_address},
    machine::{LocalMachine, LocalMachineError},
    management::forgotten,
};

/// How often peer liveness is read.
pub(crate) const MESH_PROBE_EVERY: Duration = Duration::from_secs(30);
/// How long one peer stays silent before it is asked.
pub(crate) const SILENCE: Duration = Duration::from_secs(60);
/// How long one peer is given to answer.
pub(crate) const ASK_TIMEOUT: Duration = Duration::from_secs(10);
/// Gap between asks of one peer while it stays silent, doubling each time.
pub(crate) const ASK_BACKOFF_MIN: Duration = Duration::from_secs(60);
pub(crate) const ASK_BACKOFF_MAX: Duration = Duration::from_secs(15 * 60);
const MEMBERSHIP_TIMEOUT: Duration = Duration::from_secs(2);

/// One other Machine in this Machine's view, as the mesh carries it right now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Peer {
    pub(crate) id: MachineId,
    pub(crate) name: MachineName,
    /// None when its daemon never published one; such a peer cannot be asked.
    pub(crate) identity: Option<ManagementIdentity>,
    /// Corrosion membership reports it Up.
    pub(crate) heard: bool,
}

/// One peer's answer; `None` when it could not be asked or did not answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Asked {
    pub(crate) peer: MachineName,
    pub(crate) answer: Option<ForgottenAnswer>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Verdict {
    Fence { told_by: MachineName },
    Stay,
}

pub(crate) trait FenceHost: Send + Sync {
    /// Every other Machine in this Machine's view. None when liveness cannot be read
    /// this round, which changes nothing.
    fn peers(&self) -> impl Future<Output = Option<Vec<Peer>>> + Send;
    /// Ask these peers, in parallel.
    fn ask(&self, peers: &[Peer]) -> impl Future<Output = Vec<Asked>> + Send;
    /// Depart keeping every copy. Err names why it did not happen; it is retried.
    fn fence(&self) -> impl Future<Output = Result<(), LocalMachineError>> + Send;
}

/// Fence when a peer says Forgotten and no peer says Listed.
pub(crate) fn verdict(asked: &[Asked]) -> Verdict {
    if asked
        .iter()
        .any(|asked| asked.answer == Some(ForgottenAnswer::Listed))
    {
        return Verdict::Stay;
    }
    asked
        .iter()
        .find(|asked| asked.answer == Some(ForgottenAnswer::Forgotten))
        .map_or(Verdict::Stay, |asked| Verdict::Fence {
            told_by: asked.peer.clone(),
        })
}

/// When a silent peer may next be asked, and the gap to the ask after that.
#[derive(Clone, Copy)]
struct Silence {
    ask_at: Instant,
    backoff: Duration,
}

/// Watch the peers until `shutdown`, or until this Machine fenced itself.
pub(crate) async fn run<H: FenceHost>(host: &H, shutdown: CancellationToken) {
    let mut silence: BTreeMap<MachineId, Silence> = BTreeMap::new();
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(MESH_PROBE_EVERY) => {}
        }
        let Some(peers) = host.peers().await else {
            continue;
        };
        let now = Instant::now();
        let silent: BTreeSet<MachineId> = peers
            .iter()
            .filter(|peer| !peer.heard)
            .map(|peer| peer.id)
            .collect();
        silence.retain(|id, _| silent.contains(id));
        for id in &silent {
            silence.entry(*id).or_insert(Silence {
                ask_at: now + SILENCE,
                backoff: ASK_BACKOFF_MIN,
            });
        }
        let due: Vec<Peer> = peers
            .into_iter()
            .filter(|peer| peer.identity.is_some())
            .filter(|peer| silence.get(&peer.id).is_some_and(|peer| peer.ask_at <= now))
            .collect();
        if due.is_empty() {
            continue;
        }
        for peer in &due {
            if let Some(silence) = silence.get_mut(&peer.id) {
                silence.ask_at = now + silence.backoff;
                silence.backoff = (silence.backoff * 2).min(ASK_BACKOFF_MAX);
            }
        }
        let asked = host.ask(&due).await;
        let Verdict::Fence { told_by } = verdict(&asked) else {
            tracing::debug!(
                ?asked,
                "silent peers were asked and none says this Machine was forgotten"
            );
            continue;
        };
        tracing::warn!(%told_by, "this Machine was forgotten by its Cluster; fencing and keeping every Volume copy");
        match host.fence().await {
            Ok(()) => return,
            Err(error) => tracing::warn!(
                error = %ployz_core::error_chain::inline(&error),
                "the fence did not happen; retrying"
            ),
        }
    }
}

pub(crate) struct FenceDaemon {
    pub(crate) machine_id: MachineId,
    pub(crate) machine: LocalMachine,
    pub(crate) machines: MachineView,
    pub(crate) admin: AdminClient,
    /// The daemon's own management endpoint, so the question travels under this
    /// Machine's Management Identity.
    pub(crate) endpoint: Endpoint,
    pub(crate) relay: RelayUrl,
}

impl FenceHost for FenceDaemon {
    async fn peers(&self) -> Option<Vec<Peer>> {
        let machines = self.machines.current()?;
        let states = match tokio::time::timeout(MEMBERSHIP_TIMEOUT, self.admin.membership_states())
            .await
        {
            Ok(Ok(states)) => membership_states_by_address(states),
            Ok(Err(error)) => {
                tracing::debug!(error = %ployz_core::error_chain::inline(&error), "could not read membership");
                return None;
            }
            Err(_) => {
                tracing::debug!("timed out reading membership");
                return None;
            }
        };
        Some(
            machines
                .observations
                .iter()
                .filter(|peer| peer.id != self.machine_id)
                .map(|peer| Peer {
                    id: peer.id,
                    name: peer.name.clone(),
                    identity: peer.runtime.management_identity,
                    heard: states.get(&peer.management_address()) == Some(&MembershipEvidence::Up),
                })
                .collect(),
        )
    }

    async fn ask(&self, peers: &[Peer]) -> Vec<Asked> {
        let askable = peers
            .iter()
            .filter_map(|peer| peer.identity.map(|identity| (peer.name.clone(), identity)));
        join_all(askable.map(|(peer, identity)| async move {
            let answer = tokio::time::timeout(
                ASK_TIMEOUT,
                forgotten::ask(&self.endpoint, identity, &self.relay),
            )
            .await
            .ok()
            .flatten();
            Asked { peer, answer }
        }))
        .await
    }

    async fn fence(&self) -> Result<(), LocalMachineError> {
        self.machine.fence_forgotten().await
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use super::*;

    fn name(name: &str) -> MachineName {
        MachineName::parse(name).unwrap()
    }

    fn asked(peer: &str, answer: Option<ForgottenAnswer>) -> Asked {
        Asked {
            peer: name(peer),
            answer,
        }
    }

    /// A peer named by one hex digit, which also makes its id.
    fn peer(digit: &str, heard: bool) -> Peer {
        Peer {
            id: MachineId::parse(digit.repeat(32)).unwrap(),
            name: name(digit),
            identity: Some(crate::management::ManagementSecret::generate().public_key()),
            heard,
        }
    }

    fn nameless(digit: &str, heard: bool) -> Peer {
        Peer {
            identity: None,
            ..peer(digit, heard)
        }
    }

    #[test]
    fn a_forgotten_answer_fences_unless_a_peer_still_lists_this_machine() {
        use ForgottenAnswer::{Forgotten, Listed, NoAnswer};
        let fence = |by: &str| Verdict::Fence { told_by: name(by) };
        let cases = [
            (vec![], Verdict::Stay),
            (vec![asked("a", None)], Verdict::Stay),
            (vec![asked("a", Some(NoAnswer))], Verdict::Stay),
            (vec![asked("a", Some(Forgotten))], fence("a")),
            (
                vec![asked("a", None), asked("b", Some(Forgotten))],
                fence("b"),
            ),
            (
                vec![asked("a", Some(Forgotten)), asked("b", Some(Listed))],
                Verdict::Stay,
            ),
            (
                vec![asked("a", Some(Listed)), asked("b", Some(Forgotten))],
                Verdict::Stay,
            ),
        ];
        for (answers, expected) in cases {
            assert_eq!(verdict(&answers), expected, "{answers:?}");
        }
    }

    /// Scripted rounds and fence results, each queue repeating its last entry, and a
    /// fixed answer per peer name.
    struct FakeHost {
        rounds: Mutex<VecDeque<Option<Vec<Peer>>>>,
        answers: BTreeMap<MachineName, ForgottenAnswer>,
        fences: Mutex<VecDeque<Result<(), ()>>>,
        /// When each ask happened and whom it asked.
        asks: Mutex<Vec<(Duration, Vec<MachineName>)>>,
        fenced: Mutex<Vec<Duration>>,
        /// When a round reported some peer heard.
        heard: Mutex<Vec<Duration>>,
        started: Instant,
    }

    impl FakeHost {
        fn new(rounds: impl IntoIterator<Item = Option<Vec<Peer>>>) -> Self {
            Self {
                rounds: Mutex::new(rounds.into_iter().collect()),
                answers: BTreeMap::new(),
                fences: Mutex::new(VecDeque::from([Ok(())])),
                asks: Mutex::default(),
                fenced: Mutex::default(),
                heard: Mutex::default(),
                started: Instant::now(),
            }
        }

        fn answering(
            mut self,
            answers: impl IntoIterator<Item = (&'static str, ForgottenAnswer)>,
        ) -> Self {
            self.answers = answers
                .into_iter()
                .map(|(peer, answer)| (name(peer), answer))
                .collect();
            self
        }

        fn fencing(self, fences: impl IntoIterator<Item = Result<(), ()>>) -> Self {
            *self.fences.lock().unwrap() = fences.into_iter().collect();
            self
        }

        fn elapsed(&self) -> Duration {
            self.started.elapsed()
        }

        fn ask_times(&self) -> Vec<Duration> {
            self.asks
                .lock()
                .unwrap()
                .iter()
                .map(|(at, _)| *at)
                .collect()
        }

        fn asked_names(&self) -> BTreeSet<MachineName> {
            self.asks
                .lock()
                .unwrap()
                .iter()
                .flat_map(|(_, names)| names.clone())
                .collect()
        }
    }

    fn next<T: Clone>(queue: &Mutex<VecDeque<T>>) -> T {
        let mut queue = queue.lock().unwrap();
        if queue.len() > 1 {
            queue.pop_front().unwrap()
        } else {
            queue.front().cloned().expect("a scripted value")
        }
    }

    impl FenceHost for FakeHost {
        async fn peers(&self) -> Option<Vec<Peer>> {
            let round = next(&self.rounds);
            if round
                .as_ref()
                .is_some_and(|peers| peers.iter().any(|peer| peer.heard))
            {
                self.heard.lock().unwrap().push(self.elapsed());
            }
            round
        }

        async fn ask(&self, peers: &[Peer]) -> Vec<Asked> {
            let names = peers.iter().map(|peer| peer.name.clone()).collect();
            self.asks.lock().unwrap().push((self.elapsed(), names));
            peers
                .iter()
                .map(|peer| Asked {
                    peer: peer.name.clone(),
                    answer: self.answers.get(&peer.name).copied(),
                })
                .collect()
        }

        async fn fence(&self) -> Result<(), LocalMachineError> {
            self.fenced.lock().unwrap().push(self.elapsed());
            next(&self.fences).map_err(|()| LocalMachineError::Cleanup("scripted".into()))
        }
    }

    /// Runs the loop for `budget` of paused time; true when it returned by itself.
    async fn run_for(host: &FakeHost, budget: Duration) -> bool {
        tokio::time::timeout(budget, run(host, CancellationToken::new()))
            .await
            .is_ok()
    }

    fn gaps(times: &[Duration]) -> Vec<Duration> {
        times
            .iter()
            .zip(times.iter().skip(1))
            .map(|(earlier, later)| *later - *earlier)
            .collect()
    }

    fn never_shrink(gaps: &[Duration]) -> bool {
        gaps.iter()
            .zip(gaps.iter().skip(1))
            .all(|(earlier, later)| earlier <= later)
    }

    #[tokio::test(start_paused = true)]
    async fn a_silence_shorter_than_a_minute_asks_nothing() {
        let silent = std::iter::repeat_n(
            Some(vec![peer("a", false)]),
            SILENCE.as_secs() as usize / MESH_PROBE_EVERY.as_secs() as usize,
        );
        let host = FakeHost::new(silent.chain([Some(vec![peer("a", true)])]));
        assert!(!run_for(&host, Duration::from_secs(3600)).await);
        assert!(host.ask_times().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_machine_with_no_peers_never_asks() {
        let host = FakeHost::new([Some(vec![])]);
        assert!(!run_for(&host, Duration::from_secs(3600)).await);
        assert!(host.ask_times().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn an_unreadable_round_neither_asks_nor_forgets_a_silence() {
        let host = FakeHost::new([Some(vec![peer("a", false)]), None, None, None, None])
            .answering([("a", ForgottenAnswer::Forgotten)]);
        assert!(!run_for(&host, Duration::from_secs(3600)).await);
        assert!(host.ask_times().is_empty());

        let host = FakeHost::new([
            Some(vec![peer("a", false)]),
            None,
            None,
            Some(vec![peer("a", false)]),
        ])
        .answering([("a", ForgottenAnswer::Forgotten)]);
        assert!(run_for(&host, Duration::from_secs(3600)).await);
        let asks = host.ask_times();
        let [ask] = asks.as_slice() else {
            panic!("one ask, got {asks:?}");
        };
        assert!(*ask >= MESH_PROBE_EVERY + SILENCE, "{asks:?}");
    }

    /// Two Servers forgotten together still hear each other, and each one's stale store
    /// would still list the other. The heard one is never asked, so its Listed cannot
    /// veto the member's Forgotten.
    #[tokio::test(start_paused = true)]
    async fn only_silent_peers_are_asked_so_a_co_forgotten_peer_cannot_veto() {
        let host = FakeHost::new([Some(vec![peer("a", false), peer("c", true)])]).answering([
            ("a", ForgottenAnswer::Forgotten),
            ("c", ForgottenAnswer::Listed),
        ]);
        assert!(run_for(&host, Duration::from_secs(3600)).await);
        assert_eq!(host.asked_names(), BTreeSet::from([name("a")]));
        assert_eq!(host.fenced.lock().unwrap().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_listed_answer_from_an_asked_peer_keeps_this_machine() {
        let host = FakeHost::new([Some(vec![peer("a", false), peer("b", false)])]).answering([
            ("a", ForgottenAnswer::Forgotten),
            ("b", ForgottenAnswer::Listed),
        ]);
        assert!(!run_for(&host, Duration::from_secs(3600)).await);
        assert!(!host.ask_times().is_empty());
        assert!(host.fenced.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_peer_without_an_identity_is_never_asked() {
        let host = FakeHost::new([Some(vec![nameless("b", false)])]);
        assert!(!run_for(&host, Duration::from_secs(3600)).await);
        assert!(host.ask_times().is_empty());

        let host = FakeHost::new([Some(vec![peer("a", false), nameless("b", false)])])
            .answering([("a", ForgottenAnswer::NoAnswer)]);
        assert!(!run_for(&host, Duration::from_secs(3600)).await);
        assert_eq!(host.asked_names(), BTreeSet::from([name("a")]));
    }

    #[tokio::test(start_paused = true)]
    async fn asks_of_a_peer_that_stays_silent_never_get_closer_and_keep_coming() {
        let host = FakeHost::new([Some(vec![peer("a", false)])])
            .answering([("a", ForgottenAnswer::NoAnswer)]);
        let budget = Duration::from_secs(4 * 3600);
        assert!(!run_for(&host, budget).await);
        let asks = host.ask_times();
        assert!(*asks.first().unwrap() >= SILENCE, "{asks:?}");
        let gaps = gaps(&asks);
        assert!(never_shrink(&gaps), "{gaps:?}");
        assert!(
            gaps.iter()
                .all(|gap| *gap <= ASK_BACKOFF_MAX + MESH_PROBE_EVERY),
            "{gaps:?}"
        );
        assert!(
            budget - *asks.last().unwrap() <= ASK_BACKOFF_MAX + MESH_PROBE_EVERY,
            "{asks:?}"
        );
        assert!(host.fenced.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn hearing_a_peer_again_restarts_its_silence_and_its_backoff() {
        let silent = || Some(vec![peer("a", false)]);
        let rounds =
            std::iter::repeat_n(silent(), 12).chain([Some(vec![peer("a", true)]), silent()]);
        let host = FakeHost::new(rounds).answering([("a", ForgottenAnswer::NoAnswer)]);
        assert!(!run_for(&host, Duration::from_secs(2 * 3600)).await);
        let heard_at = *host.heard.lock().unwrap().first().unwrap();
        let asks = host.ask_times();
        let (before, after): (Vec<_>, Vec<_>) = asks.iter().partition(|at| **at < heard_at);
        assert!(
            before.len() >= 3 && after.len() >= 2,
            "{asks:?} heard at {heard_at:?}"
        );
        assert!(
            *after.first().unwrap() >= heard_at + SILENCE,
            "{asks:?} heard at {heard_at:?}"
        );
        let before = gaps(&before);
        let after = gaps(&after);
        assert!(before.last() > before.first(), "{before:?}");
        assert_eq!(after.first(), before.first(), "{after:?} vs {before:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_forgotten_answer_fences_and_the_loop_ends() {
        let host = FakeHost::new([Some(vec![peer("a", false), peer("b", false)])])
            .answering([("b", ForgottenAnswer::Forgotten)]);
        assert!(run_for(&host, Duration::from_secs(3600)).await);
        assert_eq!(host.ask_times().len(), 1);
        assert_eq!(*host.fenced.lock().unwrap(), host.ask_times());
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_fence_is_asked_and_tried_again() {
        let host = FakeHost::new([Some(vec![peer("a", false)])])
            .answering([("a", ForgottenAnswer::Forgotten)])
            .fencing([Err(()), Ok(())]);
        assert!(run_for(&host, Duration::from_secs(3600)).await);
        let asks = host.ask_times();
        let [first, second] = asks.as_slice() else {
            panic!("two asks, got {asks:?}");
        };
        assert!(second > first);
        assert_eq!(*host.fenced.lock().unwrap(), asks);
    }
}
