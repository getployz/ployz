use aya::Ebpf;
use aya::programs::tc::{NlOptions, TcAttachOptions};
use aya::programs::{SchedClassifier, TcAttachType};
use ployz_ebpf_common::{RouteEntry, RouteKey};
use std::ffi::CString;
use std::net::Ipv4Addr;
use std::process::ExitCode;

// Wrapper types to satisfy orphan rules for Pod impl
#[repr(C)]
#[derive(Clone, Copy)]
struct PodRouteKey(RouteKey);
#[repr(C)]
#[derive(Clone, Copy)]
struct PodRouteEntry(RouteEntry);

unsafe impl aya::Pod for PodRouteKey {}
unsafe impl aya::Pod for PodRouteEntry {}

const PIN_PATH: &str = "/sys/fs/bpf/ployz";

macro_rules! include_bytes_aligned {
    ($path:expr) => {{
        #[repr(C, align(8))]
        struct Aligned<Bytes: ?Sized>(Bytes);
        static ALIGNED: &Aligned<[u8]> = &Aligned(*include_bytes!($path));
        &ALIGNED.0
    }};
}

pub(crate) fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let [_, cmd, rest @ ..] = args.as_slice() else {
        eprintln!("usage: ployz-bpfctl <attach|detach|route|observe> ...");
        return ExitCode::FAILURE;
    };

    let result = match cmd.as_str() {
        "attach" => cmd_attach(rest),
        "detach" => cmd_detach(rest),
        "route" => cmd_route(rest),
        "observe" => cmd_observe(rest),
        other => {
            eprintln!("unknown command: {other}");
            Err("unknown command".into())
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Ensure bpffs is mounted at /sys/fs/bpf. Idempotent.
fn ensure_bpffs() {
    let target = CString::new("/sys/fs/bpf").expect("static mount path is valid");
    let fstype = CString::new("bpf").expect("static filesystem type is valid");
    let source = CString::new("bpf").expect("static source is valid");
    let _ = std::fs::create_dir_all("/sys/fs/bpf");
    unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            0,
            std::ptr::null(),
        );
    }
}

/// attach <bridge-ifname> <wg-ifname>
/// Loads BPF bytecode, attaches TC classifiers, sets WG ifindex, pins maps.
fn cmd_attach(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let [bridge, wg_ifname, ..] = args else {
        return Err("usage: attach <bridge-ifname> <wg-ifname>".into());
    };
    let wg_ifindex = resolve_ifindex(wg_ifname)?;

    ensure_bpffs();

    let bytecode = include_bytes_aligned!(concat!(env!("OUT_DIR"), "/ployz-ebpf-tc"));
    let mut bpf = Ebpf::load(bytecode)?;

    let _ = aya::programs::tc::qdisc_add_clsact(bridge);
    attach_tc_classifier(&mut bpf, "ployz_egress", bridge, TcAttachType::Egress)?;
    attach_tc_classifier(&mut bpf, "ployz_ingress", bridge, TcAttachType::Ingress)?;

    {
        let wg_map: &mut aya::maps::Map = bpf
            .map_mut("WG_IFINDEX")
            .ok_or("WG_IFINDEX map not found")?;
        let mut arr = aya::maps::Array::<_, u32>::try_from(wg_map)?;
        arr.set(0, wg_ifindex, 0)?;
    }

    std::fs::create_dir_all(PIN_PATH)?;
    let map = bpf.map_mut("ROUTES").ok_or("ROUTES map not found")?;
    map.pin(format!("{PIN_PATH}/routes"))?;

    let map = bpf
        .map_mut("OBSERVE_FLAG")
        .ok_or("OBSERVE_FLAG map not found")?;
    map.pin(format!("{PIN_PATH}/observe_flag"))?;

    if let Some(prog) = bpf.program_mut("ployz_egress") {
        let _ = prog.pin(format!("{PIN_PATH}/egress"));
    }
    if let Some(prog) = bpf.program_mut("ployz_ingress") {
        let _ = prog.pin(format!("{PIN_PATH}/ingress"));
    }

    eprintln!("attached TC classifiers to {bridge}");
    Ok(())
}

