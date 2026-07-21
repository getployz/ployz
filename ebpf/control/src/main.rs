#[cfg(any(target_os = "linux", test))]
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[cfg(target_os = "linux")]
const DEFAULT_PIN_PATH: &str = ployz_ebpf_common::DEFAULT_PIN_PATH;
fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(EbpfCtlOutcome::Completed) => ExitCode::SUCCESS,
        #[cfg(target_os = "linux")]
        Ok(EbpfCtlOutcome::Detached { message }) => {
            eprintln!("{message}");
            ExitCode::from(ployz_ebpf_common::EBPF_STATUS_DETACHED_EXIT_CODE)
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EbpfCtlOutcome {
    Completed,
    #[cfg(target_os = "linux")]
    Detached {
        message: String,
    },
}

fn run(args: Vec<String>) -> Result<EbpfCtlOutcome, String> {
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
        EbpfCtlCommand::Status { bridge_ifname } => {
            return attachment_status(&bridge_ifname, pin_path);
        }
        EbpfCtlCommand::Route { action } => match action {
            RouteCommand::ReplaceAllIfname { ifname, subnets } => {
                route_replace_all_ifname(&ifname, &subnets, pin_path)
            }
        },
    }
    .map(|()| EbpfCtlOutcome::Completed)
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
    Status {
        bridge_ifname: String,
    },
    Route {
        #[command(subcommand)]
        action: RouteCommand,
    },
}

#[derive(Debug, Subcommand)]
enum RouteCommand {
    ReplaceAllIfname {
        ifname: String,
        #[arg(value_parser = parse_ipv4_subnet)]
        subnets: Vec<ipnet::Ipv4Net>,
    },
}

fn validate_bytecode(path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("read eBPF bytecode {}: {error}", path.display()))?;
    ployz_ebpf_common::validate_ployz_tc_bytecode(bytes.as_slice())
        .map(|_| ())
        .map_err(|error| format!("validate eBPF bytecode {}: {error:?}", path.display()))
}

