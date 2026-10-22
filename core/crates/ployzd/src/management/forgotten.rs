//! The forgotten question on [`FORGOTTEN_ALPN`]: a Machine dials a member with its own
//! Management Identity and reads from the close code whether the Cluster forgot it
//! (#2927). No stream opens and no Management Client is needed, so a Server removed
//! while it was unreachable can still ask. The asker proves only that it holds the
//! identity it asks about, and the answer gives away nothing but one bit about that
//! identity, so answering an unknown key is safe.

use std::future::Future;

use iroh::{
    Endpoint, EndpointAddr, PublicKey, RelayUrl,
    endpoint::{Connection, ConnectionError, VarInt},
};
use ployz_core::{FORGOTTEN_ALPN, ForgottenAnswer, LocalMachinePhase, ManagementIdentity};
use tokio::sync::Semaphore;

use crate::corrosion::{Error as CorrosionError, ReplicatedStore};

/// How many askers may hold a store read at once. Anyone holding a key can ask, so
/// the question never draws on the store's shared request gate past this.
const ANSWERS_AT_ONCE: usize = 3;

/// Who this Machine consults for the answer: its view of the Cluster.
pub trait Roster: Send + Sync + 'static {
    /// # Errors
    /// The store could not be read; the asker hears NoAnswer.
    fn forgotten(
        &self,
        identity: ManagementIdentity,
    ) -> impl Future<Output = Result<ForgottenAnswer, CorrosionError>> + Send;
}

impl Roster for ReplicatedStore {
    fn forgotten(
        &self,
        identity: ManagementIdentity,
    ) -> impl Future<Output = Result<ForgottenAnswer, CorrosionError>> + Send {
        ReplicatedStore::forgotten(self, identity)
    }
}

/// A Machine without a replicated store knows nothing either way.
impl<R: Roster> Roster for Option<R> {
    async fn forgotten(
        &self,
        identity: ManagementIdentity,
    ) -> Result<ForgottenAnswer, CorrosionError> {
        match self {
            Some(roster) => roster.forgotten(identity).await,
            None => Ok(ForgottenAnswer::NoAnswer),
        }
    }
}

/// This Machine's side of the question: its roster behind a small gate of its own.
pub(super) struct Answers<R> {
    roster: R,
    permits: Semaphore,
}

impl<R: Roster> Answers<R> {
    pub(super) fn new(roster: R) -> Self {
        Self {
            roster,
            permits: Semaphore::new(ANSWERS_AT_ONCE),
        }
    }

    /// What a Machine in `phase` tells an asker holding `identity`. Only a
    /// Participating Machine's view counts; a full gate or a failed read is NoAnswer.
    pub(super) async fn answer(
        &self,
        phase: LocalMachinePhase,
        identity: ManagementIdentity,
    ) -> ForgottenAnswer {
        if phase != LocalMachinePhase::Participating {
            return ForgottenAnswer::NoAnswer;
        }
        let Ok(_permit) = self.permits.try_acquire() else {
            tracing::debug!("forgotten question not answered: too many askers at once");
            return ForgottenAnswer::NoAnswer;
        };
        match self.roster.forgotten(identity).await {
            Ok(answer) => answer,
            Err(error) => {
                tracing::warn!(
                    error = %ployz_core::error_chain::inline(&error),
                    "forgotten question not answered: could not read the store"
                );
                ForgottenAnswer::NoAnswer
            }
        }
    }
}

/// Answer one connection on [`FORGOTTEN_ALPN`] from its transport key and close it.
pub(super) async fn serve<R: Roster>(
    connection: Connection,
    phase: LocalMachinePhase,
    answers: &Answers<R>,
) {
    let identity = ManagementIdentity::from_bytes(*connection.remote_id().as_bytes());
    let answer = answers.answer(phase, identity).await;
    connection.close(VarInt::from_u32(answer.close_code()), b"forgotten answered");
}

