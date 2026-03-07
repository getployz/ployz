use aya::Ebpf;
use aya::programs::tc::{NlOptions, TcAttachOptions};
use aya::programs::{SchedClassifier, TcAttachType};
use ployz_ebpf_common::{RouteEntry, RouteKey};
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
        eprintln!("usage: ployz-ebpf-ctl <attach|detach|route> ...");
        return ExitCode::FAILURE;
    }

    let result = match args[1].as_str() {
        "attach" => cmd_attach(&args[2..]),
        "detach" => cmd_detach(&args[2..]),
        "route" => cmd_route(&args[2..]),
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

/// attach <bridge-ifname>
/// Loads BPF bytecode, attaches TC classifiers, pins the ROUTES map.
fn cmd_attach(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("usage: attach <bridge-ifname>".into());
    }
    let bridge = &args[0];

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

    // Pin the ROUTES map so other invocations can open it
    std::fs::create_dir_all(PIN_PATH)?;
    let map = bpf.map_mut("ROUTES").ok_or("ROUTES map not found")?;
    map.pin(format!("{PIN_PATH}/routes"))?;

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

            let map_data = aya::maps::MapData::from_pin(format!("{PIN_PATH}/routes"))?;
            let mut map = aya::maps::HashMap::<_, PodRouteKey, PodRouteEntry>::try_from(
                aya::maps::Map::HashMap(map_data),
            )?;
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

            let map_data = aya::maps::MapData::from_pin(format!("{PIN_PATH}/routes"))?;
            let mut map = aya::maps::HashMap::<_, PodRouteKey, PodRouteEntry>::try_from(
                aya::maps::Map::HashMap(map_data),
            )?;
            let _ = map.remove(&key);

            eprintln!("removed route {subnet}");
            Ok(())
        }
        other => Err(format!("unknown route subcommand: {other}").into()),
    }
}

fn subnet_to_key(subnet: ipnet::Ipv4Net) -> RouteKey {
    let network_addr: Ipv4Addr = subnet.network();
    RouteKey {
        network: u32::from(network_addr).to_be(),
        prefix_len: subnet.prefix_len() as u32,
    }
}
