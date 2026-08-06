use std::collections::BTreeMap;

use ployz_core::network::WireGuardPublicKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinWireguardHostOutcome {
    pub ebpf: EbpfHostOutcome,
    pub latest_handshakes: BTreeMap<WireGuardPublicKey, BuiltinWireguardLatestHandshake>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinWireguardLatestHandshake {
    Never,
    AtUnixSeconds { unix_seconds: u64 },
}

impl BuiltinWireguardLatestHandshake {
    pub(super) fn parse_dump_field(value: &str) -> Result<Self, String> {
        match value.parse::<u64>() {
            Ok(0) => Ok(Self::Never),
            Ok(unix_seconds) => Ok(Self::AtUnixSeconds { unix_seconds }),
            Err(error) => Err(format!(
                "parse observed WireGuard latest handshake {value:?}: {error}"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EbpfHostOutcome {
    Ready { route_count: usize },
    Degraded { reason: EbpfDegradedReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EbpfDegradedReason {
    MissingBridge { ifname: String },
    HostEffect { message: String },
}

#[derive(Debug)]
pub(super) enum EbpfEffectError {
    MissingBridge,
    HostEffect(String),
}

pub(super) fn ebpf_outcome(
    route_count: usize,
    bridge_ifname: &str,
    result: Result<(), EbpfEffectError>,
) -> EbpfHostOutcome {
    match result {
        Ok(()) => EbpfHostOutcome::Ready { route_count },
        Err(EbpfEffectError::MissingBridge) => EbpfHostOutcome::Degraded {
            reason: EbpfDegradedReason::MissingBridge {
                ifname: bridge_ifname.to_owned(),
            },
        },
        Err(EbpfEffectError::HostEffect(message)) => EbpfHostOutcome::Degraded {
            reason: EbpfDegradedReason::HostEffect { message },
        },
    }
}
