#[cfg(any(target_os = "linux", test))]
use std::net::Ipv4Addr;
use std::path::Path;
use std::process::ExitCode;

#[cfg(target_os = "linux")]
const PIN_PATH: &str = "/sys/fs/bpf/ployz";
fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    match args.as_slice() {
        [command, bytecode] if command == "validate" => validate_bytecode(Path::new(bytecode)),
        [command, bytecode, bridge, wg_ifname] if command == "attach" => {
            attach(Path::new(bytecode), bridge, wg_ifname)
        }
        [command, bytecode, bridge, wg_ifname] if command == "ensure-attached" => {
            ensure_attached(Path::new(bytecode), bridge, wg_ifname)
        }
        [command, bridge] if command == "detach" => detach(bridge),
        [command, action, subnet, ifindex] if command == "route" && action == "add" => {
            let subnet = parse_ipv4_subnet(subnet)?;
            let ifindex = ifindex
                .parse::<u32>()
                .map_err(|error| format!("invalid ifindex {ifindex:?}: {error}"))?;
            route_add(subnet, ifindex)
        }
        [command, action, subnet, ifname] if command == "route" && action == "add-ifname" => {
            route_add_ifname(parse_ipv4_subnet(subnet)?, ifname)
        }
        [command, action, subnet] if command == "route" && action == "del" => {
            route_del(parse_ipv4_subnet(subnet)?)
        }
        [] => Err(usage()),
        [unknown, ..] => Err(format!(
            "unknown or invalid command: {unknown}\n{}",
            usage()
        )),
    }
}

fn usage() -> String {
    "usage: ployz-ebpf-ctl validate <bytecode>\n       ployz-ebpf-ctl attach <bytecode> <bridge-ifname> <wg-ifname>\n       ployz-ebpf-ctl ensure-attached <bytecode> <bridge-ifname> <wg-ifname>\n       ployz-ebpf-ctl detach <bridge-ifname>\n       ployz-ebpf-ctl route add <ipv4-subnet> <ifindex>\n       ployz-ebpf-ctl route add-ifname <ipv4-subnet> <ifname>\n       ployz-ebpf-ctl route del <ipv4-subnet>".to_owned()
}

fn validate_bytecode(path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("read eBPF bytecode {}: {error}", path.display()))?;
    ployz_ebpf_common::validate_ployz_tc_bytecode(bytes.as_slice())
        .map(|_| ())
        .map_err(|error| format!("validate eBPF bytecode {}: {error:?}", path.display()))
}

fn parse_ipv4_subnet(value: &str) -> Result<ipnet::Ipv4Net, String> {
    value
        .parse()
        .map_err(|error| format!("invalid IPv4 subnet {value:?}: {error}"))
}

#[cfg(target_os = "linux")]
fn attach(bytecode: &Path, bridge: &str, wg_ifname: &str) -> Result<(), String> {
    linux::attach(bytecode, bridge, wg_ifname)
}

