//! Fault injection for verify-cluster.

use ployz_core::{
    DockerVolumeName, SnapshotGuid, SnapshotName, VolumeGen, WantedCopy, WriterEpoch,
};

#[cfg(feature = "verify-faults")]
pub use enabled::{PATH_ENV, kill_inside, leak_memory};
#[cfg(feature = "verify-faults")]
pub(crate) use enabled::{apply, check};

/// Verbs whose effect outlives the RPC; `kill-daemon` fires inside them, from [`kill_inside`].
#[cfg_attr(not(feature = "verify-faults"), allow(dead_code))]
const LONG_EFFECTS: [&str; 8] = [
    "SetVolumeCopy.WriterToMirror",
    "SetVolumeCopy.WriterToSnapshot",
    "SetVolumeCopy.WriterToForget",
    "SetVolumeCopy.SeedToForget",
    "SetVolumeCopy.MirrorToWriter",
    "SetVolumeCopy.MirrorToMirror",
    "SetVolumeCopy.MirrorToForget",
    "SetVolumeCopy.NoneToMirror",
];

/// The Volume a fault fires for, by physical name, the want it serves and the lane's gen.
/// The verify harness credits a fault line to a run by these fields, not by when it was
/// logged; gen alone tells two runs' Forget lines apart.
#[derive(Clone, Copy, Debug)]
pub struct Identity<'a> {
    pub volume: &'a DockerVolumeName,
    pub want: Want<'a>,
    pub r#gen: VolumeGen,
}

/// A want without a Writer's start, which carries the whole resolved spec.
#[derive(Clone, Copy, Debug)]
pub enum Want<'a> {
    Writer {
        at: SnapshotGuid,
        epoch: WriterEpoch,
    },
    Mirror {
        target: &'a SnapshotName,
    },
    Snapshot {
        snapshot: &'a SnapshotName,
    },
    Forget,
    Floor,
}

impl<'a> Identity<'a> {
    #[must_use]
    pub fn new(volume: &'a DockerVolumeName, want: &'a WantedCopy, r#gen: VolumeGen) -> Self {
        let want = match want {
            WantedCopy::Writer { at, epoch, .. } => Want::Writer {
                at: *at,
                epoch: *epoch,
            },
            WantedCopy::Mirror { target, .. } => Want::Mirror { target },
            WantedCopy::Snapshot { name } => Want::Snapshot { snapshot: name },
            WantedCopy::Forget => Want::Forget,
            WantedCopy::Floor => Want::Floor,
        };
        Self {
            volume,
            want,
            r#gen,
        }
    }
}

#[cfg_attr(not(feature = "verify-faults"), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
enum Act {
    Delay {
        secs: u64,
    },
    Kill,
    KillInside,
    Unanswered,
    /// A delayed or unanswered verb resumes: the harness reads `disarm` as proof the hold
    /// was still running when it disarmed.
    Released {
        by: Release,
    },
}

#[cfg_attr(not(feature = "verify-faults"), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
enum Release {
    Disarm,
    Limit,
}

#[cfg_attr(not(feature = "verify-faults"), allow(dead_code))]
fn warn(act: Act, verb: &'static str, identity: Option<Identity<'_>>) {
    let (message, secs, by) = match act {
        Act::Delay { secs } => ("fault: delaying switch verb", Some(secs), None),
        Act::Kill => ("fault: killing daemon", None, None),
        Act::KillInside => ("fault: killing daemon inside the effect", None, None),
        Act::Unanswered => ("fault: leaving switch verb unanswered", None, None),
        Act::Released { by } => (
            "fault: releasing switch verb",
            None,
            Some(match by {
                Release::Disarm => "disarm",
                Release::Limit => "limit",
            }),
        ),
    };
    let (target, snapshot, writer) = match identity.map(|identity| identity.want) {
        Some(Want::Mirror { target }) => (Some(target), None, None),
        Some(Want::Snapshot { snapshot }) => (None, Some(snapshot), None),
        Some(Want::Writer { at, epoch }) => (None, None, Some((at, epoch))),
        Some(Want::Forget | Want::Floor) | None => (None, None, None),
    };
    tracing::warn!(
        verb,
        secs,
        by = by.map(display),
        volume = identity.map(|identity| display(identity.volume)),
        target = target.map(display),
        snapshot = snapshot.map(display),
        at = writer.map(|(at, _)| display(at)),
        epoch = writer.map(|(_, epoch)| display(epoch)),
        r#gen = identity.map(|identity| display(identity.r#gen)),
        "{message}"
    );
}

