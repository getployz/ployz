#[cfg(any(target_os = "linux", test))]
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[cfg(target_os = "linux")]
const DEFAULT_PIN_PATH: &str = ployz_ebpf_common::DEFAULT_PIN_PATH;
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
    let args = EbpfCtlCli::try_parse_from(std::iter::once("ployz-ebpf-ctl".to_owned()).chain(args))
        .map_err(|error| error.to_string())?;
    let pin_path = args.pin_path.as_deref();
    match args.command {
        EbpfCtlCommand::Validate { bytecode } => validate_bytecode(&bytecode),
        EbpfCtlCommand::Attach {
            bytecode,
            bridge_ifname,
            wg_ifname,
        } => attach(&bytecode, &bridge_ifname, &wg_ifname, pin_path),
        EbpfCtlCommand::EnsureAttached {
            bytecode,
            bridge_ifname,
            wg_ifname,
        } => ensure_attached(&bytecode, &bridge_ifname, &wg_ifname, pin_path),
        EbpfCtlCommand::Detach { bridge_ifname } => detach(&bridge_ifname, pin_path),
        EbpfCtlCommand::Route { action } => match action {
            RouteCommand::Add { subnet, ifindex } => route_add(subnet, ifindex, pin_path),
            RouteCommand::AddIfname { subnet, ifname } => {
                route_add_ifname(subnet, &ifname, pin_path)
            }
            RouteCommand::Del { subnet } => route_del(subnet, pin_path),
        },
    }
}

#[derive(Debug, Parser)]
#[command(name = "ployz-ebpf-ctl", disable_help_subcommand = true)]
struct EbpfCtlCli {
    #[arg(long)]
    pin_path: Option<String>,
    #[command(subcommand)]
    command: EbpfCtlCommand,
}

#[derive(Debug, Subcommand)]
enum EbpfCtlCommand {
    Validate {
        bytecode: PathBuf,
    },
    Attach {
        bytecode: PathBuf,
        bridge_ifname: String,
        wg_ifname: String,
    },
    EnsureAttached {
        bytecode: PathBuf,
        bridge_ifname: String,
        wg_ifname: String,
    },
    Detach {
        bridge_ifname: String,
    },
    Route {
        #[command(subcommand)]
        action: RouteCommand,
    },
}