#[cfg(not(target_os = "linux"))]
fn attach(_bytecode: &Path, _bridge: &str, _wg_ifname: &str) -> Result<(), String> {
    Err("attach requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn ensure_attached(bytecode: &Path, bridge: &str, wg_ifname: &str) -> Result<(), String> {
    linux::ensure_attached(bytecode, bridge, wg_ifname)
}

#[cfg(not(target_os = "linux"))]
fn ensure_attached(_bytecode: &Path, _bridge: &str, _wg_ifname: &str) -> Result<(), String> {
    Err("ensure-attached requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn detach(bridge: &str) -> Result<(), String> {
    linux::detach(bridge)
}

#[cfg(not(target_os = "linux"))]
fn detach(_bridge: &str) -> Result<(), String> {
    Err("detach requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn route_add(subnet: ipnet::Ipv4Net, ifindex: u32) -> Result<(), String> {
    linux::route_add(subnet, ifindex)
}

#[cfg(not(target_os = "linux"))]
fn route_add(_subnet: ipnet::Ipv4Net, _ifindex: u32) -> Result<(), String> {
    Err("route add requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn route_add_ifname(subnet: ipnet::Ipv4Net, ifname: &str) -> Result<(), String> {
    linux::route_add_ifname(subnet, ifname)
}

#[cfg(not(target_os = "linux"))]
fn route_add_ifname(_subnet: ipnet::Ipv4Net, _ifname: &str) -> Result<(), String> {
    Err("route add-ifname requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn route_del(subnet: ipnet::Ipv4Net) -> Result<(), String> {
    linux::route_del(subnet)
}

#[cfg(not(target_os = "linux"))]
fn route_del(_subnet: ipnet::Ipv4Net) -> Result<(), String> {
    Err("route del requires Linux".to_owned())
}

#[cfg(any(target_os = "linux", test))]
fn subnet_to_key(subnet: ipnet::Ipv4Net) -> ployz_ebpf_common::RouteKey {
    let network_addr: Ipv4Addr = subnet.network();
    ployz_ebpf_common::RouteKey {
        network: u32::from(network_addr).to_be(),
        prefix_len: u32::from(subnet.prefix_len()),
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{PIN_PATH, subnet_to_key, validate_bytecode};
    use aya::Ebpf;
    use aya::programs::tc::{NlOptions, TcAttachOptions};
    use aya::programs::{SchedClassifier, TcAttachType};
    use ployz_ebpf_common::{RouteEntry, RouteKey};
    use std::path::Path;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct PodRouteKey(RouteKey);

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct PodRouteEntry(RouteEntry);

    unsafe impl aya::Pod for PodRouteKey {}
    unsafe impl aya::Pod for PodRouteEntry {}

    pub fn attach(bytecode: &Path, bridge: &str, wg_ifname: &str) -> Result<(), String> {
        validate_bytecode(bytecode)?;
        let wg_ifindex = resolve_ifindex(wg_ifname)?;
        let bytes = std::fs::read(bytecode)
            .map_err(|error| format!("read eBPF bytecode {}: {error}", bytecode.display()))?;
        let mut bpf = Ebpf::load(&bytes).map_err(|error| format!("load eBPF bytecode: {error}"))?;

        let _ = aya::programs::tc::qdisc_add_clsact(bridge);

        let egress: &mut SchedClassifier = bpf
            .program_mut("ployz_egress")
            .ok_or("ployz_egress program not found")?
            .try_into()
            .map_err(|error| format!("open ployz_egress: {error}"))?;
        egress
            .load()
            .map_err(|error| format!("load ployz_egress: {error}"))?;
        egress
            .attach_with_options(
                bridge,
                TcAttachType::Egress,
                TcAttachOptions::Netlink(NlOptions::default()),
            )
            .map_err(|error| format!("attach ployz_egress to {bridge}: {error}"))?;

        let ingress: &mut SchedClassifier = bpf
            .program_mut("ployz_ingress")
            .ok_or("ployz_ingress program not found")?
            .try_into()
            .map_err(|error| format!("open ployz_ingress: {error}"))?;
        ingress
            .load()
            .map_err(|error| format!("load ployz_ingress: {error}"))?;
        ingress
            .attach_with_options(
                bridge,
                TcAttachType::Ingress,
                TcAttachOptions::Netlink(NlOptions::default()),
            )
            .map_err(|error| format!("attach ployz_ingress to {bridge}: {error}"))?;

        let wg_map = bpf
            .map_mut("WG_IFINDEX")
            .ok_or("WG_IFINDEX map not found")?;
        let mut wg_ifindex_map = aya::maps::Array::<_, u32>::try_from(wg_map)
            .map_err(|error| format!("open WG_IFINDEX map: {error}"))?;
        wg_ifindex_map
            .set(0, wg_ifindex, 0)
            .map_err(|error| format!("set WG_IFINDEX: {error}"))?;

        std::fs::create_dir_all(PIN_PATH).map_err(|error| format!("create {PIN_PATH}: {error}"))?;
        bpf.map_mut("ROUTES")
            .ok_or("ROUTES map not found")?
            .pin(format!("{PIN_PATH}/routes"))
            .map_err(|error| format!("pin ROUTES map: {error}"))?;
        bpf.program_mut("ployz_egress")
            .ok_or("ployz_egress program not found after attach")?
            .pin(format!("{PIN_PATH}/egress"))
            .map_err(|error| format!("pin ployz_egress: {error}"))?;
        bpf.program_mut("ployz_ingress")
            .ok_or("ployz_ingress program not found after attach")?
            .pin(format!("{PIN_PATH}/ingress"))
            .map_err(|error| format!("pin ployz_ingress: {error}"))?;

        Ok(())
    }

    pub fn ensure_attached(bytecode: &Path, bridge: &str, wg_ifname: &str) -> Result<(), String> {
        validate_bytecode(bytecode)?;
        if ployz_tc_pins_exist() {
            return Ok(());
        }
        remove_ployz_tc_pins()?;
        attach(bytecode, bridge, wg_ifname)
    }

    pub fn detach(bridge: &str) -> Result<(), String> {
        aya::programs::tc::qdisc_detach_program(bridge, TcAttachType::Egress, "ployz_egress")
            .map_err(|error| format!("detach ployz_egress from {bridge}: {error}"))?;
        aya::programs::tc::qdisc_detach_program(bridge, TcAttachType::Ingress, "ployz_ingress")
            .map_err(|error| format!("detach ployz_ingress from {bridge}: {error}"))?;
        remove_pin_file(format!("{PIN_PATH}/routes"))?;
        remove_pin_file(format!("{PIN_PATH}/egress"))?;
        remove_pin_file(format!("{PIN_PATH}/ingress"))?;
        remove_pin_dir(PIN_PATH)?;
        Ok(())
    }

    fn ployz_tc_pins_exist() -> bool {
        [
            format!("{PIN_PATH}/routes"),
            format!("{PIN_PATH}/egress"),
            format!("{PIN_PATH}/ingress"),
        ]
        .iter()
        .all(|path| Path::new(path).exists())
    }

    fn remove_ployz_tc_pins() -> Result<(), String> {
        remove_pin_file(format!("{PIN_PATH}/routes"))?;
        remove_pin_file(format!("{PIN_PATH}/egress"))?;
        remove_pin_file(format!("{PIN_PATH}/ingress"))?;
        remove_pin_dir(PIN_PATH)
    }

    pub fn route_add(subnet: ipnet::Ipv4Net, ifindex: u32) -> Result<(), String> {
        let mut map = open_routes_map()?;
        map.insert(
            PodRouteKey(subnet_to_key(subnet)),
            PodRouteEntry(RouteEntry { ifindex }),
            0,
        )
        .map_err(|error| format!("insert route {subnet}: {error}"))
    }

    pub fn route_add_ifname(subnet: ipnet::Ipv4Net, ifname: &str) -> Result<(), String> {
        route_add(subnet, resolve_ifindex(ifname)?)
    }

    pub fn route_del(subnet: ipnet::Ipv4Net) -> Result<(), String> {
        let mut map = open_routes_map()?;
        let _ = map.remove(&PodRouteKey(subnet_to_key(subnet)));
        Ok(())
    }

    fn open_routes_map()
    -> Result<aya::maps::HashMap<aya::maps::MapData, PodRouteKey, PodRouteEntry>, String> {
        let map_data = aya::maps::MapData::from_pin(format!("{PIN_PATH}/routes"))
            .map_err(|error| format!("open pinned ROUTES map: {error}"))?;
        aya::maps::HashMap::try_from(aya::maps::Map::HashMap(map_data))
            .map_err(|error| format!("open ROUTES map: {error}"))
    }

    fn resolve_ifindex(ifname: &str) -> Result<u32, String> {
        let name =
            std::ffi::CString::new(ifname).map_err(|error| format!("invalid ifname: {error}"))?;
        let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
        if index == 0 {
            return Err(format!("interface {ifname} not found"));
        }
        Ok(index)
    }

    fn remove_pin_file(path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("remove {}: {error}", path.display())),
        }
    }

    fn remove_pin_dir(path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        match std::fs::remove_dir(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("remove {}: {error}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_subnet_keys_match_ebpf_prefix_format() {
        let key = subnet_to_key("10.42.0.0/16".parse().expect("valid subnet"));

        assert_eq!(key.network, u32::from(Ipv4Addr::new(10, 42, 0, 0)).to_be());
        assert_eq!(key.prefix_len, 16);
    }

    #[test]
    fn invalid_commands_report_usage() {
        let error = run(Vec::new()).expect_err("empty args fail");

        assert!(error.contains("ployz-ebpf-ctl validate <bytecode>"));
        assert!(error.contains("ployz-ebpf-ctl ensure-attached <bytecode>"));
        assert!(error.contains("ployz-ebpf-ctl route add-ifname <ipv4-subnet> <ifname>"));
    }

    #[test]
    fn invalid_subnet_is_rejected_before_route_execution() {
        let error = run(vec![
            "route".to_owned(),
            "add".to_owned(),
            "not-a-subnet".to_owned(),
            "1".to_owned(),
        ])
        .expect_err("invalid subnet fails");

        assert!(error.contains("invalid IPv4 subnet"));
    }
}
