//! Host preparation as ployzd records it in `prepare/state.json`, and as the check-in loop reads
//! it back.
//!
//! Each step is one object: a `state` tag beside `started_at` and `finished_at`, as ployzd has
//! always written it, so every reader of an earlier release still parses what this one writes.
//! The file is written by ployzd and read by every CLI release, so its shape only grows.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned, ser::Error};
use serde_json::{Map, Value};

/// Everything ployzd records about preparing this host: the record it reads back to resume a run
/// and writes whole after each step.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PrepState {
    pub docker: PrepStep<DockerOutcome>,
    pub zfs: PrepStep<ZfsOutcome>,
    pub added_packages: Vec<String>,
    pub zfs_build: Option<ZfsBuild>,
    /// Preparation wrote the Debian contrib source, so discard may remove it.
    #[serde(default)]
    pub added_contrib_source: bool,
    /// Steps an earlier run left `installing` when it died, which this run redid.
    #[serde(default)]
    pub interrupted: Vec<String>,
}

/// The two steps ployzd takes to prepare a host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Docker,
    Zfs,
}

/// How a step ended, which is also the [`Step`] it was.
#[derive(Debug, PartialEq, Eq)]
pub enum StepOutcome {
    Docker(DockerOutcome),
    Zfs(ZfsOutcome),
}

impl PrepState {
    /// Starts a new run over this record: both steps begin again, and a step the last run left
    /// `installing` is named in `interrupted`. What the last run added stays, so discarding can
    /// still remove it. Returns the steps it found interrupted.
    pub fn restart(&mut self) -> Vec<String> {
        let interrupted: Vec<String> = [
            ("docker", matches!(self.docker, PrepStep::Running { .. })),
            ("zfs", matches!(self.zfs, PrepStep::Running { .. })),
        ]
        .into_iter()
        .filter(|(_, unfinished)| *unfinished)
        .map(|(step, _)| step.to_owned())
        .collect();
        self.docker = PrepStep::NotStarted;
        self.zfs = PrepStep::NotStarted;
        self.interrupted.clone_from(&interrupted);
        interrupted
    }

    /// Records `step` as running since `begun`.
    pub fn begin(&mut self, step: Step, begun: &Begun) {
        match step {
            Step::Docker => self.docker = begun.running(),
            Step::Zfs => self.zfs = begun.running(),
        }
    }

    /// Records the step `outcome` names as finished, which `begun` began.
    pub fn finish(&mut self, begun: Begun, outcome: StepOutcome, finished_at: String) {
        match outcome {
            StepOutcome::Docker(outcome) => self.docker = begun.finish(outcome, finished_at),
            StepOutcome::Zfs(outcome) => self.zfs = begun.finish(outcome, finished_at),
        }
    }

    /// Notes packages preparation is about to install, so discarding can remove them.
    pub fn note_packages(&mut self, packages: &[String]) {
        for package in packages {
            if !self.added_packages.contains(package) {
                self.added_packages.push(package.clone());
            }
        }
    }

    pub const fn note_contrib_source(&mut self) {
        self.added_contrib_source = true;
    }

    pub fn set_build(&mut self, build: ZfsBuild) {
        self.zfs_build = Some(build);
    }

    pub fn forget_build(&mut self) {
        self.zfs_build = None;
    }

    /// Records that packages now provide ZFS: what preparation added is theirs, and only `kept`
    /// of the build stays for discarding.
    pub fn take_over(&mut self, kept: Option<ZfsBuild>) {
        self.zfs_build = kept;
        self.added_packages.clear();
        self.added_contrib_source = false;
    }

    /// Records the ZFS step as finished without running, `off`, once its build and what it added
    /// are removed.
    pub fn discard(&mut self, finished_at: String) {
        self.added_packages.clear();
        self.zfs_build = None;
        self.added_contrib_source = false;
        self.zfs = PrepStep::Finished {
            started_at: None,
            finished_at,
            outcome: ZfsOutcome::Off,
        };
    }
}