#[cfg(any(target_os = "linux", test))]
fn tc_filter_has_program(output: &[u8], program: &str) -> Result<bool, String> {
    let filters = serde_json::from_slice::<serde_json::Value>(output)
        .map_err(|error| format!("invalid tc JSON: {error}"))?;
    let Some(filters) = filters.as_array() else {
        return Err("tc JSON response is not an array".to_owned());
    };
    Ok(filters.iter().any(|filter| {
        filter.get("kind").and_then(serde_json::Value::as_str) == Some("bpf")
            && filter
                .get("options")
                .and_then(|options| options.get("bpf_name"))
                .and_then(serde_json::Value::as_str)
                == Some(program)
    }))
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
fn attachment_status(bridge: &str, pin_path: Option<&str>) -> Result<EbpfCtlOutcome, String> {
    linux::attachment_status(bridge, pin_path.unwrap_or(DEFAULT_PIN_PATH))
}

#[cfg(not(target_os = "linux"))]
fn attachment_status(_bridge: &str, _pin_path: Option<&str>) -> Result<EbpfCtlOutcome, String> {
    Err("status requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn route_replace_all_ifname(
    ifname: &str,
    subnets: &[ipnet::Ipv4Net],
    pin_path: Option<&str>,
) -> Result<(), String> {
    linux::route_replace_all_ifname(ifname, subnets, pin_path.unwrap_or(DEFAULT_PIN_PATH))
}

#[cfg(not(target_os = "linux"))]
fn route_replace_all_ifname(
    _ifname: &str,
    _subnets: &[ipnet::Ipv4Net],
    _pin_path: Option<&str>,
) -> Result<(), String> {
    Err("route replace-all-ifname requires Linux".to_owned())
}

#[cfg(any(target_os = "linux", test))]
fn subnet_to_key(subnet: ipnet::Ipv4Net) -> ployz_ebpf_common::RouteKey {
    let network_addr: Ipv4Addr = subnet.network();
    ployz_ebpf_common::RouteKey {
        network: u32::from(network_addr).to_be(),
        prefix_len: u32::from(subnet.prefix_len()),
    }
}

#[cfg(any(target_os = "linux", test))]
fn stale_route_keys(
    observed: &[ployz_ebpf_common::RouteKey],
    desired: &[ployz_ebpf_common::RouteKey],
) -> Vec<ployz_ebpf_common::RouteKey> {
    observed
        .iter()
        .copied()
        .filter(|observed| {
            !desired.iter().any(|desired| {
                desired.network == observed.network && desired.prefix_len == observed.prefix_len
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{
        EbpfCtlOutcome, stale_route_keys, subnet_to_key, tc_filter_has_program, validate_bytecode,
    };
    use aya::Ebpf;
    use aya::programs::links::{FdLink, LinkOrder, PinnedLink};
    use aya::programs::tc::{NlOptions, TcAttachOptions};
    use aya::programs::{SchedClassifier, TcAttachType};
    use ployz_ebpf_common::{RouteEntry, RouteKey};
    use std::path::Path;
    use std::process::Command;

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

        std::fs::create_dir_all(pin_path).map_err(|error| format!("create {pin_path}: {error}"))?;
        // clsact backs the classic-netlink fallback; harmless (and ignored) under tcx.
        let _ = aya::programs::tc::qdisc_add_clsact(bridge);

        let egress: &mut SchedClassifier = bpf
            .program_mut("ployz_egress")
            .ok_or("ployz_egress program not found")?
            .try_into()
            .map_err(|error| format!("open ployz_egress: {error}"))?;
        egress
            .load()
            .map_err(|error| format!("load ployz_egress: {error}"))?;
        attach_and_persist(
            egress,
            bridge,
            TcAttachType::Egress,
            &format!("{pin_path}/egress-link"),
            "ployz_egress",
        )?;

        let ingress: &mut SchedClassifier = bpf
            .program_mut("ployz_ingress")
            .ok_or("ployz_ingress program not found")?
            .try_into()
            .map_err(|error| format!("open ployz_ingress: {error}"))?;
        ingress
            .load()
            .map_err(|error| format!("load ployz_ingress: {error}"))?;
        attach_and_persist(
            ingress,
            bridge,
            TcAttachType::Ingress,
            &format!("{pin_path}/ingress-link"),
            "ployz_ingress",
        )?;

        let wg_map = bpf
            .map_mut("WG_IFINDEX")
            .ok_or("WG_IFINDEX map not found")?;
        let mut wg_ifindex_map = aya::maps::Array::<_, u32>::try_from(wg_map)
            .map_err(|error| format!("open WG_IFINDEX map: {error}"))?;
        wg_ifindex_map
            .set(0, wg_ifindex, 0)
            .map_err(|error| format!("set WG_IFINDEX: {error}"))?;

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

    /// Attaches one classifier and makes the attachment outlive this process.
    /// Prefers tcx (kernel 6.6+): the returned link is pinned into bpffs, so the
    /// program stays attached after this short-lived CLI exits. Where tcx is
    /// unavailable (older kernels, some container netns) it falls back to a
    /// classic netlink filter, which the clsact qdisc keeps after the process
    /// exits — the pre-tcx behavior the DinD harness exercises.
    fn attach_and_persist(
        program: &mut SchedClassifier,
        bridge: &str,
        attach_type: TcAttachType,
        link_pin: &str,
        program_name: &str,
    ) -> Result<(), String> {
        match program.attach_with_options(
            bridge,
            attach_type,
            TcAttachOptions::TcxOrder(LinkOrder::default()),
        ) {
            Ok(link_id) => {
                let link = program
                    .take_link(link_id)
                    .map_err(|error| format!("take {program_name} tcx link: {error}"))?;
                let fd_link = FdLink::try_from(link)
                    .map_err(|error| format!("{program_name} tcx link is not fd-backed: {error}"))?;
                // Replace any stale pin so a retry after a failed prepare is idempotent.
                remove_pin_file(link_pin)?;
                fd_link
                    .pin(link_pin)
                    .map_err(|error| format!("pin {program_name} tcx link at {link_pin}: {error}"))?;
                Ok(())
            }
            Err(tcx_error) => program
                .attach_with_options(
                    bridge,
                    attach_type,
                    TcAttachOptions::Netlink(NlOptions::default()),
                )
                .map(|_link_id| ())
                .map_err(|netlink_error| {
                    format!(
                        "attach {program_name} to {bridge} (tcx: {tcx_error}; netlink: {netlink_error})"
                    )
                }),
        }
    }

    pub fn ensure_attached(
        bytecode: &Path,
        bridge: &str,
        wg_ifname: &str,
        pin_path: &str,
    ) -> Result<(), String> {
        validate_bytecode(bytecode)?;
        if let EbpfCtlOutcome::Completed = attachment_status(bridge, pin_path)? {
            return Ok(());
        }
        // Clear any partial state from a previous failed prepare, attach fresh,
        // then re-verify so that a success here means the classifier is really
        // attached (not merely that `attach` returned).
        let _ = detach(bridge, pin_path);
        attach(bytecode, bridge, wg_ifname, pin_path)?;
        match attachment_status(bridge, pin_path)? {
            EbpfCtlOutcome::Completed => Ok(()),
            EbpfCtlOutcome::Detached { message } => {
                Err(format!("eBPF attach did not persist: {message}"))
            }
        }
    }

    pub fn attachment_status(bridge: &str, pin_path: &str) -> Result<EbpfCtlOutcome, String> {
        // tcx path: a pinned, loadable link IS the attachment. This is the
        // source of truth on kernel 6.6+; classic `tc filter show` never lists
        // tcx programs, so it must not decide "attached" there.
        match tcx_links_present(pin_path)? {
            Some(true) => return Ok(EbpfCtlOutcome::Completed),
            Some(false) => {
                return Ok(EbpfCtlOutcome::Detached {
                    message: format!("pinned eBPF link under {pin_path} is stale"),
                });
            }
            None => {}
        }
        // Classic-netlink fallback: filters kept alive by the clsact qdisc.
        if !ployz_tc_pins_exist(pin_path) {
            return Ok(EbpfCtlOutcome::Detached {
                message: format!("required eBPF pins are missing under {pin_path}"),
            });
        }
        for (direction, program) in [("ingress", "ployz_ingress"), ("egress", "ployz_egress")] {
            let output = Command::new("tc")
                .args(["-j", "filter", "show", "dev", bridge, direction])
                .output()
                .map_err(|error| format!("inspect tc {direction} attachment: {error}"))?;
            if !output.status.success() {
                return Err(format!(
                    "inspect tc {direction} attachment: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            let attached = tc_filter_has_program(&output.stdout, program)
                .map_err(|error| format!("inspect tc {direction} attachment: {error}"))?;
            if !attached {
                return Ok(EbpfCtlOutcome::Detached {
                    message: format!("{program} is not attached to {bridge} {direction}"),
                });
            }
        }
        Ok(EbpfCtlOutcome::Completed)
    }

    /// tcx attachment detection. `Some(true)`: both links pinned and loadable.
    /// `Some(false)`: a pin is present but stale/unloadable. `None`: no link
    /// pins at all (a classic-netlink attachment, or nothing attached).
    fn tcx_links_present(pin_path: &str) -> Result<Option<bool>, String> {
        let links = [
            format!("{pin_path}/ingress-link"),
            format!("{pin_path}/egress-link"),
        ];
        if links.iter().all(|path| !Path::new(path).exists()) {
            return Ok(None);
        }
        for path in &links {
            // `from_pin` opens the pinned link; dropping it keeps the pin (and
            // so the attachment) and only closes our extra fd.
            if !Path::new(path).exists() || PinnedLink::from_pin(path).is_err() {
                return Ok(Some(false));
            }
        }
        Ok(Some(true))
    }

    pub fn detach(bridge: &str, pin_path: &str) -> Result<(), String> {
        // tcx: removing the pin drops the last reference and detaches.
        remove_pin_file(format!("{pin_path}/ingress-link"))?;
        remove_pin_file(format!("{pin_path}/egress-link"))?;
        // Classic-netlink fallback: remove the filters if they are present.
        detach_program_if_present(bridge, TcAttachType::Egress, "ployz_egress")?;
        detach_program_if_present(bridge, TcAttachType::Ingress, "ployz_ingress")?;
        remove_pin_file(format!("{pin_path}/routes"))?;
        remove_pin_file(format!("{pin_path}/egress"))?;
        remove_pin_file(format!("{pin_path}/ingress"))?;
        remove_pin_dir(pin_path)?;
        Ok(())
    }

    fn detach_program_if_present(
        bridge: &str,
        attach_type: TcAttachType,
        program: &str,
    ) -> Result<(), String> {
        match aya::programs::tc::qdisc_detach_program(bridge, attach_type, program) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("detach {program} from {bridge}: {error}")),
        }
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

    pub fn route_replace_all_ifname(
        ifname: &str,
        subnets: &[ipnet::Ipv4Net],
        pin_path: &str,
    ) -> Result<(), String> {
        let ifindex = resolve_ifindex(ifname)?;
        let desired = subnets
            .iter()
            .copied()
            .map(subnet_to_key)
            .collect::<Vec<_>>();
        let mut map = open_routes_map(pin_path)?;
        for key in &desired {
            map.insert(PodRouteKey(*key), PodRouteEntry(RouteEntry { ifindex }), 0)
                .map_err(|error| format!("insert desired route: {error}"))?;
        }
        let observed = map
            .keys()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("list pinned routes: {error}"))?;
        let observed = observed.iter().map(|key| key.0).collect::<Vec<_>>();
        for key in stale_route_keys(&observed, &desired) {
            map.remove(&PodRouteKey(key))
                .map_err(|error| format!("remove stale route: {error}"))?;
        }
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
    fn whole_map_replacement_selects_only_observed_keys_absent_from_desired() {
        let retained = subnet_to_key("10.42.2.0/24".parse().expect("valid subnet"));
        let stale = subnet_to_key("10.42.3.0/24".parse().expect("valid subnet"));
        let new = subnet_to_key("10.42.4.0/24".parse().expect("valid subnet"));

        let selected = stale_route_keys(&[retained, stale], &[retained, new]);

        let [selected] = selected.as_slice() else {
            panic!("exactly one stale route expected: {}", selected.len());
        };
        assert_eq!(selected.network, stale.network);
        assert_eq!(selected.prefix_len, stale.prefix_len);
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
            "replace-all-ifname".to_owned(),
            "missing-wg".to_owned(),
            "10.42.2.0/24".to_owned(),
        ])
        .expect_err("route replacement requires a live Linux interface and map");

        assert!(
            error.contains("route replace-all-ifname requires Linux")
                || error.contains("interface missing-wg not found")
                || error.contains("No such file")
                || error.contains("open pinned ROUTES map")
        );
    }

    #[test]
    fn invalid_subnet_is_rejected_before_route_execution() {
        let error = run(vec![
            "route".to_owned(),
            "replace-all-ifname".to_owned(),
            "ployz-wg0".to_owned(),
            "not-a-subnet".to_owned(),
        ])
        .expect_err("invalid subnet fails");

        assert!(error.contains("invalid IPv4 subnet"));
    }

    #[test]
    fn legacy_route_mutation_commands_are_not_exposed() {
        for action in ["add", "add-ifname", "del", "sync-ifname"] {
            let error = EbpfCtlCli::try_parse_from(["ployz-ebpf-ctl", "route", action])
                .expect_err("legacy route command must be rejected")
                .to_string();
            assert!(
                error.contains("unrecognized subcommand"),
                "{action}: {error}"
            );
        }
    }

    #[test]
    fn tc_filter_status_requires_the_owned_program_name() {
        let output = br#"[
            {"kind":"bpf","options":{"bpf_name":"ployz_ingress"}},
            {"kind":"flower","options":{"bpf_name":"ployz_egress"}}
        ]"#;

        assert_eq!(tc_filter_has_program(output, "ployz_ingress"), Ok(true));
        assert_eq!(tc_filter_has_program(output, "ployz_egress"), Ok(false));
    }

    #[test]
    fn tc_filter_status_rejects_malformed_json() {
        assert!(tc_filter_has_program(b"not-json", "ployz_ingress").is_err());
    }
}
