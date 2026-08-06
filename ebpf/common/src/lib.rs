#![no_std]

#[cfg(any(feature = "validation", test))]
extern crate std;

#[cfg(feature = "validation")]
use object::{Object, ObjectSymbol};
#[cfg(feature = "validation")]
use std::borrow::ToOwned;
#[cfg(feature = "validation")]
use std::string::ToString;

pub const REQUIRED_TC_SYMBOLS: [&str; 4] =
    ["ployz_egress", "ployz_ingress", "ROUTES", "WG_IFINDEX"];

pub const REQUIRED_ISOLATION_SYMBOLS: [&str; 4] = [
    "ployz_cgroup_egress",
    "ployz_cgroup_ingress",
    "ISOLATION_CONFIG",
    "ISOLATION_NAMESPACES",
];

pub const MAX_ROUTES: u32 = 256;
pub const MAX_ISOLATION_ENTRIES: u32 = 65_536;
pub const ISOLATION_MODE_DISABLED: u32 = 0;
pub const ISOLATION_MODE_ARMED: u32 = 1;
pub const ISOLATION_PREFIX_LEN: u32 = 16;

/// Default BPF filesystem pin directory for the Ployz TC programs and maps.
pub const DEFAULT_PIN_PATH: &str = "/sys/fs/bpf/ployz";

/// Stable `ployz-ebpf-ctl status` exit code for a known detached state.
pub const EBPF_STATUS_DETACHED_EXIT_CODE: u8 = 2;

/// BPF map key for an IPv4 network prefix.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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

/// BPF map key containing an IPv4 address in network byte order.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContainerIp {
    pub address: u32,
}

/// Canonical textual Corrosion namespace row id.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NamespaceRowId {
    pub bytes: [u8; 26],
}

/// Padded BPF map value for one container's namespace.
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug)]
pub struct NamespaceTag {
    pub namespace_id: NamespaceRowId,
    pub padding: [u8; 2],
}

impl PartialEq for NamespaceTag {
    fn eq(&self, other: &Self) -> bool {
        self.namespace_id == other.namespace_id
    }
}

impl Eq for NamespaceTag {}

/// Single-entry isolation configuration shared with the cgroup programs.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IsolationConfig {
    pub mode: u32,
    /// IPv4 `/16` network address in network byte order.
    pub network: u32,
    pub prefix_len: u32,
}

impl IsolationConfig {
    /// Returns whether this is a well-formed armed configuration.
    #[must_use]
    pub const fn is_armed(&self) -> bool {
        if self.mode != ISOLATION_MODE_ARMED || self.prefix_len != ISOLATION_PREFIX_LEN {
            return false;
        }
        u32::from_be(self.network) & 0x0000_ffff == 0
    }

    #[inline(always)]
    const fn contains(&self, ip: ContainerIp) -> bool {
        (u32::from_be(ip.address) & 0xffff_0000) == u32::from_be(self.network)
    }

    #[inline(always)]
    const fn is_host(&self, ip: ContainerIp) -> bool {
        self.contains(ip) && (u32::from_be(ip.address) & 0xff) == 1
    }
}