/// What the ZFS step built or downloaded, which discarding the build removes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "route", rename_all = "lowercase")]
pub enum ZfsBuild {
    Ubuntu {
        downloaded: Vec<String>,
    },
    Dkms {
        module: String,
        version: String,
        source_dir: PathBuf,
    },
}

/// The two steps of [`PrepState`], which is all the check-in loop reads of it. A projection, not
/// a second record: it ignores `added_packages`, `zfs_build` and the fields after them, so a file
/// ployzd itself can't read back (a missing `added_packages`, a build route this release doesn't
/// know) still tells the check-in where each step stands. A host with no record reads as both steps
/// not started.
#[derive(Debug, Default)]
pub struct PrepSteps {
    pub docker: PrepStep<DockerOutcome>,
    pub zfs: PrepStep<ZfsOutcome>,
}

impl<'de> Deserialize<'de> for PrepSteps {
    /// Reads through a struct that serde names `PrepState`, as the check-in's own record was
    /// called before it moved here, so the "expected struct PrepState" a malformed file produces
    /// reads as it always has.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct PrepState {
            docker: PrepStep<DockerOutcome>,
            zfs: PrepStep<ZfsOutcome>,
        }
        let PrepState { docker, zfs } = PrepState::deserialize(deserializer)?;
        Ok(Self { docker, zfs })
    }
}

/// Why `prepare/state.json` didn't read.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("The host preparation state couldn't be read.")]
    Unreadable(#[source] io::Error),
    #[error("The host preparation state isn't a record this release reads.")]
    Malformed(#[source] serde_json::Error),
}

/// Reads the preparation record at `path` as `S`: the whole [`PrepState`] for ployzd, or the
/// [`PrepSteps`] a check-in reports from. `None` when there is no file.
///
/// # Errors
///
/// [`StateError::Unreadable`] when the file exists but can't be read, [`StateError::Malformed`]
/// when its bytes aren't an `S`.
pub fn read_state<S: DeserializeOwned>(path: &Path) -> Result<Option<S>, StateError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(StateError::Unreadable(error)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(StateError::Malformed)
}

/// The bytes ployzd writes to `prepare/state.json`.
///
/// # Errors
///
/// When a step can't be serialized.
pub fn encode_state(state: &PrepState) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec_pretty(state)
}

/// One preparation step, Docker's install or the ZFS build, with the outcome `O` it finishes on.
///
/// A step whose fields don't fit together, such as a failure with no `finished_at` or an
/// `installing` step that names one, reads as [`PrepStep::Unknown`]. So does a tag this release
/// doesn't know. Unknown keeps the object it read and writes it back unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PrepStep<O> {
    #[default]
    NotStarted,
    Running {
        started_at: String,
    },
    Finished {
        /// None for a step finished without running, as discarding a ZFS build records it.
        started_at: Option<String>,
        finished_at: String,
        outcome: O,
    },
    Unknown(Map<String, Value>),
}

impl<O> PrepStep<O> {
    #[must_use]
    pub const fn outcome(&self) -> Option<&O> {
        match self {
            Self::Finished { outcome, .. } => Some(outcome),
            Self::NotStarted | Self::Running { .. } | Self::Unknown(_) => None,
        }
    }

    /// The last time this step records, as RFC 3339 text.
    #[must_use]
    pub fn last_dated(&self) -> Option<&str> {
        match self {
            Self::Running { started_at } => Some(started_at),
            Self::Finished { finished_at, .. } => Some(finished_at),
            Self::NotStarted | Self::Unknown(_) => None,
        }
    }
}

impl<O: Outcome> PrepStep<O> {
    /// When this step failed, for a step that finished failed.
    #[must_use]
    pub fn failed_at(&self) -> Option<&str> {
        match self {
            Self::Finished {
                finished_at,
                outcome,
                ..
            } if outcome.failure().is_some() => Some(finished_at),
            Self::NotStarted | Self::Running { .. } | Self::Finished { .. } | Self::Unknown(_) => {
                None
            }
        }
    }
}