#[cfg(not(feature = "verify-faults"))]
pub(crate) async fn apply(_verb: &'static str, _identity: Option<Identity<'_>>) {}

#[cfg(not(feature = "verify-faults"))]
pub fn kill_inside(_verb: &'static str, _identity: Option<Identity<'_>>) {}

#[cfg(not(feature = "verify-faults"))]
pub fn leak_memory() {}

#[cfg(not(feature = "verify-faults"))]
pub(crate) fn check() -> std::io::Result<()> {
    Ok(())
}

#[cfg(feature = "verify-faults")]
mod enabled {
    use std::{
        io,
        path::{Path, PathBuf},
        str::FromStr,
        time::{Duration, Instant},
    };

    use super::{Act, Identity, Release, warn};

    /// Read on every check, so a run arms and disarms a fault without restarting a daemon.
    const PATH: &str = "/run/ployz/fault";
    /// Names another fault file, so parallel test children each arm their own.
    pub const PATH_ENV: &str = "PLOYZD_FAULT_FILE";
    const POLL: Duration = Duration::from_secs(1);

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) enum FaultPoint {
        DelayRpc {
            verb: String,
            secs: u64,
        },
        KillDaemon {
            verb: String,
        },
        Unanswered,
        /// Grows the process by `mib_per_sec` MiB of touched memory every second, forever.
        LeakMemory {
            mib_per_sec: usize,
        },
    }

    #[derive(Debug, thiserror::Error)]
    #[error(
        "{0:?} is not a fault point (delay-rpc:<verb>:<secs>, kill-daemon:<verb>, unanswered, leak-memory:<mib-per-sec>)"
    )]
    pub(crate) struct FaultParseError(String);

    impl FromStr for FaultPoint {
        type Err = FaultParseError;

        fn from_str(value: &str) -> Result<Self, Self::Err> {
            let malformed = || FaultParseError(value.to_owned());
            let mut parts = value.split(':');
            let fault = match (parts.next(), parts.next(), parts.next(), parts.next()) {
                (Some("delay-rpc"), Some(verb), Some(secs), None) if !verb.is_empty() => {
                    FaultPoint::DelayRpc {
                        verb: verb.to_owned(),
                        secs: secs.parse().map_err(|_| malformed())?,
                    }
                }
                (Some("kill-daemon"), Some(verb), None, None) if !verb.is_empty() => {
                    FaultPoint::KillDaemon {
                        verb: verb.to_owned(),
                    }
                }
                (Some("unanswered"), None, None, None) => FaultPoint::Unanswered,
                (Some("leak-memory"), Some(mib), None, None) => FaultPoint::LeakMemory {
                    mib_per_sec: mib
                        .parse()
                        .ok()
                        .filter(|mib| *mib > 0)
                        .ok_or_else(malformed)?,
                },
                _ => return Err(malformed()),
            };
            Ok(fault)
        }
    }

    fn read(path: &Path) -> io::Result<Option<FaultPoint>> {
        match std::fs::read_to_string(path) {
            Ok(text) => text
                .trim()
                .parse()
                .map(Some)
                .map_err(|error| io::Error::other(format!("{}: {error}", path.display()))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn path() -> PathBuf {
        std::env::var_os(PATH_ENV).map_or_else(|| PathBuf::from(PATH), PathBuf::from)
    }

    fn armed() -> Option<FaultPoint> {
        read(&path()).ok().flatten()
    }

    /// Refuses to start a daemon whose fault is misspelled, so a run cannot pass without it.
    pub(crate) fn check() -> io::Result<()> {
        read(&path()).map(|_| ())
    }

    pub(crate) async fn apply(verb: &'static str, identity: Option<Identity<'_>>) {
        apply_at(&path(), verb, identity).await;
    }

    async fn apply_at(path: &Path, verb: &'static str, identity: Option<Identity<'_>>) {
        let Some(fault) = read(path).ok().flatten() else {
            return;
        };
        match &fault {
            FaultPoint::DelayRpc { verb: wanted, secs } if wanted == verb => {
                warn(Act::Delay { secs: *secs }, verb, identity);
                let by = hold(path, &fault, Some(Duration::from_secs(*secs))).await;
                warn(Act::Released { by }, verb, identity);
            }
            FaultPoint::KillDaemon { verb: wanted }
                if wanted == verb && !super::LONG_EFFECTS.contains(&verb) =>
            {
                warn(Act::Kill, verb, identity);
                std::process::abort();
            }
            FaultPoint::Unanswered => {
                warn(Act::Unanswered, verb, identity);
                let by = hold(path, &fault, None).await;
                warn(Act::Released { by }, verb, identity);
            }
            FaultPoint::DelayRpc { .. }
            | FaultPoint::KillDaemon { .. }
            | FaultPoint::LeakMemory { .. } => {}
        }
    }

    /// Holds while `path` still names `fault`, so disarming frees the held verb and the
    /// switch lock it may own.
    async fn hold(path: &Path, fault: &FaultPoint, limit: Option<Duration>) -> Release {
        let until = limit.map(|limit| Instant::now() + limit);
        loop {
            let left = until.map(|until| until.saturating_duration_since(Instant::now()));
            if left == Some(Duration::ZERO) {
                return Release::Limit;
            }
            tokio::time::sleep(left.map_or(POLL, |left| left.min(POLL))).await;
            if read(path).ok().flatten().as_ref() != Some(fault) {
                return Release::Disarm;
            }
        }
    }

    /// Starts a thread that leaks memory when `leak-memory:<mib-per-sec>` is armed, so a run
    /// can watch the kernel kill the process at its unit's `MemoryMax`.
    pub fn leak_memory() {
        let Some(FaultPoint::LeakMemory { mib_per_sec }) = armed() else {
            return;
        };
        tracing::warn!(mib_per_sec, "fault: leaking memory");
        std::thread::spawn(move || {
            loop {
                std::mem::forget(vec![1_u8; mib_per_sec << 20]);
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    }

    /// Aborts the process running `verb`'s effect when `kill-daemon:<verb>` names it.
    pub fn kill_inside(verb: &'static str, identity: Option<Identity<'_>>) {
        if let Some(FaultPoint::KillDaemon { verb: wanted }) = armed()
            && wanted == verb
        {
            warn(Act::KillInside, verb, identity);
            std::process::abort();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn fault_points_parse_their_file_form() {
            for (value, fault) in [
                (
                    "delay-rpc:SetVolumeCopy:30",
                    FaultPoint::DelayRpc {
                        verb: "SetVolumeCopy".into(),
                        secs: 30,
                    },
                ),
                (
                    "kill-daemon:InspectVolumeCopy",
                    FaultPoint::KillDaemon {
                        verb: "InspectVolumeCopy".into(),
                    },
                ),
                ("unanswered", FaultPoint::Unanswered),
                ("leak-memory:16", FaultPoint::LeakMemory { mib_per_sec: 16 }),
            ] {
                assert_eq!(value.parse::<FaultPoint>().unwrap(), fault, "{value}");
            }
            for value in [
                "",
                "delay-rpc",
                "delay-rpc:SetVolumeCopy",
                "delay-rpc:SetVolumeCopy:soon",
                "delay-rpc::3",
                "kill-daemon",
                "kill-daemon:SetVolumeCopy:extra",
                "kill-after-record:SeedVolume",
                "hold-task:SetVolumeCopy:610",
                "busy-mount:data",
                "unanswered:SetVolumeCopy",
                "leak-memory",
                "leak-memory:0",
                "leak-memory:lots",
                "explode",
            ] {
                assert!(value.parse::<FaultPoint>().is_err(), "{value:?}");
            }
        }

        #[test]
        fn the_file_arms_a_fault_and_names_itself_when_malformed() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("fault");
            assert!(read(&path).unwrap().is_none());
            std::fs::write(&path, "delay-rpc:SetVolumeCopy:30\n").unwrap();
            assert_eq!(
                read(&path).unwrap(),
                Some(FaultPoint::DelayRpc {
                    verb: "SetVolumeCopy".into(),
                    secs: 30
                })
            );
            std::fs::write(&path, "explode").unwrap();
            let error = read(&path).unwrap_err().to_string();
            assert!(error.contains(&path.display().to_string()), "{error}");
        }

        #[tokio::test]
        async fn disarming_ends_a_hold() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("fault");
            std::fs::write(&path, "unanswered").unwrap();
            let held = tokio::spawn({
                let path = path.clone();
                async move { hold(&path, &FaultPoint::Unanswered, None).await }
            });
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(!held.is_finished(), "an armed hold waits");
            std::fs::remove_file(&path).unwrap();
            let by = tokio::time::timeout(3 * POLL, held).await.unwrap().unwrap();
            assert!(matches!(by, Release::Disarm), "{by:?}");
        }

        #[tokio::test]
        async fn a_held_verb_logs_its_release_by_disarm_or_limit_with_its_identity() {
            let volume = ployz_core::DockerVolumeName::parse("app_data").unwrap();
            let target: ployz_core::SnapshotName = "w-3-0".parse().unwrap();
            let identity = Identity {
                volume: &volume,
                want: super::super::Want::Mirror { target: &target },
                r#gen: ployz_core::VolumeGen::new(5),
            };
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("fault");
            for (armed, disarm, line) in [
                (
                    "delay-rpc:SetVolumeCopy.MirrorToMirror:30",
                    true,
                    r#" WARN fault: releasing switch verb verb="SetVolumeCopy.MirrorToMirror" by=disarm volume=app_data target=w-3-0 gen=5"#,
                ),
                (
                    "delay-rpc:SetVolumeCopy.MirrorToMirror:1",
                    false,
                    r#" WARN fault: releasing switch verb verb="SetVolumeCopy.MirrorToMirror" by=limit volume=app_data target=w-3-0 gen=5"#,
                ),
            ] {
                std::fs::write(&path, armed).unwrap();
                let (capture, _logging) = crate::diag::test_support::capture();
                let applied = apply_at(&path, "SetVolumeCopy.MirrorToMirror", Some(identity));
                let disarming = async {
                    if disarm {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        std::fs::remove_file(&path).unwrap();
                    }
                };
                tokio::time::timeout(3 * POLL, async { tokio::join!(applied, disarming) })
                    .await
                    .unwrap();
                let lines = capture.text();
                assert_eq!(lines.lines().nth(1), Some(line), "{lines}");
            }
        }

        #[tokio::test]
        async fn a_hold_that_runs_out_says_so() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("fault");
            let fault = FaultPoint::DelayRpc {
                verb: "SetVolumeCopy".into(),
                secs: 1,
            };
            std::fs::write(&path, "delay-rpc:SetVolumeCopy:1").unwrap();
            let by = hold(&path, &fault, Some(Duration::from_millis(100))).await;
            assert!(matches!(by, Release::Limit), "{by:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{DockerVolumeName, SnapshotGuid, SnapshotName, VolumeGen, WriterEpoch};

    use super::{Act, Identity, Release, Want, warn};

    #[test]
    fn fault_lines_name_the_volume_and_the_want() {
        let volume = DockerVolumeName::parse("app_data").unwrap();
        let round: SnapshotName = "w-1-2".parse().unwrap();
        let snapshot: SnapshotName = "w-1-1".parse().unwrap();
        let writer = Want::Writer {
            at: SnapshotGuid::new(7_310_442),
            epoch: WriterEpoch::new(1),
        };
        let delay = Act::Delay { secs: 6 };
        for (act, verb, want, line) in [
            (
                delay,
                "SetVolumeCopy.MirrorToMirror",
                Some(Want::Mirror { target: &round }),
                r#" WARN fault: delaying switch verb verb="SetVolumeCopy.MirrorToMirror" secs=6 volume=app_data target=w-1-2 gen=4"#,
            ),
            (
                Act::Delay { secs: 20 },
                "SetVolumeCopy.MirrorToWriter",
                Some(writer),
                r#" WARN fault: delaying switch verb verb="SetVolumeCopy.MirrorToWriter" secs=20 volume=app_data at=7310442 epoch=1 gen=4"#,
            ),
            (
                delay,
                "SetVolumeCopy.WriterToSnapshot",
                Some(Want::Snapshot {
                    snapshot: &snapshot,
                }),
                r#" WARN fault: delaying switch verb verb="SetVolumeCopy.WriterToSnapshot" secs=6 volume=app_data snapshot=w-1-1 gen=4"#,
            ),
            (
                delay,
                "SetVolumeCopy.WriterToForget",
                Some(Want::Forget),
                r#" WARN fault: delaying switch verb verb="SetVolumeCopy.WriterToForget" secs=6 volume=app_data gen=4"#,
            ),
            (
                Act::KillInside,
                "SetVolumeCopy.MirrorToMirror",
                Some(Want::Mirror { target: &round }),
                r#" WARN fault: killing daemon inside the effect verb="SetVolumeCopy.MirrorToMirror" volume=app_data target=w-1-2 gen=4"#,
            ),
            (
                Act::KillInside,
                "SetVolumeCopy.MirrorToWriter",
                Some(writer),
                r#" WARN fault: killing daemon inside the effect verb="SetVolumeCopy.MirrorToWriter" volume=app_data at=7310442 epoch=1 gen=4"#,
            ),
            (
                Act::KillInside,
                "SetVolumeCopy.WriterToSnapshot",
                Some(Want::Snapshot {
                    snapshot: &snapshot,
                }),
                r#" WARN fault: killing daemon inside the effect verb="SetVolumeCopy.WriterToSnapshot" volume=app_data snapshot=w-1-1 gen=4"#,
            ),
            (
                Act::KillInside,
                "SetVolumeCopy.WriterToForget",
                Some(Want::Forget),
                r#" WARN fault: killing daemon inside the effect verb="SetVolumeCopy.WriterToForget" volume=app_data gen=4"#,
            ),
            (
                Act::Kill,
                "SetVolumeCopy.WriterToWriter",
                Some(writer),
                r#" WARN fault: killing daemon verb="SetVolumeCopy.WriterToWriter" volume=app_data at=7310442 epoch=1 gen=4"#,
            ),
            (
                Act::Unanswered,
                "SetVolumeCopy",
                Some(Want::Mirror { target: &round }),
                r#" WARN fault: leaving switch verb unanswered verb="SetVolumeCopy" volume=app_data target=w-1-2 gen=4"#,
            ),
            (
                Act::Released {
                    by: Release::Disarm,
                },
                "SetVolumeCopy.MirrorToMirror",
                Some(Want::Mirror { target: &round }),
                r#" WARN fault: releasing switch verb verb="SetVolumeCopy.MirrorToMirror" by=disarm volume=app_data target=w-1-2 gen=4"#,
            ),
            (
                Act::Released { by: Release::Limit },
                "SetVolumeCopy",
                Some(Want::Forget),
                r#" WARN fault: releasing switch verb verb="SetVolumeCopy" by=limit volume=app_data gen=4"#,
            ),
            (
                Act::Unanswered,
                "SetVolumeCopy",
                Some(Want::Floor),
                r#" WARN fault: leaving switch verb unanswered verb="SetVolumeCopy" volume=app_data gen=4"#,
            ),
            (
                Act::KillInside,
                "Create",
                None,
                r#" WARN fault: killing daemon inside the effect verb="Create""#,
            ),
        ] {
            let (capture, _logging) = crate::diag::test_support::capture();
            let identity = want.map(|want| Identity {
                volume: &volume,
                want,
                r#gen: VolumeGen::new(4),
            });
            warn(act, verb, identity);
            assert_eq!(capture.text().lines().collect::<Vec<_>>(), vec![line]);
        }
    }
}
