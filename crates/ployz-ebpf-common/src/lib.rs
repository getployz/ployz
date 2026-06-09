#![no_std]

#[cfg(feature = "validation")]
extern crate std;

#[cfg(feature = "validation")]
use object::{Object, ObjectSymbol};
#[cfg(feature = "validation")]
use std::borrow::ToOwned;
#[cfg(feature = "validation")]
use std::string::ToString;

pub const REQUIRED_TC_SYMBOLS: [&str; 6] = [
    "ployz_egress",
    "ployz_ingress",
    "ROUTES",
    "WG_IFINDEX",
    "OBSERVE_FLAG",
    "EVENTS",
];

/// BPF map key for an IPv4 network prefix.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RouteKey {
    pub network: u32,
    pub prefix_len: u32,
}

/// BPF map value for the interface that receives matching packets.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RouteEntry {
    pub ifindex: u32,
}

/// Raw packet event emitted by the eBPF observation tap.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PacketEvent {
    pub ts_ns: u64,
    pub src_addr: [u8; 16],
    pub dst_addr: [u8; 16],
    pub src_port: u16,
    pub dst_port: u16,
    pub pkt_len: u32,
    pub proto: u8,
    pub direction: u8,
    pub _pad: [u8; 2],
}

#[cfg(feature = "validation")]
pub fn validate_ployz_tc_bytecode(
    bytes: &[u8],
) -> Result<std::vec::Vec<std::string::String>, BytecodeValidationError> {
    let object = object::File::parse(bytes).map_err(|source| BytecodeValidationError::Parse {
        message: source.to_string(),
    })?;
    if object.architecture() != object::Architecture::Bpf {
        return Err(BytecodeValidationError::NotBpfObject);
    }

    let symbols = object
        .symbols()
        .filter_map(|symbol| symbol.name().ok())
        .collect::<std::collections::BTreeSet<_>>();
    let mut found = std::vec::Vec::new();
    for symbol in REQUIRED_TC_SYMBOLS {
        if !symbols.contains(symbol) {
            return Err(BytecodeValidationError::MissingSymbol { symbol });
        }
        found.push(symbol.to_owned());
    }

    Ok(found)
}

#[cfg(feature = "validation")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BytecodeValidationError {
    Parse { message: std::string::String },
    NotBpfObject,
    MissingSymbol { symbol: &'static str },
}
