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

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: ployz-bpfctl <attach|detach|route> ...");
        return ExitCode::FAILURE;
    }

    let result = match args[1].as_str() {
        "attach" => cmd_attach(&args[2..]),
        "detach" => cmd_detach(&args[2..]),
        "route" => cmd_route(&args[2..]),
        "observe" => cmd_observe(&args[2..]),
        other => {
            eprintln!("unknown command: {other}");
            Err("unknown command".into())
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Ensure bpffs is mounted at /sys/fs/bpf. Idempotent.
fn ensure_bpffs() {
    let target = CString::new("/sys/fs/bpf").unwrap();
    let fstype = CString::new("bpf").unwrap();
    let source = CString::new("bpf").unwrap();
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
    if args.len() < 2 {
        return Err("usage: attach <bridge-ifname> <wg-ifname>".into());
    }
    let bridge = &args[0];
    let wg_ifname = &args[1];
    let wg_ifindex = resolve_ifindex(wg_ifname)?;

    ensure_bpffs();

    let bytecode = include_bytes_aligned!(concat!(env!("OUT_DIR"), "/ployz-ebpf-tc"));
    let mut bpf = Ebpf::load(bytecode)?;

    // Ensure clsact qdisc exists
    let _ = aya::programs::tc::qdisc_add_clsact(bridge);

    let nl_opts = TcAttachOptions::Netlink(NlOptions::default());

    // Attach egress
    let egress: &mut SchedClassifier = bpf
        .program_mut("ployz_egress")
        .ok_or("ployz_egress not found")?
        .try_into()?;
    egress.load()?;
    egress.attach_with_options(bridge, TcAttachType::Egress, nl_opts)?;

    let nl_opts = TcAttachOptions::Netlink(NlOptions::default());

    // Attach ingress
    let ingress: &mut SchedClassifier = bpf
        .program_mut("ployz_ingress")
        .ok_or("ployz_ingress not found")?
        .try_into()?;
    ingress.load()?;
    ingress.attach_with_options(bridge, TcAttachType::Ingress, nl_opts)?;

    // Set the WG interface index for IPv6 ULA redirect
    {
        let wg_map: &mut aya::maps::Map = bpf.map_mut("WG_IFINDEX").ok_or("WG_IFINDEX map not found")?;
        let mut arr = aya::maps::Array::<_, u32>::try_from(wg_map)?;
        arr.set(0, wg_ifindex, 0)?;
    }

    // Pin maps so other invocations can open them
    std::fs::create_dir_all(PIN_PATH)?;
    let map = bpf.map_mut("ROUTES").ok_or("ROUTES map not found")?;
    map.pin(format!("{PIN_PATH}/routes"))?;

    let map = bpf.map_mut("OBSERVE_FLAG").ok_or("OBSERVE_FLAG map not found")?;
    map.pin(format!("{PIN_PATH}/observe_flag"))?;

    // Pin programs to keep them alive after this process exits
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
    if args.is_empty() {
        return Err("usage: detach <bridge-ifname>".into());
    }
    let bridge = &args[0];

    // Removing the qdisc detaches all TC programs on the interface
    let _ = aya::programs::tc::qdisc_detach_program(bridge, TcAttachType::Egress, "ployz_egress");
    let _ = aya::programs::tc::qdisc_detach_program(bridge, TcAttachType::Ingress, "ployz_ingress");

    // Clean up pins
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
    if args.is_empty() {
        return Err("usage: route <add|del> <subnet> [ifindex]".into());
    }

    match args[0].as_str() {
        "add" => {
            if args.len() < 3 {
                return Err("usage: route add <subnet> <ifindex>".into());
            }
            let subnet: ipnet::Ipv4Net = args[1].parse()?;
            let ifindex: u32 = args[2].parse()?;

            let key = PodRouteKey(subnet_to_key(subnet));
            let entry = PodRouteEntry(RouteEntry { ifindex });

            let mut map = open_routes_map()?;
            map.insert(key, entry, 0)?;

            eprintln!("added route {subnet} -> ifindex {ifindex}");
            Ok(())
        }
        "del" => {
            if args.len() < 2 {
                return Err("usage: route del <subnet>".into());
            }
            let subnet: ipnet::Ipv4Net = args[1].parse()?;
            let key = PodRouteKey(subnet_to_key(subnet));

            let mut map = open_routes_map()?;
            let _ = map.remove(&key);

            eprintln!("removed route {subnet}");
            Ok(())
        }
        other => Err(format!("unknown route subcommand: {other}").into()),
    }
}

fn open_routes_map() -> Result<aya::maps::HashMap<aya::maps::MapData, PodRouteKey, PodRouteEntry>, Box<dyn std::error::Error>> {
    let map_data = aya::maps::MapData::from_pin(format!("{PIN_PATH}/routes"))?;
    Ok(aya::maps::HashMap::try_from(aya::maps::Map::HashMap(map_data))?)
}

fn resolve_ifindex(ifname: &str) -> Result<u32, Box<dyn std::error::Error>> {
    let c_name = std::ffi::CString::new(ifname)?;
    let idx = unsafe { libc::if_nametoindex(c_name.as_ptr()) };
    if idx == 0 {
        return Err(format!("interface {ifname} not found").into());
    }
    Ok(idx)
}

/// observe on|off
/// Toggles the OBSERVE_FLAG in the pinned BPF map.
fn cmd_observe(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("usage: observe <on|off>".into());
    }

    let value: u32 = match args[0].as_str() {
        "on" => 1,
        "off" => 0,
        other => return Err(format!("unknown observe state: {other} (expected on|off)").into()),
    };

    let map_data = aya::maps::MapData::from_pin(format!("{PIN_PATH}/observe_flag"))?;
    let mut arr = aya::maps::Array::<_, u32>::try_from(aya::maps::Map::Array(map_data))?;
    arr.set(0, value, 0)?;

    eprintln!("observation {}", if value == 1 { "enabled" } else { "disabled" });
    Ok(())
}

fn subnet_to_key(subnet: ipnet::Ipv4Net) -> RouteKey {
    let network_addr: Ipv4Addr = subnet.network();
    RouteKey {
        network: u32::from(network_addr).to_be(),
        prefix_len: subnet.prefix_len() as u32,
    }
}
