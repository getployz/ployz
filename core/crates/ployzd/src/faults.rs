//! Fault injection for verify-cluster.

#[cfg(feature = "verify-faults")]
pub(crate) use enabled::{apply, check_env};
#[cfg(feature = "verify-faults")]
pub use enabled::{hold_task, kill_after_record, kill_inside, leak_memory};

/// Verbs whose effect outlives the RPC; `kill-daemon` fires inside them, from [`kill_inside`].
#[cfg_attr(not(feature = "verify-faults"), allow(dead_code))]
const LONG_EFFECTS: [&str; 4] = ["StartReceive", "Close", "Promote", "StartHandedContainer"];

#[cfg(not(feature = "verify-faults"))]
pub(crate) async fn apply(_verb: &'static str) {}

#[cfg(not(feature = "verify-faults"))]
pub fn kill_inside(_verb: &'static str) {}

#[cfg(not(feature = "verify-faults"))]
pub fn kill_after_record(_verb: &'static str) {}

#[cfg(not(feature = "verify-faults"))]
pub async fn hold_task(_verb: &'static str) {}

#[cfg(not(feature = "verify-faults"))]
pub fn leak_memory() {}

#[cfg(not(feature = "verify-faults"))]
pub(crate) fn check_env() -> std::io::Result<()> {
    Ok(())
}

#[cfg(feature = "verify-faults")]
mod enabled {
    use std::{io, str::FromStr, time::Duration};

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
        /// Aborts the Volume plugin once `verb` recorded its position, before its effect.
        KillAfterRecord {
            verb: String,
        },
        /// Holds `verb`'s spawned task for `secs` before it takes the mutation lock.
        HoldTask {
            verb: String,
            secs: u64,
        },
        Unanswered,
        /// Grows the process by `mib_per_sec` MiB of touched memory every second, forever.
        LeakMemory {
            mib_per_sec: usize,
        },
    }

    #[derive(Debug, thiserror::Error)]
    #[error(
        "{ENV}={0:?} is not a fault point (delay-rpc:<verb>:<secs>, kill-daemon:<verb>, kill-after-record:<verb>, hold-task:<verb>:<secs>, unanswered, leak-memory:<mib-per-sec>)"
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
                (Some("kill-after-record"), Some(verb), None, None) if !verb.is_empty() => {
                    FaultPoint::KillAfterRecord {
                        verb: verb.to_owned(),
                    }
                }
                (Some("hold-task"), Some(verb), Some(secs), None) if !verb.is_empty() => {
                    FaultPoint::HoldTask {
                        verb: verb.to_owned(),
                        secs: secs.parse().map_err(|_| malformed())?,
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

    pub(crate) async fn apply(verb: &'static str) {
        let Ok(Some(fault)) = FaultPoint::from_env() else {
            return;
        };
        match fault {
            FaultPoint::DelayRpc { verb: wanted, secs } if wanted == verb => {
                tracing::warn!(verb, secs, "fault: delaying switch verb");
                tokio::time::sleep(Duration::from_secs(secs)).await;
            }
            FaultPoint::KillDaemon { verb: wanted }
                if wanted == verb && !super::LONG_EFFECTS.contains(&verb) =>
            {
                tracing::warn!(verb, "fault: killing daemon");
                std::process::abort();
            }
            FaultPoint::Unanswered => {
                tracing::warn!(verb, "fault: leaving switch verb unanswered");
                std::future::pending::<()>().await;
            }
            FaultPoint::DelayRpc { .. }
            | FaultPoint::KillDaemon { .. }
            | FaultPoint::KillAfterRecord { .. }
            | FaultPoint::HoldTask { .. }
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

    /// Sleeps inside `verb`'s spawned task when `hold-task:<verb>:<secs>` names it.
    pub async fn hold_task(verb: &'static str) {
        if let Ok(Some(FaultPoint::HoldTask { verb: wanted, secs })) = FaultPoint::from_env()
            && wanted == verb
        {
            tracing::warn!(verb, secs, "fault: holding the task");
            tokio::time::sleep(Duration::from_secs(secs)).await;
        }
    }

    /// Aborts the process running `verb`'s effect when `kill-daemon:<verb>` names it.
    pub fn kill_inside(verb: &'static str) {
        if let Ok(Some(FaultPoint::KillDaemon { verb: wanted })) = FaultPoint::from_env()
            && wanted == verb
        {
            tracing::warn!(verb, "fault: killing daemon inside the effect");
            std::process::abort();
        }
    }

    /// Aborts the Volume plugin when `kill-after-record:<verb>` names `verb`.
    pub fn kill_after_record(verb: &'static str) {
        if let Ok(Some(FaultPoint::KillAfterRecord { verb: wanted })) = FaultPoint::from_env()
            && wanted == verb
        {
            tracing::warn!(
                verb,
                "fault: killing the plugin after the record, before the effect"
            );
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
                    "delay-rpc:AdoptLease:30",
                    FaultPoint::DelayRpc {
                        verb: "AdoptLease".into(),
                        secs: 30,
                    },
                ),
                (
                    "kill-daemon:InspectVolumeCopy",
                    FaultPoint::KillDaemon {
                        verb: "InspectVolumeCopy".into(),
                    },
                ),
                (
                    "kill-after-record:WarmSnapshot",
                    FaultPoint::KillAfterRecord {
                        verb: "WarmSnapshot".into(),
                    },
                ),
                (
                    "hold-task:Promote:610",
                    FaultPoint::HoldTask {
                        verb: "Promote".into(),
                        secs: 610,
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
                "delay-rpc:AdoptLease",
                "delay-rpc:AdoptLease:soon",
                "delay-rpc::3",
                "kill-daemon",
                "kill-daemon:AdoptLease:extra",
                "kill-after-record",
                "hold-task:Promote",
                "hold-task:Promote:later",
                "busy-mount:data",
                "unanswered:AdoptLease",
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
