//! How a build handed to GitHub ends: what Cloud's look at it settles, and which runs the
//! Store lets end it.
//!
//! `github_finish` re-reads the build, then asks [`first`] whether the grant must be read on
//! the Machine that minted it, and [`after_read`] what that read ends the build with. The
//! Store's `github_end` asks [`github_end_holds`] whether the run that ends it still holds it.
//! The sentences here are the ones the build's log and message record.

use ployz_core::ImageDigest;
use serde::{Deserialize, Serialize};

/// Which of Cloud's looks at a GitHub build calls `github_finish`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubLook {
    /// No run checked in by the time another Builder could take the build. It passes
    /// on only a run that still hasn't: one that checked in meanwhile started in time.
    StartLimit,
    /// The run ran past its budget, and Cloud cancelled it.
    Budget,
    /// The run reported its end, or GitHub says it completed.
    Ended,
}

impl GithubLook {
    /// Whether the look is one that ends a run for running out of time.
    fn timed_out(self) -> bool {
        match self {
            Self::StartLimit | Self::Budget => true,
            Self::Ended => false,
        }
    }
}

/// What reading a grant on the Machine that minted it found.
#[derive(Debug, Eq, PartialEq)]
pub enum GrantRead {
    /// What it received, if anything. A grant that expired or whose Machine restarted
    /// received nothing more.
    Pushed(Option<ImageDigest>),
    /// The Machine cannot read grants, or belongs to another Cluster; the build fails
    /// with this sentence.
    Refused(String),
    /// The Machine can't be reached now.
    Unreachable,
}

/// How a run reported its end. `B` is what the caller keeps of a built run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EndedRun<B> {
    Built(B),
    Failed,
}

/// Why a build failed or was passed on, as the sentence it records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Why {
    RanOutOfTime,
    EndedWithoutPushing,
    NoRunnerInTime,
    EndedBeforeStart,
    StepFailed,
    Unreachable,
    /// The Machine's own refusal.
    Refused(String),
}

impl Why {
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::RanOutOfTime => "the run ran out of time",
            Self::EndedWithoutPushing => "the run ended without pushing an image",
            Self::NoRunnerInTime => "no runner started the build in time",
            Self::EndedBeforeStart => "the run ended before it started the build",
            Self::StepFailed => "a build step failed",
            Self::Unreachable => "your Machine couldn't be reached to confirm the push",
            Self::Refused(message) => message,
        }
    }
}

/// How a look ends a build.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Ending<B> {
    Failed(Why),
    /// GitHub ran out or stopped for its own reasons: the next Builder starts the build.
    Skipped(Why),
    /// The Machine received `pushed` and the run reported Built.
    Built {
        pushed: ImageDigest,
        run: B,
    },
}

/// What a look does first, with what the re-read of the build found.
#[derive(Debug, Eq, PartialEq)]
pub enum First<G> {
    /// The run never checked in: the next Builder starts the build afresh. The Store
    /// refuses this end for a run that checked in meanwhile, which started in time, so
    /// the next look finds it started.
    Unstarted(Why),
    /// Read the grant on its Machine, then ask [`after_read`].
    ReadGrant(G),
}

/// What `look` does with a build whose run checked in with `grant`, if it did.
///
/// The start limit ends only a run that never checked in, whatever the re-read found: the
/// Store's Unstarted refuses a run whose check-in committed.
#[must_use]
pub fn first<G>(grant: Option<G>, look: GithubLook) -> First<G> {
    match grant {
        Some(grant) if look != GithubLook::StartLimit => First::ReadGrant(grant),
        _ => First::Unstarted(if look == GithubLook::Ended {
            Why::EndedBeforeStart
        } else {
            Why::NoRunnerInTime
        }),
    }
}

/// What `look` ends a build with, once the grant was read, or `None` while its Machine
/// can't be reached and the run only ended: the next look reads it again.
#[must_use]
pub fn after_read<B>(
    ended: Option<EndedRun<B>>,
    look: GithubLook,
    read: GrantRead,
) -> Option<Ending<B>> {
    let pushed = match read {
        GrantRead::Pushed(pushed) => pushed,
        GrantRead::Refused(message) => return Some(Ending::Failed(Why::Refused(message))),
        GrantRead::Unreachable if !look.timed_out() => return None,
        GrantRead::Unreachable => return Some(Ending::Failed(Why::Unreachable)),
    };
    Some(match (pushed, ended) {
        (Some(pushed), Some(EndedRun::Built(run))) => Ending::Built { pushed, run },
        (_, Some(EndedRun::Failed)) => Ending::Failed(Why::StepFailed),
        _ if look == GithubLook::Budget => Ending::Skipped(Why::RanOutOfTime),
        _ => Ending::Skipped(Why::EndedWithoutPushing),
    })
}

/// How far the run that holds a build got.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Hold {
    /// It has not checked in.
    Dispatched,
    CheckedIn,
    /// Its final report came.
    Reported,
}

/// The kind of end the Store is asked to record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndKind {
    Built,
    Failed,
    Skipped,
    Unstarted,
}

/// Who the Store is asked to end a build for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    /// A run, which holds the build now as `building_on` says, if it does.
    Run { building_on: Option<Hold> },
    /// No run: before GitHub took it.
    BeforeGithub {
        /// The build waits for a Builder and no earlier end passed it on.
        waiting_unskipped: bool,
    },
}

