#![no_std]

#[cfg(feature = "validation")]
extern crate std;

#[cfg(feature = "validation")]
use object::{Object, ObjectSymbol};
#[cfg(feature = "validation")]
use std::borrow::ToOwned;
#[cfg(feature = "validation")]
use std::string::ToString;

pub const REQUIRED_TC_SYMBOLS: [&str; 4] =
    ["ployz_egress", "ployz_ingress", "ROUTES", "WG_IFINDEX"];

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