/// Ask `member` whether the Cluster forgot the identity `endpoint` dials with. None
/// when the member cannot be reached, runs a daemon without this ALPN, or closes with
/// a code that is not an answer. The caller bounds how long this may take.
pub async fn ask(
    endpoint: &Endpoint,
    member: ManagementIdentity,
    relay: &RelayUrl,
) -> Option<ForgottenAnswer> {
    let member = PublicKey::from_bytes(member.as_bytes()).ok()?;
    let address = EndpointAddr::new(member).with_relay_url(relay.clone());
    let connection = endpoint.connect(address, FORGOTTEN_ALPN).await.ok()?;
    let ConnectionError::ApplicationClosed(close) = connection.closed().await else {
        return None;
    };
    ForgottenAnswer::from_close_code(u32::try_from(close.error_code.into_inner()).ok()?)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use tokio::sync::watch;

    use super::*;

    /// Answers Forgotten once `released`, counting every read it is asked for.
    struct SlowRoster {
        reads: AtomicUsize,
        released: watch::Receiver<bool>,
    }

    impl Roster for SlowRoster {
        async fn forgotten(
            &self,
            _: ManagementIdentity,
        ) -> Result<ForgottenAnswer, CorrosionError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.released
                .clone()
                .wait_for(|released| *released)
                .await
                .unwrap();
            Ok(ForgottenAnswer::Forgotten)
        }
    }

    struct FailingRoster;

    impl Roster for FailingRoster {
        async fn forgotten(
            &self,
            _: ManagementIdentity,
        ) -> Result<ForgottenAnswer, CorrosionError> {
            Err(CorrosionError::Api("scripted".into()))
        }
    }

    fn identity() -> ManagementIdentity {
        crate::management::ManagementSecret::generate().public_key()
    }

    #[tokio::test]
    async fn only_a_participating_machine_reads_its_roster() {
        let (release, released) = watch::channel(true);
        let answers = Answers::new(SlowRoster {
            reads: AtomicUsize::new(0),
            released,
        });
        for phase in [
            LocalMachinePhase::Uninitialized,
            LocalMachinePhase::Resetting,
        ] {
            assert_eq!(
                answers.answer(phase, identity()).await,
                ForgottenAnswer::NoAnswer
            );
        }
        assert_eq!(answers.roster.reads.load(Ordering::SeqCst), 0);
        assert_eq!(
            answers
                .answer(LocalMachinePhase::Participating, identity())
                .await,
            ForgottenAnswer::Forgotten
        );
        drop(release);
    }

    #[tokio::test]
    async fn a_failed_store_read_is_no_answer() {
        let answers = Answers::new(FailingRoster);
        assert_eq!(
            answers
                .answer(LocalMachinePhase::Participating, identity())
                .await,
            ForgottenAnswer::NoAnswer
        );
    }

    #[tokio::test]
    async fn askers_past_the_gate_hear_no_answer_without_a_store_read() {
        let (release, released) = watch::channel(false);
        let answers = Arc::new(Answers::new(SlowRoster {
            reads: AtomicUsize::new(0),
            released,
        }));
        let held: Vec<_> = (0..ANSWERS_AT_ONCE)
            .map(|_| {
                let answers = Arc::clone(&answers);
                tokio::spawn(async move {
                    answers
                        .answer(LocalMachinePhase::Participating, identity())
                        .await
                })
            })
            .collect();
        while answers.roster.reads.load(Ordering::SeqCst) < ANSWERS_AT_ONCE {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            answers
                .answer(LocalMachinePhase::Participating, identity())
                .await,
            ForgottenAnswer::NoAnswer
        );
        assert_eq!(answers.roster.reads.load(Ordering::SeqCst), ANSWERS_AT_ONCE);
        release.send(true).unwrap();
        for answer in held {
            assert_eq!(answer.await.unwrap(), ForgottenAnswer::Forgotten);
        }
        assert_eq!(
            answers
                .answer(LocalMachinePhase::Participating, identity())
                .await,
            ForgottenAnswer::Forgotten
        );
    }
}
