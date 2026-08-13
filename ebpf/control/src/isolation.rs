use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

const CONFIG_PIN: &str = "config";
const NAMESPACES_PIN: &str = "namespaces";
const INGRESS_PROGRAM_PIN: &str = "ingress";
const EGRESS_PROGRAM_PIN: &str = "egress";
const INGRESS_LINK_PIN: &str = "ingress-link";
const EGRESS_LINK_PIN: &str = "egress-link";

const OWNED_PINS: [&str; 6] = [
    CONFIG_PIN,
    NAMESPACES_PIN,
    INGRESS_PROGRAM_PIN,
    EGRESS_PROGRAM_PIN,
    INGRESS_LINK_PIN,
    EGRESS_LINK_PIN,
];

fn has_exact_owned_pin_names(observed: &BTreeSet<std::ffi::OsString>) -> bool {
    let expected = OWNED_PINS
        .iter()
        .map(std::ffi::OsString::from)
        .collect::<BTreeSet<_>>();
    observed == &expected
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum IsolationStatus {
    Complete,
    Detached { message: String },
}

pub(super) fn sibling_pin_path(tc_pin_path: &str) -> Result<PathBuf, String> {
    let tc_pin_path = Path::new(tc_pin_path);
    let Some(name) = tc_pin_path.file_name().and_then(|name| name.to_str()) else {
        return Err(format!(
            "eBPF pin path must end in a UTF-8 directory name: {}",
            tc_pin_path.display()
        ));
    };
    let Some(parent) = tc_pin_path.parent() else {
        return Err(format!(
            "eBPF pin path must have a parent directory: {}",
            tc_pin_path.display()
        ));
    };
    Ok(parent.join(format!("{name}-isolation")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DesiredNamespace {
    address: u32,
    namespace_id: [u8; 64],
}

fn parse_rows(contents: &str) -> Result<Vec<DesiredNamespace>, String> {
    let mut desired = BTreeMap::<u32, [u8; 64]>::new();
    for (index, line) in contents.lines().enumerate() {
        let line_number = index + 1;
        let mut fields = line.split_whitespace();
        let Some(address) = fields.next() else {
            continue;
        };
        let Some(namespace_id) = fields.next() else {
            return Err(format!(
                "isolation rows line {line_number} must contain an IPv4 address and namespace name"
            ));
        };
        if fields.next().is_some() {
            return Err(format!(
                "isolation rows line {line_number} contains unexpected fields"
            ));
        }
        let address = address.parse::<Ipv4Addr>().map_err(|error| {
            format!("invalid isolation IPv4 address on line {line_number}: {error}")
        })?;
        let bytes = encode_namespace_name(namespace_id).map_err(|detail| {
            format!("invalid namespace name on isolation rows line {line_number}: {detail}")
        })?;
        let address = u32::from(address).to_be();
        match desired.entry(address) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(bytes);
            }
            std::collections::btree_map::Entry::Occupied(entry) if entry.get() == &bytes => {}
            std::collections::btree_map::Entry::Occupied(_) => {
                return Err(format!(
                    "isolation rows assign {} to conflicting namespaces",
                    Ipv4Addr::from(u32::from_be(address))
                ));
            }
        }
    }
    if desired.len() > ployz_ebpf_common::MAX_ISOLATION_ENTRIES as usize {
        return Err(format!(
            "desired isolation row count {} exceeds map capacity {}",
            desired.len(),
            ployz_ebpf_common::MAX_ISOLATION_ENTRIES
        ));
    }
    Ok(desired
        .into_iter()
        .map(|(address, namespace_id)| DesiredNamespace {
            address,
            namespace_id,
        })
        .collect())
}

fn validate_desired_prefix(
    desired: &[DesiredNamespace],
    prefix: ipnet::Ipv4Net,
) -> Result<(), String> {
    for row in desired {
        let address = Ipv4Addr::from(u32::from_be(row.address));
        let [_, _, _, host_octet] = address.octets();
        if !prefix.contains(&address) || !(2..=254).contains(&host_octet) {
            return Err(format!(
                "isolation address {address} is not a container address in {prefix} (.2-.254)"
            ));
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct NamespaceReplacementPlan {
    remove: Vec<u32>,
    insert: Vec<DesiredNamespace>,
}

fn plan_replacement(
    observed: &[(u32, [u8; 64])],
    desired: &[DesiredNamespace],
) -> NamespaceReplacementPlan {
    let desired_by_address = desired
        .iter()
        .map(|row| (row.address, row.namespace_id))
        .collect::<BTreeMap<_, _>>();
    let observed_by_address = observed.iter().copied().collect::<BTreeMap<_, _>>();

    let remove = observed_by_address
        .iter()
        .filter_map(|(address, namespace_id)| {
            (desired_by_address.get(address) != Some(namespace_id)).then_some(*address)
        })
        .collect();
    let insert = desired
        .iter()
        .filter(|row| observed_by_address.get(&row.address) != Some(&row.namespace_id))
        .cloned()
        .collect();

    NamespaceReplacementPlan { remove, insert }
}

fn encode_namespace_name(value: &str) -> Result<[u8; 64], &'static str> {
    let source = value.as_bytes();
    let is_alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if source.is_empty() || source.len() > 63 {
        return Err("must contain 1 to 63 bytes");
    }
    if !source.first().copied().is_some_and(is_alphanumeric)
        || !source.last().copied().is_some_and(is_alphanumeric)
    {
        return Err("must start and end with a lowercase ASCII letter or digit");
    }
    if !source
        .iter()
        .all(|byte| is_alphanumeric(*byte) || *byte == b'-')
    {
        return Err("must be a lowercase DNS label");
    }
    let mut encoded = [0_u8; 64];
    encoded[0] = source.len() as u8;
    encoded
        .get_mut(1..=source.len())
        .ok_or("namespace encoding exceeds 64 bytes")?
        .copy_from_slice(source);
    Ok(encoded)
}

fn config_prefix(config: ployz_ebpf_common::IsolationConfig) -> Result<ipnet::Ipv4Net, String> {
    if config.mode != ployz_ebpf_common::ISOLATION_MODE_ARMED {
        return Err("isolation configuration is not armed".to_owned());
    }
    if config.prefix_len != ployz_ebpf_common::ISOLATION_PREFIX_LEN {
        return Err(format!(
            "pinned isolation prefix length {} is not /{}",
            config.prefix_len,
            ployz_ebpf_common::ISOLATION_PREFIX_LEN
        ));
    }
    if !config.is_armed() {
        return Err("pinned isolation network is not canonical for its /16 prefix".to_owned());
    }
    ipnet::Ipv4Net::new(
        Ipv4Addr::from(u32::from_be(config.network)),
        config.prefix_len as u8,
    )
    .map_err(|error| format!("invalid pinned isolation prefix: {error}"))
}

#[derive(Debug, PartialEq, Eq)]
enum ExistingConfigDisposition {
    Repairable,
    Matching,
    Conflicting { pinned: ipnet::Ipv4Net },
}

fn existing_config_disposition(
    config: ployz_ebpf_common::IsolationConfig,
    requested: ipnet::Ipv4Net,
) -> ExistingConfigDisposition {
    match config_prefix(config) {
        Ok(pinned) if pinned == requested => ExistingConfigDisposition::Matching,
        Ok(pinned) => ExistingConfigDisposition::Conflicting { pinned },
        Err(_) => ExistingConfigDisposition::Repairable,
    }
}

#[cfg(target_os = "linux")]
pub(super) mod linux {
    use super::*;
    use aya::Ebpf;
    use aya::programs::links::{FdLink, PinnedLink};
    use aya::programs::{CgroupAttachMode, CgroupSkb, CgroupSkbAttachType};
    use std::fs::File;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct PodContainerIp(ployz_ebpf_common::ContainerIp);

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct PodNamespaceTag(ployz_ebpf_common::NamespaceTag);

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct PodIsolationConfig(ployz_ebpf_common::IsolationConfig);

    unsafe impl aya::Pod for PodContainerIp {}
    unsafe impl aya::Pod for PodNamespaceTag {}
    unsafe impl aya::Pod for PodIsolationConfig {}

    pub(crate) fn ensure_attached(
        bytecode: &Path,
        prefix: ipnet::Ipv4Net,
        cgroup_root: &Path,
        pin_path: &Path,
    ) -> Result<(), String> {
        validate_cgroup_v2(cgroup_root)?;
        validate_fixed_prefix(prefix)?;
        validate_bytecode(bytecode)?;

        match read_config(pin_path) {
            Ok(Some(config)) => match existing_config_disposition(config, prefix) {
                ExistingConfigDisposition::Repairable | ExistingConfigDisposition::Matching => {}
                ExistingConfigDisposition::Conflicting { pinned } => {
                    return Err(format!(
                        "isolation prefix is fixed at initialization: pinned {pinned}, requested {prefix}"
                    ));
                }
            },
            Ok(None) => {}
            // An unreadable owned config is partial state, so the requested
            // prefix is installed only after all owned pins are removed.
            Err(_) => {}
        }

        if status_for_prefix(pin_path, prefix)? == IsolationStatus::Complete {
            return Ok(());
        }
        remove_owned_pins(pin_path)?;
        attach(bytecode, prefix, cgroup_root, pin_path)?;
        match status_for_prefix(pin_path, prefix)? {
            IsolationStatus::Complete => Ok(()),
            IsolationStatus::Detached { message } => Err(format!(
                "isolation attachment did not persist under {}: {message}",
                pin_path.display()
            )),
        }
    }

    pub(crate) fn status(pin_path: &Path) -> Result<IsolationStatus, String> {
        let config = match read_config(pin_path) {
            Ok(config) => config,
            Err(error) => return Ok(detached(pin_path, &error)),
        };
        let Some(config) = config else {
            return Ok(detached(pin_path, "configuration pin is missing"));
        };
        let prefix = match config_prefix(config) {
            Ok(prefix) => prefix,
            Err(error) => return Ok(detached(pin_path, &error)),
        };
        status_for_prefix(pin_path, prefix)
    }

    fn status_for_prefix(
        pin_path: &Path,
        expected_prefix: ipnet::Ipv4Net,
    ) -> Result<IsolationStatus, String> {
        let observed = match std::fs::read_dir(pin_path) {
            Ok(entries) => entries
                .map(|entry| {
                    entry
                        .map_err(|error| error.to_string())
                        .map(|entry| entry.file_name())
                })
                .collect::<Result<BTreeSet<_>, _>>()
                .map_err(|error| format!("list {}: {error}", pin_path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(detached(pin_path, "pin directory is missing"));
            }
            Err(error) => return Err(format!("list {}: {error}", pin_path.display())),
        };
        if !has_exact_owned_pin_names(&observed) {
            return Ok(detached(
                pin_path,
                "required isolation pins are incomplete or the pin directory contains foreign entries",
            ));
        }

        for name in [INGRESS_LINK_PIN, EGRESS_LINK_PIN] {
            if PinnedLink::from_pin(pin_path.join(name)).is_err() {
                return Ok(detached(pin_path, &format!("{name} is stale")));
            }
        }
        if CgroupSkb::from_pin(
            pin_path.join(INGRESS_PROGRAM_PIN),
            CgroupSkbAttachType::Ingress,
        )
        .is_err()
        {
            return Ok(detached(pin_path, "ingress program pin is stale"));
        }
        if CgroupSkb::from_pin(
            pin_path.join(EGRESS_PROGRAM_PIN),
            CgroupSkbAttachType::Egress,
        )
        .is_err()
        {
            return Ok(detached(pin_path, "egress program pin is stale"));
        }
        if open_namespaces_map(pin_path).is_err() {
            return Ok(detached(pin_path, "namespace map pin is stale"));
        }
        let config = match read_config(pin_path) {
            Ok(config) => config,
            Err(error) => return Ok(detached(pin_path, &error)),
        };
        let Some(config) = config else {
            return Ok(detached(pin_path, "configuration pin is missing"));
        };
        match config_prefix(config) {
            Ok(prefix) if prefix == expected_prefix => Ok(IsolationStatus::Complete),
            Ok(prefix) => Ok(detached(
                pin_path,
                &format!("pinned prefix {prefix} differs from expected {expected_prefix}"),
            )),
            Err(error) => Ok(detached(pin_path, &error)),
        }
    }

    pub(crate) fn replace_all(rows_file: &Path, pin_path: &Path) -> Result<(), String> {
        let contents = std::fs::read_to_string(rows_file)
            .map_err(|error| format!("read isolation rows {}: {error}", rows_file.display()))?;
        let desired = parse_rows(&contents)?;
        let config = read_config(pin_path)?.ok_or_else(|| {
            format!(
                "pinned isolation configuration is missing under {}",
                pin_path.display()
            )
        })?;
        let prefix = config_prefix(config)?;
        validate_desired_prefix(&desired, prefix)?;

        let mut map = open_namespaces_map(pin_path)?;
        let observed = map
            .iter()
            .map(|entry| {
                let (address, namespace) =
                    entry.map_err(|error| format!("list pinned isolation namespaces: {error}"))?;
                Ok((address.0.address, namespace.0.namespace_id.bytes))
            })
            .collect::<Result<Vec<_>, String>>()?;
        if observed.len() > ployz_ebpf_common::MAX_ISOLATION_ENTRIES as usize {
            return Err(format!(
                "observed isolation row count {} exceeds map capacity {}",
                observed.len(),
                ployz_ebpf_common::MAX_ISOLATION_ENTRIES
            ));
        }
        let NamespaceReplacementPlan { remove, insert } = plan_replacement(&observed, &desired);

        // Removing stale and namespace-changed entries first makes a partial
        // replacement fail closed: an old namespace can never retain access.
        for address in remove {
            map.remove(&PodContainerIp(ployz_ebpf_common::ContainerIp { address }))
                .map_err(|error| format!("remove stale isolation namespace: {error}"))?;
        }
        for row in insert {
            map.insert(
                PodContainerIp(ployz_ebpf_common::ContainerIp {
                    address: row.address,
                }),
                PodNamespaceTag(ployz_ebpf_common::NamespaceTag {
                    namespace_id: ployz_ebpf_common::NamespaceName {
                        bytes: row.namespace_id,
                    },
                }),
                0,
            )
            .map_err(|error| format!("insert desired isolation namespace: {error}"))?;
        }
        Ok(())
    }

    fn attach(
        bytecode: &Path,
        prefix: ipnet::Ipv4Net,
        cgroup_root: &Path,
        pin_path: &Path,
    ) -> Result<(), String> {
        let bytes = std::fs::read(bytecode)
            .map_err(|error| format!("read eBPF bytecode {}: {error}", bytecode.display()))?;
        let mut bpf = Ebpf::load(&bytes).map_err(|error| format!("load eBPF bytecode: {error}"))?;
        std::fs::create_dir_all(pin_path)
            .map_err(|error| format!("create {}: {error}", pin_path.display()))?;

        bpf.map_mut("ISOLATION_CONFIG")
            .ok_or("ISOLATION_CONFIG map not found")?
            .pin(pin_path.join(CONFIG_PIN))
            .map_err(|error| format!("pin ISOLATION_CONFIG map: {error}"))?;
        bpf.map_mut("ISOLATION_NAMESPACES")
            .ok_or("ISOLATION_NAMESPACES map not found")?
            .pin(pin_path.join(NAMESPACES_PIN))
            .map_err(|error| format!("pin ISOLATION_NAMESPACES map: {error}"))?;

        let mut config = open_config_map(pin_path)?;
        config
            .set(
                0,
                PodIsolationConfig(ployz_ebpf_common::IsolationConfig {
                    mode: ployz_ebpf_common::ISOLATION_MODE_ARMED,
                    network: u32::from(prefix.network()).to_be(),
                    prefix_len: u32::from(prefix.prefix_len()),
                }),
                0,
            )
            .map_err(|error| format!("arm ISOLATION_CONFIG: {error}"))?;
        drop(config);

        let cgroup = File::open(cgroup_root)
            .map_err(|error| format!("open cgroup root {}: {error}", cgroup_root.display()))?;
        attach_program(
            &mut bpf,
            &cgroup,
            "ployz_cgroup_ingress",
            CgroupSkbAttachType::Ingress,
            pin_path.join(INGRESS_PROGRAM_PIN),
            pin_path.join(INGRESS_LINK_PIN),
        )?;
        attach_program(
            &mut bpf,
            &cgroup,
            "ployz_cgroup_egress",
            CgroupSkbAttachType::Egress,
            pin_path.join(EGRESS_PROGRAM_PIN),
            pin_path.join(EGRESS_LINK_PIN),
        )?;
        Ok(())
    }

    fn attach_program(
        bpf: &mut Ebpf,
        cgroup: &File,
        name: &str,
        attach_type: CgroupSkbAttachType,
        program_pin: PathBuf,
        link_pin: PathBuf,
    ) -> Result<(), String> {
        let program: &mut CgroupSkb = bpf
            .program_mut(name)
            .ok_or_else(|| format!("{name} program not found"))?
            .try_into()
            .map_err(|error| format!("open {name}: {error}"))?;
        program
            .load()
            .map_err(|error| format!("load {name}: {error}"))?;
        program
            .pin(&program_pin)
            .map_err(|error| format!("pin {name} at {}: {error}", program_pin.display()))?;
        // Cgroup BPF links are independently composable and require link
        // creation flags to be zero. Aya's AllowMultiple mode maps to the
        // legacy BPF_PROG_ATTACH flag, which the BPF_LINK_CREATE ABI rejects.
        let link_id = program
            .attach(cgroup, attach_type, cgroup_link_attach_mode())
            .map_err(|error| format!("attach {name}: {}", program_error_details(&error)))?;
        let link = program
            .take_link(link_id)
            .map_err(|error| format!("take {name} cgroup link: {error}"))?;
        let fd_link = FdLink::try_from(link)
            .map_err(|error| format!("{name} cgroup link is not fd-backed: {error}"))?;
        fd_link
            .pin(&link_pin)
            .map_err(|error| format!("pin {name} link at {}: {error}", link_pin.display()))?;
        Ok(())
    }

    pub(super) const fn cgroup_link_attach_mode() -> CgroupAttachMode {
        CgroupAttachMode::Single
    }

    pub(super) fn program_error_details(error: &aya::programs::ProgramError) -> String {
        if let aya::programs::ProgramError::SyscallError(error) = error {
            format!("{}: {}", error.call, error.io_error)
        } else {
            error.to_string()
        }
    }

    fn validate_bytecode(path: &Path) -> Result<(), String> {
        let bytes = std::fs::read(path)
            .map_err(|error| format!("read eBPF bytecode {}: {error}", path.display()))?;
        ployz_ebpf_common::validate_ployz_isolation_bytecode(bytes.as_slice())
            .map(|_| ())
            .map_err(|error| {
                format!(
                    "validate isolation eBPF bytecode {}: {error:?}",
                    path.display()
                )
            })
    }

    fn validate_fixed_prefix(prefix: ipnet::Ipv4Net) -> Result<(), String> {
        if u32::from(prefix.prefix_len()) != ployz_ebpf_common::ISOLATION_PREFIX_LEN {
            return Err(format!(
                "isolation prefix must be /{}: {prefix}",
                ployz_ebpf_common::ISOLATION_PREFIX_LEN
            ));
        }
        Ok(())
    }

    pub(super) fn validate_cgroup_v2(cgroup_root: &Path) -> Result<(), String> {
        if !cgroup_root.is_absolute() {
            return Err(format!(
                "cgroup v2 root must be absolute: {}",
                cgroup_root.display()
            ));
        }
        if !cgroup_root.is_dir() {
            return Err(format!(
                "cgroup v2 root is not a directory: {}",
                cgroup_root.display()
            ));
        }
        let controllers = cgroup_root.join("cgroup.controllers");
        if !controllers.is_file() {
            return Err(format!(
                "cgroup v2 controllers file is missing: {}",
                controllers.display()
            ));
        }
        Ok(())
    }

    fn read_config(pin_path: &Path) -> Result<Option<ployz_ebpf_common::IsolationConfig>, String> {
        let path = pin_path.join(CONFIG_PIN);
        if !path.exists() {
            return Ok(None);
        }
        let config = open_config_map(pin_path)?;
        config
            .get(&0, 0)
            .map(|config| Some(config.0))
            .map_err(|error| format!("read pinned isolation configuration: {error}"))
    }

    fn open_config_map(
        pin_path: &Path,
    ) -> Result<aya::maps::Array<aya::maps::MapData, PodIsolationConfig>, String> {
        let map_data = aya::maps::MapData::from_pin(pin_path.join(CONFIG_PIN))
            .map_err(|error| format!("open pinned ISOLATION_CONFIG map: {error}"))?;
        aya::maps::Array::try_from(aya::maps::Map::Array(map_data))
            .map_err(|error| format!("open ISOLATION_CONFIG map: {error}"))
    }

    fn open_namespaces_map(
        pin_path: &Path,
    ) -> Result<aya::maps::HashMap<aya::maps::MapData, PodContainerIp, PodNamespaceTag>, String>
    {
        let map_data = aya::maps::MapData::from_pin(pin_path.join(NAMESPACES_PIN))
            .map_err(|error| format!("open pinned ISOLATION_NAMESPACES map: {error}"))?;
        aya::maps::HashMap::try_from(aya::maps::Map::HashMap(map_data))
            .map_err(|error| format!("open ISOLATION_NAMESPACES map: {error}"))
    }

    pub(super) fn remove_owned_pins(pin_path: &Path) -> Result<(), String> {
        for name in OWNED_PINS {
            match std::fs::remove_file(pin_path.join(name)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "remove owned isolation pin {}: {error}",
                        pin_path.join(name).display()
                    ));
                }
            }
        }
        match std::fs::remove_dir(pin_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                let foreign = std::fs::read_dir(pin_path)
                    .map_err(|list_error| {
                        format!(
                            "list foreign entries under isolation pin directory {}: {list_error}",
                            pin_path.display()
                        )
                    })?
                    .map(|entry| {
                        entry
                            .map(|entry| entry.file_name().to_string_lossy().into_owned())
                            .map_err(|entry_error| {
                                format!(
                                    "inspect foreign entry under {}: {entry_error}",
                                    pin_path.display()
                                )
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Err(format!(
                    "isolation repair refused because foreign entries remain under {}: {}",
                    pin_path.display(),
                    foreign.join(", ")
                ))
            }
            Err(error) => Err(format!(
                "remove isolation pin directory {}: {error}",
                pin_path.display()
            )),
        }
    }

    fn detached(pin_path: &Path, reason: &str) -> IsolationStatus {
        IsolationStatus::Detached {
            message: format!(
                "namespace isolation is detached under {}: {reason}",
                pin_path.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    const NS_A: &str = "alpha";
    const NS_B: &str = "beta";

    #[test]
    fn isolation_pin_root_is_a_sibling_of_the_tc_root() {
        assert_eq!(
            sibling_pin_path("/sys/fs/bpf/ployz"),
            Ok(PathBuf::from("/sys/fs/bpf/ployz-isolation"))
        );
        assert_eq!(
            sibling_pin_path("/tmp/custom"),
            Ok(PathBuf::from("/tmp/custom-isolation"))
        );
    }

    #[test]
    fn status_requires_exactly_the_owned_isolation_pin_set() {
        let exact = OWNED_PINS
            .iter()
            .map(std::ffi::OsString::from)
            .collect::<BTreeSet<_>>();
        assert!(has_exact_owned_pin_names(&exact));

        let mut partial = exact.clone();
        partial.remove(std::ffi::OsStr::new(EGRESS_LINK_PIN));
        assert!(!has_exact_owned_pin_names(&partial));

        let mut foreign = exact;
        foreign.insert(std::ffi::OsString::from("foreign"));
        assert!(!has_exact_owned_pin_names(&foreign));
    }

    #[test]
    fn rows_parse_completely_and_coalesce_identical_duplicates() {
        let rows = parse_rows(&format!(
            "10.210.1.2 {NS_A}\n10.210.1.2 {NS_A}\n10.210.2.3 {NS_B}\n"
        ))
        .expect("valid rows");

        assert_eq!(rows.len(), 2);
        let Some(first) = rows.first() else {
            panic!("two parsed rows must have a first row");
        };
        assert_eq!(
            first.address,
            u32::from(Ipv4Addr::new(10, 210, 1, 2)).to_be()
        );
        assert_eq!(
            first.namespace_id,
            encode_namespace_name(NS_A).expect("namespace A")
        );
    }

    #[test]
    fn malformed_or_conflicting_rows_fail_before_a_plan_exists() {
        assert!(parse_rows("not-an-ip alpha\n").is_err());
        assert!(parse_rows("10.210.1.2 Not-Canonical\n").is_err());
        assert!(parse_rows(&format!("10.210.1.2 {NS_A}\n10.210.1.2 {NS_B}\n")).is_err());
    }

    #[test]
    fn desired_rows_are_bounded_before_replacement() {
        let mut rows = String::new();
        for index in 0..=ployz_ebpf_common::MAX_ISOLATION_ENTRIES {
            let second = (index / (254 * 254)) as u8;
            let third = ((index / 254) % 254) as u8;
            let fourth = (index % 254 + 1) as u8;
            rows.push_str(&format!("10.{second}.{third}.{fourth} {NS_A}\n"));
        }

        let error = parse_rows(&rows).expect_err("capacity is validated before map mutation");
        assert!(error.contains("exceeds map capacity"));
    }

    #[test]
    fn desired_addresses_must_be_container_slots_in_the_fixed_prefix() {
        let prefix = "10.210.0.0/16".parse().expect("prefix");
        for address in ["10.210.1.2", "10.210.1.254"] {
            let desired = parse_rows(&format!("{address} {NS_A}\n")).expect("row");
            validate_desired_prefix(&desired, prefix).expect("container address");
        }
        for address in ["10.210.1.0", "10.210.1.1", "10.210.1.255", "10.211.1.2"] {
            let desired = parse_rows(&format!("{address} {NS_A}\n")).expect("row");
            assert!(
                validate_desired_prefix(&desired, prefix).is_err(),
                "{address}"
            );
        }
    }

    #[test]
    fn replacement_removes_stale_and_changed_rows_before_inserting() {
        let a = encode_namespace_name(NS_A).expect("namespace A");
        let b = encode_namespace_name(NS_B).expect("namespace B");
        let retained = u32::from(Ipv4Addr::new(10, 210, 1, 2)).to_be();
        let changed = u32::from(Ipv4Addr::new(10, 210, 1, 3)).to_be();
        let stale = u32::from(Ipv4Addr::new(10, 210, 1, 4)).to_be();
        let added = u32::from(Ipv4Addr::new(10, 210, 1, 5)).to_be();
        let desired = vec![
            DesiredNamespace {
                address: retained,
                namespace_id: a,
            },
            DesiredNamespace {
                address: changed,
                namespace_id: b,
            },
            DesiredNamespace {
                address: added,
                namespace_id: a,
            },
        ];

        let plan = plan_replacement(&[(retained, a), (changed, a), (stale, b)], &desired);

        assert_eq!(plan.remove, [changed, stale]);
        assert_eq!(
            plan.insert
                .iter()
                .map(|row| row.address)
                .collect::<Vec<_>>(),
            [changed, added]
        );
    }

    #[test]
    fn unchanged_rows_require_no_map_mutation() {
        let namespace = encode_namespace_name(NS_A).expect("namespace");
        let address = u32::from(Ipv4Addr::new(10, 210, 1, 2)).to_be();
        let desired = [DesiredNamespace {
            address,
            namespace_id: namespace,
        }];

        let plan = plan_replacement(&[(address, namespace)], &desired);

        assert!(plan.remove.is_empty());
        assert!(plan.insert.is_empty());
    }

    #[test]
    fn configuration_requires_an_armed_fixed_prefix() {
        let prefix = config_prefix(ployz_ebpf_common::IsolationConfig {
            mode: ployz_ebpf_common::ISOLATION_MODE_ARMED,
            network: u32::from(Ipv4Addr::new(10, 210, 0, 0)).to_be(),
            prefix_len: 16,
        })
        .expect("armed /16");
        assert_eq!(prefix, "10.210.0.0/16".parse().expect("expected prefix"));

        assert!(
            config_prefix(ployz_ebpf_common::IsolationConfig {
                mode: ployz_ebpf_common::ISOLATION_MODE_DISABLED,
                network: 0,
                prefix_len: 0,
            })
            .is_err()
        );
    }

    #[test]
    fn safe_pass_all_configurations_are_repairable_partial_state() {
        let requested = "10.210.0.0/16".parse().expect("requested prefix");
        for config in [
            ployz_ebpf_common::IsolationConfig {
                mode: ployz_ebpf_common::ISOLATION_MODE_DISABLED,
                network: 0,
                prefix_len: 0,
            },
            ployz_ebpf_common::IsolationConfig {
                mode: ployz_ebpf_common::ISOLATION_MODE_ARMED,
                network: u32::from(Ipv4Addr::new(10, 210, 0, 0)).to_be(),
                prefix_len: 24,
            },
            ployz_ebpf_common::IsolationConfig {
                mode: ployz_ebpf_common::ISOLATION_MODE_ARMED,
                network: u32::from(Ipv4Addr::new(10, 210, 1, 2)).to_be(),
                prefix_len: 16,
            },
        ] {
            assert_eq!(
                existing_config_disposition(config, requested),
                ExistingConfigDisposition::Repairable
            );
        }
    }

    #[test]
    fn only_a_well_formed_armed_config_locks_the_initialized_prefix() {
        let requested = "10.210.0.0/16".parse().expect("requested prefix");
        let matching = ployz_ebpf_common::IsolationConfig {
            mode: ployz_ebpf_common::ISOLATION_MODE_ARMED,
            network: u32::from(Ipv4Addr::new(10, 210, 0, 0)).to_be(),
            prefix_len: 16,
        };
        assert_eq!(
            existing_config_disposition(matching, requested),
            ExistingConfigDisposition::Matching
        );

        let conflicting = ployz_ebpf_common::IsolationConfig {
            mode: ployz_ebpf_common::ISOLATION_MODE_ARMED,
            network: u32::from(Ipv4Addr::new(10, 211, 0, 0)).to_be(),
            prefix_len: 16,
        };
        assert_eq!(
            existing_config_disposition(conflicting, requested),
            ExistingConfigDisposition::Conflicting {
                pinned: "10.211.0.0/16".parse().expect("pinned prefix")
            }
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn partial_retry_removes_only_owned_isolation_pins() {
        let pin_path = std::env::temp_dir().join(format!(
            "ployz-isolation-cleanup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir(&pin_path).expect("test pin directory");
        for name in OWNED_PINS {
            std::fs::write(pin_path.join(name), b"owned").expect("owned test pin");
        }
        std::fs::write(pin_path.join("foreign"), b"foreign").expect("foreign test pin");

        let error = linux::remove_owned_pins(&pin_path)
            .expect_err("foreign entry must obstruct exact-pin repair");

        assert!(pin_path.join("foreign").is_file());
        assert!(OWNED_PINS.iter().all(|name| !pin_path.join(name).exists()));
        assert!(error.contains("foreign"));
        assert!(error.contains("repair refused"));
        std::fs::remove_file(pin_path.join("foreign")).expect("remove foreign test pin");
        std::fs::remove_dir(pin_path).expect("remove test pin directory");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_v2_root_must_be_absolute_and_expose_controllers() {
        assert!(linux::validate_cgroup_v2(Path::new("relative")).is_err());

        let root = std::env::temp_dir().join(format!(
            "ployz-cgroup-v2-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir(&root).expect("test cgroup root");
        assert!(linux::validate_cgroup_v2(&root).is_err());
        std::fs::write(root.join("cgroup.controllers"), b"cpu io memory")
            .expect("test controllers file");
        linux::validate_cgroup_v2(&root).expect("cgroup v2 root");
        std::fs::remove_file(root.join("cgroup.controllers")).expect("remove controllers file");
        std::fs::remove_dir(root).expect("remove cgroup test root");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_bpf_links_use_zero_link_creation_flags() {
        assert!(matches!(
            linux::cgroup_link_attach_mode(),
            aya::programs::CgroupAttachMode::Single
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn syscall_failures_include_the_kernel_errno() {
        let error = aya::programs::ProgramError::SyscallError(aya::sys::SyscallError {
            call: "bpf_link_create",
            io_error: std::io::Error::from_raw_os_error(libc::EINVAL),
        });

        let detail = linux::program_error_details(&error);

        assert!(detail.contains("bpf_link_create"));
        assert!(detail.contains("Invalid argument"));
        assert!(detail.contains("os error 22"));
    }

    #[test]
    fn non_fixed_isolation_prefix_is_rejected_by_the_cli() {
        let error = crate::EbpfCtlCli::try_parse_from([
            "ployz-ebpf-ctl",
            "isolation",
            "ensure-attached",
            "/usr/lib/ployz/ployz-ebpf-tc",
            "10.210.1.0/24",
            "/sys/fs/cgroup",
        ])
        .expect_err("only the initialization-fixed /16 is accepted")
        .to_string();

        assert!(error.contains("isolation prefix must be /16"));
    }

    #[test]
    fn isolation_cli_shape_is_stable() {
        let cli = crate::EbpfCtlCli::try_parse_from([
            "ployz-ebpf-ctl",
            "isolation",
            "ensure-attached",
            "/usr/lib/ployz/ployz-ebpf-tc",
            "10.210.0.0/16",
            "/sys/fs/cgroup",
        ])
        .expect("ensure-attached CLI");
        assert!(matches!(
            cli.command,
            crate::EbpfCtlCommand::Isolation {
                action: crate::IsolationCommand::EnsureAttached { .. }
            }
        ));

        crate::EbpfCtlCli::try_parse_from([
            "ployz-ebpf-ctl",
            "isolation",
            "replace-all",
            "/run/ployz/isolation-rows",
        ])
        .expect("replace-all CLI");
        crate::EbpfCtlCli::try_parse_from(["ployz-ebpf-ctl", "isolation", "status"])
            .expect("status CLI");

        let cli = crate::EbpfCtlCli::try_parse_from([
            "ployz-ebpf-ctl",
            "--pin-path",
            "/sys/fs/bpf/custom-tc",
            "isolation",
            "status",
        ])
        .expect("custom tc pin root before isolation command");
        assert_eq!(
            sibling_pin_path(cli.pin_path.as_deref().expect("custom pin path")),
            Ok(PathBuf::from("/sys/fs/bpf/custom-tc-isolation"))
        );
    }
}
