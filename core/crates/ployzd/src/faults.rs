//! Fault injection for verify-cluster.

use ployz_core::{DockerVolumeName, SnapshotGuid, SnapshotName, WantedCopy, WriterEpoch};

#[cfg(feature = "verify-faults")]
pub(crate) use enabled::{apply, check_env};
#[cfg(feature = "verify-faults")]
pub use enabled::{kill_inside, leak_memory};

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

/// The Volume a fault fires for, by physical name, and the want it serves. The verify
/// harness credits a fault line to a run by these fields, not by when it was logged.
#[derive(Clone, Copy, Debug)]
pub struct Identity<'a> {
    pub volume: &'a DockerVolumeName,
    pub want: Want<'a>,
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
}

impl<'a> Identity<'a> {
    #[must_use]
    pub fn new(volume: &'a DockerVolumeName, want: &'a WantedCopy) -> Self {
        let want = match want {
            WantedCopy::Writer { at, epoch, .. } => Want::Writer {
                at: *at,
                epoch: *epoch,
            },
            WantedCopy::Mirror { target, .. } => Want::Mirror { target },
            WantedCopy::Snapshot { name } => Want::Snapshot { snapshot: name },
            WantedCopy::Forget => Want::Forget,
        };
        Self { volume, want }
    }
}

#[cfg_attr(not(feature = "verify-faults"), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
enum Act {
    Delay { secs: u64 },
    Kill,
    KillInside,
    Unanswered,
}