/// detach <bridge-ifname>
/// Removes pinned BPF objects. Removing the clsact qdisc detaches all TC programs.
fn cmd_detach(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let [bridge, ..] = args else {
        return Err("usage: detach <bridge-ifname>".into());
    };

    let _ = aya::programs::tc::qdisc_detach_program(bridge, TcAttachType::Egress, "ployz_egress");
    let _ = aya::programs::tc::qdisc_detach_program(bridge, TcAttachType::Ingress, "ployz_ingress");

    let _ = std::fs::remove_file(format!("{PIN_PATH}/routes"));
    let _ = std::fs::remove_file(format!("{PIN_PATH}/observe_flag"));
    let _ = std::fs::remove_file(format!("{PIN_PATH}/egress"));
    let _ = std::fs::remove_file(format!("{PIN_PATH}/ingress"));
    let _ = std::fs::remove_dir(PIN_PATH);

    eprintln!("detached TC classifiers from {bridge}");
    Ok(())
}

/// route add <subnet> <ifindex>
/// route del <subnet>
/// Opens the pinned ROUTES map and inserts/removes entries.
fn cmd_route(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let [subcmd, rest @ ..] = args else {
        return Err("usage: route <add|del> <subnet> [ifindex]".into());
    };

    match subcmd.as_str() {
        "add" => {
            let [subnet_str, ifindex_str, ..] = rest else {
                return Err("usage: route add <subnet> <ifindex>".into());
            };
            let subnet: ipnet::Ipv4Net = subnet_str.parse()?;
            let ifindex: u32 = ifindex_str.parse()?;

            let key = PodRouteKey(subnet_to_key(subnet));
            let entry = PodRouteEntry(RouteEntry { ifindex });

            let mut map = open_routes_map()?;
            map.insert(key, entry, 0)?;

            eprintln!("added route {subnet} -> ifindex {ifindex}");
            Ok(())
        }
        "del" => {
            let [subnet_str, ..] = rest else {
                return Err("usage: route del <subnet>".into());
            };
            let subnet: ipnet::Ipv4Net = subnet_str.parse()?;
            let key = PodRouteKey(subnet_to_key(subnet));

            let mut map = open_routes_map()?;
            let _ = map.remove(&key);

            eprintln!("removed route {subnet}");
            Ok(())
        }
        other => Err(format!("unknown route subcommand: {other}").into()),
    }
}

fn open_routes_map() -> Result<
    aya::maps::HashMap<aya::maps::MapData, PodRouteKey, PodRouteEntry>,
    Box<dyn std::error::Error>,
> {
    let map_data = aya::maps::MapData::from_pin(format!("{PIN_PATH}/routes"))?;
    Ok(aya::maps::HashMap::try_from(aya::maps::Map::HashMap(
        map_data,
    ))?)
}

fn resolve_ifindex(ifname: &str) -> Result<u32, Box<dyn std::error::Error>> {
    let c_name = CString::new(ifname)?;
    let idx = unsafe { libc::if_nametoindex(c_name.as_ptr()) };
    if idx == 0 {
        return Err(format!("interface {ifname} not found").into());
    }
    Ok(idx)
}

/// observe on|off
/// Toggles the OBSERVE_FLAG in the pinned BPF map.
fn cmd_observe(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let [state, ..] = args else {
        return Err("usage: observe <on|off>".into());
    };

    let value: u32 = match state.as_str() {
        "on" => 1,
        "off" => 0,
        other => return Err(format!("unknown observe state: {other} (expected on|off)").into()),
    };

    let map_data = aya::maps::MapData::from_pin(format!("{PIN_PATH}/observe_flag"))?;
    let mut arr = aya::maps::Array::<_, u32>::try_from(aya::maps::Map::Array(map_data))?;
    arr.set(0, value, 0)?;

    eprintln!(
        "observation {}",
        if value == 1 { "enabled" } else { "disabled" }
    );
    Ok(())
}

fn attach_tc_classifier(
    bpf: &mut Ebpf,
    program_name: &str,
    ifname: &str,
    attach_type: TcAttachType,
) -> Result<(), Box<dyn std::error::Error>> {
    let classifier: &mut SchedClassifier = bpf
        .program_mut(program_name)
        .ok_or(format!("{program_name} not found"))?
        .try_into()?;
    classifier.load()?;
    classifier.attach_with_options(
        ifname,
        attach_type,
        TcAttachOptions::Netlink(NlOptions::default()),
    )?;
    Ok(())
}

fn subnet_to_key(subnet: ipnet::Ipv4Net) -> RouteKey {
    let network_addr: Ipv4Addr = subnet.network();
    RouteKey {
        network: u32::from(network_addr).to_be(),
        prefix_len: subnet.prefix_len() as u32,
    }
}
