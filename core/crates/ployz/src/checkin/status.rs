//! `status.json`: what the last check-in pass saw, kept for `ployz status`.

use std::{fs, io, path::Path};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::paths::{Paths, holds};
use super::wire::Wait;
use super::{Attempt, DOCKER_INSTALL_FAILED, DockerPrep, Issue, Prep, StopReason, ZfsPrep, enroll};

pub(crate) const NO_CLUSTER: &str = "No cluster yet";
pub(crate) const NO_CLUSTER_WHY: &str = "Enroll one server in Ployz Cloud to start it.";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Seen {
    Wait,
    Enroll,
    Joined,
    Stop,
    Refused,
    Held,
    Unreachable,
    NotCloud,
    ClockOff,
    Outdated,
    ProbeFailed,
    NoPloyzd,
}

/// What a check-in pass leaves in status.json, as the two readers of the file take it apart: the
/// next pass reads the [`Memory`], and `ployz status` prints the [`View`]. On disk they are one
/// flat object, in the order the file has always had its keys, since an older loop or `ployz
/// status` reads the file across a release.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(from = "Document", into = "Document")]
pub(crate) struct LocalCheckin {
    pub(crate) memory: Memory,
    pub(crate) view: View,
}

/// What the next pass reads back from status.json, however long ago and under whichever restart
/// it was written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Memory {
    pub(crate) token_fingerprint: Option<String>,
    pub(crate) prepare_retries: u32,
    pub(crate) prepare_started: Option<DateTime<Utc>>,
    pub(crate) refused: Option<Refusal>,
    /// The enroll attempt Cloud last instructed, sent on every check-in until Cloud replaces it.
    pub(crate) attempt: Option<Attempt>,
}

/// What `ployz status` prints of the last pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct View {
    pub(crate) answer: Seen,
    pub(crate) reason: Option<StopReason>,
    pub(crate) message: Option<String>,
    pub(crate) at: DateTime<Utc>,
    pub(crate) prep: Option<Prep>,
    pub(crate) blockers: Vec<Issue>,
    /// Cloud's last wait said the Organization has no cluster for this host to join yet.
    pub(crate) no_cluster: bool,
}

/// status.json as written.
#[derive(Deserialize, Serialize)]
struct Document {
    answer: Seen,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<StopReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prep: Option<Prep>,
    #[serde(default)]
    blockers: Vec<Issue>,
    #[serde(default)]
    prepare_retries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prepare_started: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refused: Option<Refusal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attempt: Option<Attempt>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    no_cluster: bool,
}

impl From<Document> for LocalCheckin {
    fn from(document: Document) -> Self {
        Self {
            memory: Memory {
                token_fingerprint: document.token_fingerprint,
                prepare_retries: document.prepare_retries,
                prepare_started: document.prepare_started,
                refused: document.refused,
                attempt: document.attempt,
            },
            view: View {
                answer: document.answer,
                reason: document.reason,
                message: document.message,
                at: document.at,
                prep: document.prep,
                blockers: document.blockers,
                no_cluster: document.no_cluster,
            },
        }
    }
}

impl From<LocalCheckin> for Document {
    fn from(status: LocalCheckin) -> Self {
        let LocalCheckin { memory, view } = status;
        Self {
            answer: view.answer,
            token_fingerprint: memory.token_fingerprint,
            reason: view.reason,
            message: view.message,
            at: view.at,
            prep: view.prep,
            blockers: view.blockers,
            prepare_retries: memory.prepare_retries,
            prepare_started: memory.prepare_started,
            refused: memory.refused,
            attempt: memory.attempt,
            no_cluster: view.no_cluster,
        }
    }
}

/// Cloud's first refusal of a token, dated by Cloud's clock. A second refusal of the same token
/// confirms it once Cloud dates that one BACKOFF later, across restarts.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Refusal {
    pub(crate) token: String,
    pub(crate) date: DateTime<Utc>,
}

/// What a pass takes from the record it finds for the token it sends. The preparation memory
/// carries over whichever token wrote it; the enroll attempt, the join and the refusal only count
/// for the token they were recorded under.
pub(super) struct Recalled {
    /// The memory the pass's own record starts from.
    pub(super) memory: Memory,
    pub(super) joined: bool,
    pub(super) refusal: Option<Refusal>,
}