/// Whether the Store lets `kind` end the build for `target`. A stale run must not end
/// it, and an Unstarted end refuses a run that checked in.
#[must_use]
pub fn github_end_holds(kind: EndKind, target: Target) -> bool {
    match (target, kind) {
        (Target::Run { building_on }, EndKind::Unstarted) => building_on == Some(Hold::Dispatched),
        (Target::Run { building_on }, _) => building_on.is_some(),
        (Target::BeforeGithub { waiting_unskipped }, _) => waiting_unskipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest() -> ImageDigest {
        ImageDigest::parse(format!("sha256:{}", "a".repeat(64))).expect("a digest")
    }

    const LOOKS: [GithubLook; 3] = [
        GithubLook::StartLimit,
        GithubLook::Budget,
        GithubLook::Ended,
    ];

    #[test]
    fn looks_keep_the_strings_cloud_sends() {
        let json: Vec<String> = LOOKS
            .iter()
            .map(|look| serde_json::to_string(look).expect("a look is JSON"))
            .collect();
        assert_eq!(json, [r#""start_limit""#, r#""budget""#, r#""ended""#]);
    }

    #[test]
    fn a_run_that_never_checked_in_is_unstarted_under_every_look() {
        let sentences: Vec<_> = LOOKS
            .iter()
            .map(|look| match first::<()>(None, *look) {
                First::Unstarted(why) => why.message().to_owned(),
                First::ReadGrant(()) => panic!("a grant was read"),
            })
            .collect();
        assert_eq!(
            sentences,
            [
                "no runner started the build in time",
                "no runner started the build in time",
                "the run ended before it started the build",
            ]
        );
    }

    #[test]
    fn the_start_limit_ends_only_a_run_that_never_checked_in() {
        assert_eq!(
            first(Some(()), GithubLook::StartLimit),
            First::Unstarted(Why::NoRunnerInTime)
        );
        assert_eq!(first(Some(()), GithubLook::Budget), First::ReadGrant(()));
        assert_eq!(first(Some(()), GithubLook::Ended), First::ReadGrant(()));
    }

    #[test]
    fn a_refused_read_fails_the_build_with_the_refusal() {
        for look in [GithubLook::Budget, GithubLook::Ended] {
            let ending = after_read::<()>(None, look, GrantRead::Refused("another Cluster".into()));
            assert_eq!(
                ending,
                Some(Ending::Failed(Why::Refused("another Cluster".into())))
            );
        }
    }

    #[test]
    fn an_unreachable_machine_waits_for_a_run_that_ended_and_fails_one_out_of_time() {
        assert_eq!(
            after_read::<()>(None, GithubLook::Ended, GrantRead::Unreachable),
            None
        );
        let ending = after_read::<()>(None, GithubLook::Budget, GrantRead::Unreachable);
        let Some(Ending::Failed(why)) = ending else {
            panic!("{ending:?}");
        };
        assert_eq!(
            why.message(),
            "your Machine couldn't be reached to confirm the push"
        );
    }

    #[test]
    fn a_read_settles_by_what_the_run_reported_and_the_machine_received() {
        let ended = |ended: Option<EndedRun<u8>>, look, pushed| {
            after_read(ended, look, GrantRead::Pushed(pushed)).map(|ending| match ending {
                Ending::Built { run, .. } => format!("Built {run}"),
                Ending::Failed(why) => format!("Failed {}", why.message()),
                Ending::Skipped(why) => format!("Skipped {}", why.message()),
            })
        };
        let built = || Some(EndedRun::Built(7));
        let found = [
            ended(built(), GithubLook::Ended, Some(digest())),
            ended(built(), GithubLook::Budget, Some(digest())),
            ended(built(), GithubLook::Ended, None),
            ended(Some(EndedRun::Failed), GithubLook::Ended, Some(digest())),
            ended(Some(EndedRun::Failed), GithubLook::Budget, None),
            ended(None, GithubLook::Budget, Some(digest())),
            ended(None, GithubLook::Ended, Some(digest())),
            ended(None, GithubLook::Ended, None),
        ];
        let found: Vec<_> = found.into_iter().map(Option::unwrap).collect();
        assert_eq!(
            found,
            [
                "Built 7",
                "Built 7",
                "Skipped the run ended without pushing an image",
                "Failed a build step failed",
                "Failed a build step failed",
                "Skipped the run ran out of time",
                "Skipped the run ended without pushing an image",
                "Skipped the run ended without pushing an image",
            ]
        );
    }

    #[test]
    fn only_a_run_that_has_not_checked_in_can_be_ended_unstarted() {
        let holds = |kind, building_on| github_end_holds(kind, Target::Run { building_on });
        for hold in [Hold::Dispatched, Hold::CheckedIn, Hold::Reported] {
            assert_eq!(
                holds(EndKind::Unstarted, Some(hold)),
                hold == Hold::Dispatched
            );
            for kind in [EndKind::Built, EndKind::Failed, EndKind::Skipped] {
                assert!(holds(kind, Some(hold)));
            }
        }
        for kind in [
            EndKind::Built,
            EndKind::Failed,
            EndKind::Skipped,
            EndKind::Unstarted,
        ] {
            assert!(!holds(kind, None));
        }
    }

    #[test]
    fn a_build_no_run_holds_ends_only_while_it_waits_unskipped() {
        for kind in [EndKind::Built, EndKind::Skipped] {
            let holds = |waiting_unskipped| {
                github_end_holds(kind, Target::BeforeGithub { waiting_unskipped })
            };
            assert!(holds(true));
            assert!(!holds(false));
        }
    }
}
