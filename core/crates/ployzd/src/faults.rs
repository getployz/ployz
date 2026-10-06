//! Fault injection for verify-cluster.

#[cfg(feature = "verify-faults")]
pub use enabled::kill_inside;
#[cfg(feature = "verify-faults")]
pub(crate) use enabled::{apply, check_env};

/// Verbs whose effect outlives the RPC; `kill-daemon` fires inside them, from [`kill_inside`].
#[cfg_attr(not(feature = "verify-faults"), allow(dead_code))]
const LONG_EFFECTS: [&str; 1] = ["StartReceive"];

#[cfg(not(feature = "verify-faults"))]
pub(crate) async fn apply(_verb: &'static str) {}

#[cfg(not(feature = "verify-faults"))]
pub fn kill_inside(_verb: &'static str) {}

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
        DelayRpc { verb: String, secs: u64 },
        KillDaemon { verb: String },
        Unanswered,
    }

    #[derive(Debug, thiserror::Error)]
    #[error(
        "{ENV}={0:?} is not a fault point (delay-rpc:<verb>:<secs>, kill-daemon:<verb>, unanswered)"
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
            FaultPoint::DelayRpc { .. } | FaultPoint::KillDaemon { .. } => {}
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
                ("unanswered", FaultPoint::Unanswered),
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
                "busy-mount:data",
                "unanswered:AdoptLease",
                "explode",
            ] {
                assert!(value.parse::<FaultPoint>().is_err(), "{value:?}");
            }
        }
    }
}
