//! `status.json`: what the last check-in pass saw, kept for `ployz status`.

use std::{fs, io, os::unix::fs::PermissionsExt, path::Path};

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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct LocalCheckin {
    pub(crate) answer: Seen,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) token_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<StopReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<String>,
    pub(crate) at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prep: Option<Prep>,
    #[serde(default)]
    pub(crate) blockers: Vec<Issue>,
    #[serde(default)]
    pub(crate) prepare_retries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prepare_started: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) refused: Option<Refusal>,
    /// The enroll attempt Cloud last instructed, sent on every check-in until Cloud replaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) attempt: Option<Attempt>,
    /// Cloud's last wait said the Organization has no cluster for this host to join yet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) no_cluster: bool,
}

/// Cloud's first refusal of a token, dated by Cloud's clock. A second refusal of the same token
/// confirms it once Cloud dates that one BACKOFF later, across restarts.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Refusal {
    pub(crate) token: String,
    pub(crate) date: DateTime<Utc>,
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
    pub(crate) fn read(path: &Path) -> io::Result<Option<Self>> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Whether `ployz status` should show the message: it says what ended the loop or what to fix.
    pub(crate) fn says_why(&self) -> bool {
        matches!(
            self.answer,
            Seen::Stop
                | Seen::Refused
                | Seen::Held
                | Seen::NotCloud
                | Seen::ClockOff
                | Seen::Outdated
                | Seen::ProbeFailed
                | Seen::NoPloyzd
        ) || self.enroll_failed()
            || (self.answer == Seen::Wait && self.no_cluster)
    }

    pub(super) fn waited(&mut self, wait: Wait) {
        self.answer = Seen::Wait;
        self.no_cluster = wait.no_cluster;
        if wait.no_cluster {
            self.message = Some(NO_CLUSTER_WHY.to_owned());
        }
    }

    /// A preparation started at `now` tries Docker's install again, so its last failure no
    /// longer stands, and its automatic retries are restored.
    pub(crate) fn restarted_preparation(&mut self, now: DateTime<Utc>) {
        self.prepare_started = Some(now);
        self.prepare_retries = 0;
        self.blockers
            .retain(|blocker| blocker.code != DOCKER_INSTALL_FAILED);
    }

    pub(super) fn enroll_failed(&self) -> bool {
        self.answer == Seen::Wait
            && self
                .attempt
                .as_ref()
                .is_some_and(|attempt| attempt.failure.is_some())
    }

    pub(crate) fn headline(&self) -> Headline {
        match self.answer {
            Seen::Stop => Headline::ended(self.reason.map_or("Stopped", StopReason::title)),
            Seen::Refused => Headline::open("Token refused"),
            Seen::Held => Headline::open("Held by Cloud"),
            Seen::Unreachable => Headline::open("Can't reach Cloud"),
            Seen::NotCloud => Headline::open("Not Ployz Cloud"),
            Seen::ClockOff => Headline::open("Clock is off"),
            Seen::Outdated => Headline::open("Needs a newer ployz"),
            Seen::ProbeFailed => Headline::open("Can't inspect this host"),
            Seen::NoPloyzd => Headline::open("No ployzd"),
            Seen::Enroll => Headline::open(
                self.attempt
                    .as_ref()
                    .map_or("Enrolling", |attempt| enroll::title(attempt.stage)),
            ),
            Seen::Joined => Headline::open("Joined"),
            Seen::Wait if !self.blockers.is_empty() => Headline::open("Blocked"),
            Seen::Wait if self.enroll_failed() => Headline::open("Enroll failed"),
            Seen::Wait => match self.prep {
                Some(prep) if prep.docker == DockerPrep::Installing => {
                    Headline::open("Installing Docker")
                }
                Some(prep) if prep.zfs == ZfsPrep::Installing => Headline::open("Building ZFS"),
                Some(_) | None if self.no_cluster => Headline::open(NO_CLUSTER),
                Some(_) | None => Headline::open("Waiting for approval"),
            },
        }
    }

    fn write(&self, path: &Path) -> io::Result<()> {
        let directory = path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
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
        serde_json::to_writer_pretty(&mut file, &written)?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(STATUS_READABLE_BY_ALL))?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|error| error.error)?;
        Ok(())
    }
}

/// Says in status.json why the loop can't start, keeping what the last pass saw of the host.
pub(crate) fn no_ployzd(paths: &Paths, why: &str, now: DateTime<Utc>) {
    let previous = LocalCheckin::read(&paths.status()).ok().flatten();
    let status = LocalCheckin {
        answer: Seen::NoPloyzd,
        token_fingerprint: previous
            .as_ref()
            .and_then(|status| status.token_fingerprint.clone()),
        reason: None,
        message: Some(why.to_owned()),
        at: now,
        prep: previous.as_ref().and_then(|status| status.prep),
        blockers: Vec::new(),
        prepare_retries: previous.as_ref().map_or(0, |status| status.prepare_retries),
        prepare_started: previous.as_ref().and_then(|status| status.prepare_started),
        refused: None,
        attempt: previous.and_then(|status| status.attempt),
        no_cluster: false,
    };
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
