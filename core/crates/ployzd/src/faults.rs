//! Fault injection for verify-cluster.

#[cfg(feature = "verify-faults")]
pub(crate) use enabled::{apply, check};
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

#[cfg(not(feature = "verify-faults"))]
pub(crate) async fn apply(_verb: &'static str) {}

#[cfg(not(feature = "verify-faults"))]
pub fn kill_inside(_verb: &'static str) {}

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
        path::Path,
        str::FromStr,
        time::{Duration, Instant},
    };

    /// Read on every check, so a run arms and disarms a fault without restarting a daemon.
    const PATH: &str = "/run/ployz/fault";
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

    fn armed() -> Option<FaultPoint> {
        read(Path::new(PATH)).ok().flatten()
    }

    /// Refuses to start a daemon whose fault is misspelled, so a run cannot pass without it.
    pub(crate) fn check() -> io::Result<()> {
        read(Path::new(PATH)).map(|_| ())
    }

    pub(crate) async fn apply(verb: &'static str) {
        let Some(fault) = armed() else {
            return;
        };
        match &fault {
            FaultPoint::DelayRpc { verb: wanted, secs } if wanted == verb => {
                tracing::warn!(verb, secs, "fault: delaying switch verb");
                hold(Path::new(PATH), &fault, Some(Duration::from_secs(*secs))).await;
            }
            FaultPoint::KillDaemon { verb: wanted }
                if wanted == verb && !super::LONG_EFFECTS.contains(&verb) =>
            {
                tracing::warn!(verb, "fault: killing daemon");
                std::process::abort();
            }
            FaultPoint::Unanswered => {
                tracing::warn!(verb, "fault: leaving switch verb unanswered");
                hold(Path::new(PATH), &fault, None).await;
            }
            FaultPoint::DelayRpc { .. }
            | FaultPoint::KillDaemon { .. }
            | FaultPoint::LeakMemory { .. } => {}
        }
    }

    /// Holds while `path` still names `fault`, so disarming frees the held verb and the
    /// switch lock it may own.
    async fn hold(path: &Path, fault: &FaultPoint, limit: Option<Duration>) {
        let until = limit.map(|limit| Instant::now() + limit);
        loop {
            let left = until.map(|until| until.saturating_duration_since(Instant::now()));
            if left == Some(Duration::ZERO) {
                return;
            }
            tokio::time::sleep(left.map_or(POLL, |left| left.min(POLL))).await;
            if read(path).ok().flatten().as_ref() != Some(fault) {
                return;
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
    pub fn kill_inside(verb: &'static str) {
        if let Some(FaultPoint::KillDaemon { verb: wanted }) = armed()
            && wanted == verb
        {
            tracing::warn!(verb, "fault: killing daemon inside the effect");
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
            tokio::time::timeout(3 * POLL, held).await.unwrap().unwrap();
        }
    }
}