/// How a step ended.
pub trait Outcome {
    /// Why the step failed, when it did.
    fn failure(&self) -> Option<&str>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum DockerOutcome {
    Ready,
    Failed { message: String },
}

impl Outcome for DockerOutcome {
    fn failure(&self) -> Option<&str> {
        match self {
            Self::Ready => None,
            Self::Failed { message } => Some(message),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum ZfsOutcome {
    Ready,
    Failed { message: String },
    Skipped { reason: String },
    Off,
}

impl Outcome for ZfsOutcome {
    fn failure(&self) -> Option<&str> {
        match self {
            Self::Failed { message } => Some(message),
            Self::Ready | Self::Skipped { .. } | Self::Off => None,
        }
    }
}

/// A step ployzd has begun. A convention for ployzd's begin and finish pair: begin returns it and
/// finish consumes it, so that writer finishes only a step it began. The type enforces nothing
/// more: anyone can build one, or a finished step directly.
#[derive(Debug)]
#[must_use]
pub struct Begun {
    started_at: String,
}

impl Begun {
    pub const fn at(started_at: String) -> Self {
        Self { started_at }
    }

    /// The step as recorded while it runs.
    #[must_use]
    pub fn running<O>(&self) -> PrepStep<O> {
        PrepStep::Running {
            started_at: self.started_at.clone(),
        }
    }

    pub fn finish<O>(self, outcome: O, finished_at: String) -> PrepStep<O> {
        PrepStep::Finished {
            started_at: Some(self.started_at),
            finished_at,
            outcome,
        }
    }
}

const STATE: &str = "state";
const INSTALLING: &str = "installing";
const STARTED_AT: &str = "started_at";
const FINISHED_AT: &str = "finished_at";

impl<O: DeserializeOwned> PrepStep<O> {
    fn decode(fields: Map<String, Value>) -> Self {
        let time = |key| match fields.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(at)) => Ok(Some(at.clone())),
            Some(_) => Err(()),
        };
        let (Ok(started_at), Ok(finished_at)) = (time(STARTED_AT), time(FINISHED_AT)) else {
            return Self::Unknown(fields);
        };
        let installing = fields.get(STATE).and_then(Value::as_str) == Some(INSTALLING);
        match (installing, started_at, finished_at) {
            (true, None, None) => Self::NotStarted,
            (true, Some(started_at), None) => Self::Running { started_at },
            (false, started_at, Some(finished_at)) => {
                match O::deserialize(Value::Object(fields.clone())) {
                    Ok(outcome) => Self::Finished {
                        started_at,
                        finished_at,
                        outcome,
                    },
                    Err(_) => Self::Unknown(fields),
                }
            }
            (true, _, Some(_)) | (false, _, None) => Self::Unknown(fields),
        }
    }
}

impl<'de, O: DeserializeOwned> Deserialize<'de> for PrepStep<O> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Map::deserialize(deserializer).map(Self::decode)
    }
}

impl<O: Serialize> Serialize for PrepStep<O> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let installing = || Map::from_iter([(STATE.to_owned(), Value::from(INSTALLING))]);
        let (mut fields, started_at, finished_at) = match self {
            Self::NotStarted => (installing(), None, None),
            Self::Running { started_at } => (installing(), Some(started_at), None),
            Self::Finished {
                started_at,
                finished_at,
                outcome,
            } => {
                let Value::Object(fields) =
                    serde_json::to_value(outcome).map_err(S::Error::custom)?
                else {
                    return Err(S::Error::custom("a step outcome is a tagged object"));
                };
                (fields, started_at.as_ref(), Some(finished_at))
            }
            Self::Unknown(fields) => return fields.serialize(serializer),
        };
        let time = |at: Option<&String>| at.map_or(Value::Null, |at| Value::from(at.as_str()));
        fields.insert(STARTED_AT.to_owned(), time(started_at));
        fields.insert(FINISHED_AT.to_owned(), time(finished_at));
        fields.serialize(serializer)
    }
}

#[cfg(test)]
#[path = "host_preparation_tests.rs"]
mod tests;