#[derive(Debug, Subcommand)]
enum RouteCommand {
    Add {
        #[arg(value_parser = parse_ipv4_subnet)]
        subnet: ipnet::Ipv4Net,
        ifindex: u32,
    },
    AddIfname {
        #[arg(value_parser = parse_ipv4_subnet)]
        subnet: ipnet::Ipv4Net,
        ifname: String,
    },
    Del {
        #[arg(value_parser = parse_ipv4_subnet)]
        subnet: ipnet::Ipv4Net,
    },
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
fn attach(
    bytecode: &Path,
    bridge: &str,
    wg_ifname: &str,
    pin_path: Option<&str>,
) -> Result<(), String> {
    linux::attach(
        bytecode,
        bridge,
        wg_ifname,
        pin_path.unwrap_or(DEFAULT_PIN_PATH),
    )
}

#[cfg(not(target_os = "linux"))]
fn attach(
    _bytecode: &Path,
    _bridge: &str,
    _wg_ifname: &str,
    _pin_path: Option<&str>,
) -> Result<(), String> {
    Err("attach requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn ensure_attached(
    bytecode: &Path,
    bridge: &str,
    wg_ifname: &str,
    pin_path: Option<&str>,
) -> Result<(), String> {
    linux::ensure_attached(
        bytecode,
        bridge,
        wg_ifname,
        pin_path.unwrap_or(DEFAULT_PIN_PATH),
    )
}

#[cfg(not(target_os = "linux"))]
fn ensure_attached(
    _bytecode: &Path,
    _bridge: &str,
    _wg_ifname: &str,
    _pin_path: Option<&str>,
) -> Result<(), String> {
    Err("ensure-attached requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn detach(bridge: &str, pin_path: Option<&str>) -> Result<(), String> {
    linux::detach(bridge, pin_path.unwrap_or(DEFAULT_PIN_PATH))
}

#[cfg(not(target_os = "linux"))]
fn detach(_bridge: &str, _pin_path: Option<&str>) -> Result<(), String> {
    Err("detach requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn route_add(subnet: ipnet::Ipv4Net, ifindex: u32, pin_path: Option<&str>) -> Result<(), String> {
    linux::route_add(subnet, ifindex, pin_path.unwrap_or(DEFAULT_PIN_PATH))
}

#[cfg(not(target_os = "linux"))]
fn route_add(
    _subnet: ipnet::Ipv4Net,
    _ifindex: u32,
    _pin_path: Option<&str>,
) -> Result<(), String> {
    Err("route add requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn route_add_ifname(
    subnet: ipnet::Ipv4Net,
    ifname: &str,
    pin_path: Option<&str>,
) -> Result<(), String> {
    linux::route_add_ifname(subnet, ifname, pin_path.unwrap_or(DEFAULT_PIN_PATH))
}

#[cfg(not(target_os = "linux"))]
fn route_add_ifname(
    _subnet: ipnet::Ipv4Net,
    _ifname: &str,
    _pin_path: Option<&str>,
) -> Result<(), String> {
    Err("route add-ifname requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn route_del(subnet: ipnet::Ipv4Net, pin_path: Option<&str>) -> Result<(), String> {
    linux::route_del(subnet, pin_path.unwrap_or(DEFAULT_PIN_PATH))
}

#[cfg(not(target_os = "linux"))]
fn route_del(_subnet: ipnet::Ipv4Net, _pin_path: Option<&str>) -> Result<(), String> {
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
    use super::{subnet_to_key, validate_bytecode};
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

    pub fn attach(
        bytecode: &Path,
        bridge: &str,
        wg_ifname: &str,
        pin_path: &str,
    ) -> Result<(), String> {
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

        std::fs::create_dir_all(pin_path).map_err(|error| format!("create {pin_path}: {error}"))?;
        bpf.map_mut("ROUTES")
            .ok_or("ROUTES map not found")?
            .pin(format!("{pin_path}/routes"))
            .map_err(|error| format!("pin ROUTES map: {error}"))?;
        bpf.program_mut("ployz_egress")
            .ok_or("ployz_egress program not found after attach")?
            .pin(format!("{pin_path}/egress"))
            .map_err(|error| format!("pin ployz_egress: {error}"))?;
        bpf.program_mut("ployz_ingress")
            .ok_or("ployz_ingress program not found after attach")?
            .pin(format!("{pin_path}/ingress"))
            .map_err(|error| format!("pin ployz_ingress: {error}"))?;

        Ok(())
    }

    pub fn ensure_attached(
        bytecode: &Path,
        bridge: &str,
        wg_ifname: &str,
        pin_path: &str,
    ) -> Result<(), String> {
        validate_bytecode(bytecode)?;
        if ployz_tc_pins_exist(pin_path) {
            return Ok(());
        }
        remove_ployz_tc_pins(pin_path)?;
        attach(bytecode, bridge, wg_ifname, pin_path)
    }

    pub fn detach(bridge: &str, pin_path: &str) -> Result<(), String> {
        aya::programs::tc::qdisc_detach_program(bridge, TcAttachType::Egress, "ployz_egress")
            .map_err(|error| format!("detach ployz_egress from {bridge}: {error}"))?;
        aya::programs::tc::qdisc_detach_program(bridge, TcAttachType::Ingress, "ployz_ingress")
            .map_err(|error| format!("detach ployz_ingress from {bridge}: {error}"))?;
        remove_pin_file(format!("{pin_path}/routes"))?;
        remove_pin_file(format!("{pin_path}/egress"))?;
        remove_pin_file(format!("{pin_path}/ingress"))?;
        remove_pin_dir(pin_path)?;
        Ok(())
    }

    fn ployz_tc_pins_exist(pin_path: &str) -> bool {
        [
            format!("{pin_path}/routes"),
            format!("{pin_path}/egress"),
            format!("{pin_path}/ingress"),
        ]
        .iter()
        .all(|path| Path::new(path).exists())
    }

    fn remove_ployz_tc_pins(pin_path: &str) -> Result<(), String> {
        remove_pin_file(format!("{pin_path}/routes"))?;
        remove_pin_file(format!("{pin_path}/egress"))?;
        remove_pin_file(format!("{pin_path}/ingress"))?;
        remove_pin_dir(pin_path)
    }

    pub fn route_add(subnet: ipnet::Ipv4Net, ifindex: u32, pin_path: &str) -> Result<(), String> {
        let mut map = open_routes_map(pin_path)?;
        map.insert(
            PodRouteKey(subnet_to_key(subnet)),
            PodRouteEntry(RouteEntry { ifindex }),
            0,
        )
        .map_err(|error| format!("insert route {subnet}: {error}"))
    }

    pub fn route_add_ifname(
        subnet: ipnet::Ipv4Net,
        ifname: &str,
        pin_path: &str,
    ) -> Result<(), String> {
        route_add(subnet, resolve_ifindex(ifname)?, pin_path)
    }

    pub fn route_del(subnet: ipnet::Ipv4Net, pin_path: &str) -> Result<(), String> {
        let mut map = open_routes_map(pin_path)?;
        let _ = map.remove(&PodRouteKey(subnet_to_key(subnet)));
        Ok(())
    }

    fn open_routes_map(
        pin_path: &str,
    ) -> Result<aya::maps::HashMap<aya::maps::MapData, PodRouteKey, PodRouteEntry>, String> {
        let map_data = aya::maps::MapData::from_pin(format!("{pin_path}/routes"))
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

        assert!(error.contains("Usage: ployz-ebpf-ctl"));
        assert!(error.contains("<COMMAND>"));
    }

    #[test]
    fn global_pin_path_is_accepted_before_command() {
        let error = run(vec![
            "--pin-path".to_owned(),
            "/sys/fs/bpf/ployz-core".to_owned(),
            "route".to_owned(),
            "add".to_owned(),
            "10.42.2.0/24".to_owned(),
            "7".to_owned(),
        ])
        .expect_err("route add requires Linux on this test platform");

        assert!(
            error.contains("route add requires Linux")
                || error.contains("No such file")
                || error.contains("open pinned ROUTES map")
        );
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