#[cfg_attr(not(feature = "verify-faults"), allow(dead_code))]
fn warn(act: Act, verb: &'static str, identity: Option<Identity<'_>>) {
    let (message, secs) = match act {
        Act::Delay { secs } => ("fault: delaying switch verb", Some(secs)),
        Act::Kill => ("fault: killing daemon", None),
        Act::KillInside => ("fault: killing daemon inside the effect", None),
        Act::Unanswered => ("fault: leaving switch verb unanswered", None),
    };
    let (target, snapshot, writer) = match identity.map(|identity| identity.want) {
        Some(Want::Mirror { target }) => (Some(target), None, None),
        Some(Want::Snapshot { snapshot }) => (None, Some(snapshot), None),
        Some(Want::Writer { at, epoch }) => (None, None, Some((at, epoch))),
        Some(Want::Forget) | None => (None, None, None),
    };
    tracing::warn!(
        verb,
        secs,
        volume = identity.map(|identity| display(identity.volume)),
        target = target.map(display),
        snapshot = snapshot.map(display),
        at = writer.map(|(at, _)| display(at)),
        epoch = writer.map(|(_, epoch)| display(epoch)),
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
pub(crate) fn check_env() -> std::io::Result<()> {
    Ok(())
}

#[cfg(feature = "verify-faults")]
mod enabled {
    use std::{io, str::FromStr, time::Duration};

    use super::{Act, Identity, warn};

    pub(crate) const ENV: &str = "PLOYZD_FAULT";

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
        "{ENV}={0:?} is not a fault point (delay-rpc:<verb>:<secs>, kill-daemon:<verb>, unanswered, leak-memory:<mib-per-sec>)"
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

    impl FaultPoint {
        pub(crate) fn from_env() -> Result<Option<Self>, FaultParseError> {
            match std::env::var(ENV) {
                Ok(value) => value.parse().map(Some),
                Err(_) => Ok(None),
            }
        }
    }

    /// Refuses to start a daemon whose fault is misspelled, so a run cannot pass without it.
    pub(crate) fn check_env() -> io::Result<()> {
        FaultPoint::from_env().map(|_| ()).map_err(io::Error::other)
    }

    pub(crate) async fn apply(verb: &'static str, identity: Option<Identity<'_>>) {
        let Ok(Some(fault)) = FaultPoint::from_env() else {
            return;
        };
        match fault {
            FaultPoint::DelayRpc { verb: wanted, secs } if wanted == verb => {
                warn(Act::Delay { secs }, verb, identity);
                tokio::time::sleep(Duration::from_secs(secs)).await;
            }
            FaultPoint::KillDaemon { verb: wanted }
                if wanted == verb && !super::LONG_EFFECTS.contains(&verb) =>
            {
                warn(Act::Kill, verb, identity);
                std::process::abort();
            }
            FaultPoint::Unanswered => {
                warn(Act::Unanswered, verb, identity);
                std::future::pending::<()>().await;
            }
            FaultPoint::DelayRpc { .. }
            | FaultPoint::KillDaemon { .. }
            | FaultPoint::LeakMemory { .. } => {}
        }
    }

    /// Starts a thread that leaks memory when `leak-memory:<mib-per-sec>` is set, so a run
    /// can watch the kernel kill the process at its unit's `MemoryMax`.
    pub fn leak_memory() {
        let Ok(Some(FaultPoint::LeakMemory { mib_per_sec })) = FaultPoint::from_env() else {
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
        if let Ok(Some(FaultPoint::KillDaemon { verb: wanted })) = FaultPoint::from_env()
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
        fn fault_points_parse_their_env_form() {
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
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{DockerVolumeName, SnapshotGuid, SnapshotName, WriterEpoch};

    use super::{Act, Identity, Want, warn};

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
                r#" WARN fault: delaying switch verb verb="SetVolumeCopy.MirrorToMirror" secs=6 volume=app_data target=w-1-2"#,
            ),
            (
                Act::Delay { secs: 20 },
                "SetVolumeCopy.MirrorToWriter",
                Some(writer),
                r#" WARN fault: delaying switch verb verb="SetVolumeCopy.MirrorToWriter" secs=20 volume=app_data at=7310442 epoch=1"#,
            ),
            (
                delay,
                "SetVolumeCopy.WriterToSnapshot",
                Some(Want::Snapshot {
                    snapshot: &snapshot,
                }),
                r#" WARN fault: delaying switch verb verb="SetVolumeCopy.WriterToSnapshot" secs=6 volume=app_data snapshot=w-1-1"#,
            ),
            (
                delay,
                "SetVolumeCopy.WriterToForget",
                Some(Want::Forget),
                r#" WARN fault: delaying switch verb verb="SetVolumeCopy.WriterToForget" secs=6 volume=app_data"#,
            ),
            (
                Act::KillInside,
                "SetVolumeCopy.MirrorToMirror",
                Some(Want::Mirror { target: &round }),
                r#" WARN fault: killing daemon inside the effect verb="SetVolumeCopy.MirrorToMirror" volume=app_data target=w-1-2"#,
            ),
            (
                Act::KillInside,
                "SetVolumeCopy.MirrorToWriter",
                Some(writer),
                r#" WARN fault: killing daemon inside the effect verb="SetVolumeCopy.MirrorToWriter" volume=app_data at=7310442 epoch=1"#,
            ),
            (
                Act::KillInside,
                "SetVolumeCopy.WriterToSnapshot",
                Some(Want::Snapshot {
                    snapshot: &snapshot,
                }),
                r#" WARN fault: killing daemon inside the effect verb="SetVolumeCopy.WriterToSnapshot" volume=app_data snapshot=w-1-1"#,
            ),
            (
                Act::KillInside,
                "SetVolumeCopy.WriterToForget",
                Some(Want::Forget),
                r#" WARN fault: killing daemon inside the effect verb="SetVolumeCopy.WriterToForget" volume=app_data"#,
            ),
            (
                Act::Kill,
                "SetVolumeCopy.WriterToWriter",
                Some(writer),
                r#" WARN fault: killing daemon verb="SetVolumeCopy.WriterToWriter" volume=app_data at=7310442 epoch=1"#,
            ),
            (
                Act::Unanswered,
                "SetVolumeCopy",
                Some(Want::Mirror { target: &round }),
                r#" WARN fault: leaving switch verb unanswered verb="SetVolumeCopy" volume=app_data target=w-1-2"#,
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
            });
            warn(act, verb, identity);
            assert_eq!(capture.text().lines().collect::<Vec<_>>(), vec![line]);
        }
    }
}