impl Recalled {
    pub(super) fn of(previous: Option<LocalCheckin>, fingerprint: &str) -> Self {
        let Some(previous) = previous else {
            return Self {
                memory: Memory {
                    token_fingerprint: Some(fingerprint.to_owned()),
                    ..Memory::default()
                },
                joined: false,
                refusal: None,
            };
        };
        let this_token = previous.memory.token_fingerprint.as_deref() == Some(fingerprint);
        let refusal = (previous.view.answer == Seen::Refused)
            .then_some(previous.memory.refused)
            .flatten()
            .filter(|refusal| refusal.token == fingerprint);
        Self {
            joined: this_token && previous.view.answer == Seen::Joined,
            refusal,
            memory: Memory {
                token_fingerprint: Some(fingerprint.to_owned()),
                prepare_retries: previous.memory.prepare_retries,
                prepare_started: previous.memory.prepare_started,
                refused: None,
                attempt: this_token.then_some(previous.memory.attempt).flatten(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Headline {
    pub(crate) title: &'static str,
    pub(crate) ended: bool,
}

impl Headline {
    pub(super) const fn open(title: &'static str) -> Self {
        Self {
            title,
            ended: false,
        }
    }

    const fn ended(title: &'static str) -> Self {
        Self { title, ended: true }
    }
}

const STATUS_READABLE_BY_ALL: u32 = 0o644;

impl LocalCheckin {
    /// A record of what a pass saw at `at`, with nothing said yet beyond `answer`.
    pub(crate) fn new(answer: Seen, at: DateTime<Utc>, memory: Memory) -> Self {
        Self {
            memory,
            view: View {
                answer,
                reason: None,
                message: None,
                at,
                prep: None,
                blockers: Vec::new(),
                no_cluster: false,
            },
        }
    }

    pub(crate) fn read(path: &Path) -> io::Result<Option<Self>> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The record the last pass left, or none when there is none or it can't be read. A pass that
    /// finds none starts fresh, as on a host that never checked in.
    pub(super) fn recall(path: &Path) -> Option<Self> {
        Self::read(path).ok().flatten()
    }

    /// Whether `ployz status` should show the message: it says what ended the loop or what to fix.
    pub(crate) fn says_why(&self) -> bool {
        matches!(
            self.view.answer,
            Seen::Stop
                | Seen::Refused
                | Seen::Held
                | Seen::NotCloud
                | Seen::ClockOff
                | Seen::Outdated
                | Seen::ProbeFailed
                | Seen::NoPloyzd
        ) || self.enroll_failed()
            || (self.view.answer == Seen::Wait && self.view.no_cluster)
    }

    pub(super) fn waited(&mut self, wait: Wait) {
        self.view.answer = Seen::Wait;
        self.view.no_cluster = wait.no_cluster;
        if wait.no_cluster {
            self.view.message = Some(NO_CLUSTER_WHY.to_owned());
        }
    }

    /// A preparation started at `now` tries Docker's install again, so its last failure no
    /// longer stands, and its automatic retries are restored.
    pub(crate) fn restarted_preparation(&mut self, now: DateTime<Utc>) {
        self.memory.prepare_started = Some(now);
        self.memory.prepare_retries = 0;
        self.view
            .blockers
            .retain(|blocker| blocker.code != DOCKER_INSTALL_FAILED);
    }

    pub(super) fn enroll_failed(&self) -> bool {
        self.view.answer == Seen::Wait
            && self
                .memory
                .attempt
                .as_ref()
                .is_some_and(|attempt| attempt.failure.is_some())
    }

    pub(crate) fn headline(&self) -> Headline {
        match self.view.answer {
            Seen::Stop => Headline::ended(self.view.reason.map_or("Stopped", StopReason::title)),
            Seen::Refused => Headline::open("Token refused"),
            Seen::Held => Headline::open("Held by Cloud"),
            Seen::Unreachable => Headline::open("Can't reach Cloud"),
            Seen::NotCloud => Headline::open("Not Ployz Cloud"),
            Seen::ClockOff => Headline::open("Clock is off"),
            Seen::Outdated => Headline::open("Needs a newer ployz"),
            Seen::ProbeFailed => Headline::open("Can't inspect this host"),
            Seen::NoPloyzd => Headline::open("No ployzd"),
            Seen::Enroll => Headline::open(
                self.memory
                    .attempt
                    .as_ref()
                    .map_or("Enrolling", |attempt| enroll::title(attempt.stage)),
            ),
            Seen::Joined => Headline::open("Joined"),
            Seen::Wait if !self.view.blockers.is_empty() => Headline::open("Blocked"),
            Seen::Wait if self.enroll_failed() => Headline::open("Enroll failed"),
            Seen::Wait => match self.view.prep {
                Some(prep) if prep.docker == DockerPrep::Installing => {
                    Headline::open("Installing Docker")
                }
                Some(prep) if prep.zfs == ZfsPrep::Installing => Headline::open("Building ZFS"),
                Some(_) | None if self.view.no_cluster => Headline::open(NO_CLUSTER),
                Some(_) | None => Headline::open("Waiting for approval"),
            },
        }
    }

    fn write(&self, path: &Path) -> io::Result<()> {
        let directory = path.parent().unwrap_or(Path::new("."));
        // Whether the loop writing this held its lock, for `loop_stopped`.
        #[derive(Serialize)]
        struct Written<'a> {
            #[serde(flatten)]
            record: &'a LocalCheckin,
            #[serde(skip_serializing_if = "std::ops::Not::not")]
            locked: bool,
        }
        let written = Written {
            record: self,
            locked: holds(directory),
        };
        let contents = serde_json::to_vec_pretty(&written)?;
        crate::atomic_file::replace(path, &contents, STATUS_READABLE_BY_ALL)?;
        Ok(())
    }
}

/// Says in status.json why the loop can't start, keeping what the last pass saw of the host.
pub(crate) fn no_ployzd(paths: &Paths, why: &str, now: DateTime<Utc>) {
    let (memory, prep) = LocalCheckin::recall(&paths.status()).map_or_else(
        || (Memory::default(), None),
        |previous| {
            let memory = Memory {
                refused: None,
                ..previous.memory
            };
            (memory, previous.view.prep)
        },
    );
    let mut status = LocalCheckin::new(Seen::NoPloyzd, now, memory);
    status.view.message = Some(why.to_owned());
    status.view.prep = prep;
    record(&status, paths);
}

pub(crate) fn record(status: &LocalCheckin, paths: &Paths) {
    if let Err(error) = status.write(&paths.status()) {
        crate::ui::note(format!(
            "Could not write {}: {error}",
            paths.status().display()
        ));
    }
}

pub(crate) fn ago(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = now.signed_duration_since(at).num_seconds().max(0);
    match seconds {
        0..60 => format!("{seconds}s ago"),
        60..3600 => format!("{}m ago", seconds / 60),
        3600..86400 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}
