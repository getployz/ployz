//! Linux signals as Docker names them, and which of them stop a container.

use std::str::FromStr;

use bollard::models::ContainerInspectResponse;

pub(super) fn stop_signal(inspected: &ContainerInspectResponse) -> Option<Signal> {
    inspected
        .config
        .as_ref()?
        .stop_signal
        .as_deref()?
        .parse()
        .ok()
}

/// A signal that stops a container: the image's `StopSignal`, or one
/// `docker stop`, `docker kill` or a terminal sends. Any other signal, such as
/// `HUP` to reload or `CONT`, leaves the run going.
pub(super) fn is_stop_signal(signal: Signal, stop_signal: Option<Signal>) -> bool {
    [Signal::TERM, Signal::KILL, Signal::INT, Signal::QUIT].contains(&signal)
        || Some(signal) == stop_signal
}

/// A Linux signal number. Realtime signals such as systemd's
/// `SIGRTMIN+3` have no name in libc, so signals compare as numbers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Signal(u8);

#[derive(Debug, thiserror::Error)]
#[error("Docker names no such signal.")]
pub(super) struct UnknownSignal;

impl Signal {
    const INT: Self = Self(2);
    const QUIT: Self = Self(3);
    const KILL: Self = Self(9);
    const TERM: Self = Self(15);
    const RTMIN: u8 = 34;
    const RTMAX: u8 = 64;
    /// moby's `pkg/signal` map for Linux, less the realtime range.
    const NAMES: [(&str, u8); 34] = [
        ("ABRT", 6),
        ("ALRM", 14),
        ("BUS", 7),
        ("CHLD", 17),
        ("CLD", 17),
        ("CONT", 18),
        ("FPE", 8),
        ("HUP", 1),
        ("ILL", 4),
        ("INT", 2),
        ("IO", 29),
        ("IOT", 6),
        ("KILL", 9),
        ("PIPE", 13),
        ("POLL", 29),
        ("PROF", 27),
        ("PWR", 30),
        ("QUIT", 3),
        ("SEGV", 11),
        ("STKFLT", 16),
        ("STOP", 19),
        ("SYS", 31),
        ("TERM", 15),
        ("TRAP", 5),
        ("TSTP", 20),
        ("TTIN", 21),
        ("TTOU", 22),
        ("URG", 23),
        ("USR1", 10),
        ("USR2", 12),
        ("VTALRM", 26),
        ("WINCH", 28),
        ("XCPU", 24),
        ("XFSZ", 25),
    ];

    pub(super) fn from_number(number: i64) -> Option<Self> {
        let number = u8::try_from(number).ok()?;
        (1..=Self::RTMAX).contains(&number).then_some(Self(number))
    }

    /// `RTMIN`, `RTMIN+1` to `RTMIN+15`, `RTMAX-14` to `RTMAX-1` and `RTMAX`.
    fn realtime(name: &str) -> Option<Self> {
        let offset = |rest: &str, sign: char| match rest {
            "" => Some(0),
            _ => rest.strip_prefix(sign)?.parse::<u8>().ok(),
        };
        let number = if let Some(rest) = name.strip_prefix("RTMIN") {
            let offset = offset(rest, '+')?;
            (offset <= 15).then(|| Self::RTMIN + offset)?
        } else {
            let offset = offset(name.strip_prefix("RTMAX")?, '-')?;
            (offset <= 14).then(|| Self::RTMAX - offset)?
        };
        Some(Self(number))
    }
}

/// Docker gives a signal by number in events and by name, with or without
/// `SIG`, in `Config.StopSignal`.
impl FromStr for Signal {
    type Err = UnknownSignal;

    fn from_str(signal: &str) -> Result<Self, Self::Err> {
        let name = signal.trim().to_ascii_uppercase();
        if let Ok(number) = name.parse::<i64>() {
            return Self::from_number(number).ok_or(UnknownSignal);
        }
        let name = name.strip_prefix("SIG").unwrap_or(&name);
        Self::NAMES
            .iter()
            .find(|(known, _)| *known == name)
            .map(|&(_, number)| Self(number))
            .or_else(|| Self::realtime(name))
            .ok_or(UnknownSignal)
    }
}