/// Applies the fixed namespace isolation policy to one IPv4 packet.
///
/// The caller supplies map lookups for both packet endpoints. Disabled or
/// malformed configuration passes all traffic.
#[must_use]
#[inline(always)]
pub fn isolation_packet_allowed(
    config: &IsolationConfig,
    source: ContainerIp,
    source_namespace: Option<&NamespaceTag>,
    destination: ContainerIp,
    destination_namespace: Option<&NamespaceTag>,
) -> bool {
    if !config.is_armed() {
        return true;
    }

    if config.is_host(source) || config.is_host(destination) {
        return true;
    }

    if !config.contains(source) || !config.contains(destination) {
        return true;
    }

    match (source_namespace, destination_namespace) {
        (Some(source), Some(destination)) => source == destination,
        (Some(_), None) | (None, Some(_)) | (None, None) => false,
    }
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
pub fn validate_ployz_isolation_bytecode(
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
    for symbol in REQUIRED_ISOLATION_SYMBOLS {
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

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, size_of};
    use core::str::FromStr;

    #[derive(Clone, Copy)]
    struct Endpoint {
        ip: ContainerIp,
        namespace: Option<NamespaceTag>,
    }

    #[test]
    fn isolation_layout_and_capacity_are_stable() {
        assert_eq!(MAX_ISOLATION_ENTRIES, 65_536);
        assert_eq!(size_of::<ContainerIp>(), 4);
        assert_eq!(align_of::<ContainerIp>(), 4);
        assert_eq!(size_of::<NamespaceRowId>(), 26);
        assert_eq!(align_of::<NamespaceRowId>(), 1);
        assert_eq!(size_of::<NamespaceTag>(), 28);
        assert_eq!(align_of::<NamespaceTag>(), 4);
        assert_eq!(size_of::<IsolationConfig>(), 12);
        assert_eq!(align_of::<IsolationConfig>(), 4);
    }

    #[test]
    fn ordered_endpoint_matrix_enforces_the_fixed_policy() {
        let namespace_a = namespace_tag(b'a');
        let namespace_b = namespace_tag(b'b');
        let endpoints = [
            Endpoint {
                ip: container_ip("10.210.7.1"),
                namespace: Some(namespace_b),
            },
            Endpoint {
                ip: container_ip("192.0.2.10"),
                namespace: Some(namespace_a),
            },
            Endpoint {
                ip: container_ip("10.210.7.2"),
                namespace: Some(namespace_a),
            },
            Endpoint {
                ip: container_ip("10.210.8.2"),
                namespace: Some(namespace_a),
            },
            Endpoint {
                ip: container_ip("10.210.7.3"),
                namespace: Some(namespace_b),
            },
            Endpoint {
                ip: container_ip("10.210.7.4"),
                namespace: None,
            },
        ];

        for (source_index, source) in endpoints.iter().enumerate() {
            for (destination_index, destination) in endpoints.iter().enumerate() {
                let host_or_outside = source_index < 2 || destination_index < 2;
                let same_known_namespace = matches!(
                    (source.namespace, destination.namespace),
                    (Some(source), Some(destination)) if source == destination
                );
                let expected = host_or_outside || same_known_namespace;
                assert_eq!(
                    isolation_packet_allowed(
                        &armed_config(),
                        source.ip,
                        source.namespace.as_ref(),
                        destination.ip,
                        destination.namespace.as_ref(),
                    ),
                    expected,
                    "source {source_index}, destination {destination_index}",
                );
            }
        }
    }

    #[test]
    fn every_non_gateway_unknown_inside_the_prefix_drops() {
        for unknown in ["10.210.7.0", "10.210.7.254", "10.210.7.255"] {
            let unknown = container_ip(unknown);
            assert!(!isolation_packet_allowed(
                &armed_config(),
                unknown,
                None,
                container_ip("10.210.8.9"),
                None,
            ));
        }
    }

    #[test]
    fn disabled_and_malformed_configs_pass_unknown_traffic() {
        let configs = [
            IsolationConfig {
                mode: ISOLATION_MODE_DISABLED,
                network: container_ip("10.210.0.0").address,
                prefix_len: ISOLATION_PREFIX_LEN,
            },
            IsolationConfig {
                mode: 77,
                network: container_ip("10.210.0.0").address,
                prefix_len: ISOLATION_PREFIX_LEN,
            },
            IsolationConfig {
                mode: ISOLATION_MODE_ARMED,
                network: container_ip("10.210.0.0").address,
                prefix_len: 24,
            },
            IsolationConfig {
                mode: ISOLATION_MODE_ARMED,
                network: container_ip("10.210.1.0").address,
                prefix_len: ISOLATION_PREFIX_LEN,
            },
        ];

        for config in configs {
            assert!(isolation_packet_allowed(
                &config,
                container_ip("10.210.7.2"),
                None,
                container_ip("10.210.8.2"),
                None,
            ));
        }
    }

    #[test]
    fn armed_config_drops_unknown_traffic() {
        assert!(!isolation_packet_allowed(
            &armed_config(),
            container_ip("10.210.7.2"),
            None,
            container_ip("10.210.8.2"),
            None,
        ));
    }

    fn armed_config() -> IsolationConfig {
        IsolationConfig {
            mode: ISOLATION_MODE_ARMED,
            network: container_ip("10.210.0.0").address,
            prefix_len: ISOLATION_PREFIX_LEN,
        }
    }

    fn container_ip(value: &str) -> ContainerIp {
        let address = std::net::Ipv4Addr::from_str(value).expect("valid test IPv4 address");
        ContainerIp {
            address: u32::from(address).to_be(),
        }
    }

    fn namespace_tag(byte: u8) -> NamespaceTag {
        NamespaceTag {
            namespace_id: NamespaceRowId { bytes: [byte; 26] },
            padding: [0; 2],
        }
    }

    #[cfg(feature = "validation")]
    #[test]
    fn tc_only_fixture_passes_tc_validation_and_fails_isolation_validation() {
        let fixture = fixture_with_symbols(&REQUIRED_TC_SYMBOLS);

        assert_eq!(
            validate_ployz_tc_bytecode(&fixture),
            Ok(REQUIRED_TC_SYMBOLS
                .iter()
                .map(ToString::to_string)
                .collect())
        );
        assert_eq!(
            validate_ployz_isolation_bytecode(&fixture),
            Err(BytecodeValidationError::MissingSymbol {
                symbol: REQUIRED_ISOLATION_SYMBOLS
                    .first()
                    .copied()
                    .expect("isolation symbol list is nonempty"),
            })
        );
    }

    #[cfg(feature = "validation")]
    fn fixture_with_symbols(symbols: &[&str]) -> std::vec::Vec<u8> {
        use object::write::{Object, StandardSegment, Symbol, SymbolSection};
        use object::{
            Architecture, BinaryFormat, Endianness, SectionKind, SymbolFlags, SymbolKind,
        };

        let mut object = Object::new(BinaryFormat::Elf, Architecture::Bpf, Endianness::Little);
        let section = object.add_section(
            object.segment_name(StandardSegment::Text).to_vec(),
            b".text".to_vec(),
            SectionKind::Text,
        );
        object.append_section_data(section, &[0; 8], 8);
        for symbol in symbols {
            object.add_symbol(Symbol {
                name: symbol.as_bytes().to_vec(),
                value: 0,
                size: 0,
                kind: SymbolKind::Text,
                scope: object::SymbolScope::Linkage,
                weak: false,
                section: SymbolSection::Section(section),
                flags: SymbolFlags::None,
            });
        }
        object.write().expect("write BPF ELF fixture")
    }
}
