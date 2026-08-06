#[cfg(any(target_os = "linux", test))]
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod isolation;

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
        EbpfCtlCommand::Isolation { action } => match action {
            IsolationCommand::EnsureAttached {
                bytecode,
                prefix,
                cgroup_root,
            } => isolation_ensure_attached(&bytecode, prefix, &cgroup_root, pin_path),
            IsolationCommand::ReplaceAll { rows_file } => {
                isolation_replace_all(&rows_file, pin_path)
            }
            IsolationCommand::Status => return isolation_status(pin_path),
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
    Isolation {
        #[command(subcommand)]
        action: IsolationCommand,
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

#[derive(Debug, Subcommand)]
enum IsolationCommand {
    EnsureAttached {
        bytecode: PathBuf,
        #[arg(value_parser = parse_isolation_prefix)]
        prefix: ipnet::Ipv4Net,
        cgroup_root: PathBuf,
    },
    ReplaceAll {
        rows_file: PathBuf,
    },
    Status,
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

fn parse_isolation_prefix(value: &str) -> Result<ipnet::Ipv4Net, String> {
    let prefix = parse_ipv4_subnet(value)?;
    if u32::from(prefix.prefix_len()) != ployz_ebpf_common::ISOLATION_PREFIX_LEN {
        return Err(format!(
            "isolation prefix must be /{}: {value}",
            ployz_ebpf_common::ISOLATION_PREFIX_LEN
        ));
    }
    Ok(prefix)
}

fn isolation_pin_path(pin_path: Option<&str>) -> Result<PathBuf, String> {
    isolation::sibling_pin_path(pin_path.unwrap_or(DEFAULT_PIN_PATH))
}

#[cfg(target_os = "linux")]
fn isolation_ensure_attached(
    bytecode: &Path,
    prefix: ipnet::Ipv4Net,
    cgroup_root: &Path,
    pin_path: Option<&str>,
) -> Result<(), String> {
    isolation::linux::ensure_attached(
        bytecode,
        prefix,
        cgroup_root,
        &isolation_pin_path(pin_path)?,
    )
}

#[cfg(not(target_os = "linux"))]
fn isolation_ensure_attached(
    _bytecode: &Path,
    _prefix: ipnet::Ipv4Net,
    _cgroup_root: &Path,
    _pin_path: Option<&str>,
) -> Result<(), String> {
    Err("isolation ensure-attached requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn isolation_replace_all(rows_file: &Path, pin_path: Option<&str>) -> Result<(), String> {
    isolation::linux::replace_all(rows_file, &isolation_pin_path(pin_path)?)
}

#[cfg(not(target_os = "linux"))]
fn isolation_replace_all(_rows_file: &Path, _pin_path: Option<&str>) -> Result<(), String> {
    Err("isolation replace-all requires Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn isolation_status(pin_path: Option<&str>) -> Result<EbpfCtlOutcome, String> {
    match isolation::linux::status(&isolation_pin_path(pin_path)?)? {
        isolation::IsolationStatus::Complete => Ok(EbpfCtlOutcome::Completed),
        isolation::IsolationStatus::Detached { message } => {
            Ok(EbpfCtlOutcome::Detached { message })
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn isolation_status(_pin_path: Option<&str>) -> Result<EbpfCtlOutcome, String> {
    Err("isolation status requires Linux".to_owned())
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
#[derive(Debug, PartialEq, Eq)]
struct RouteReplacementPlan {
    remove_before_insert: Vec<ployz_ebpf_common::RouteKey>,
    insert: Vec<ployz_ebpf_common::RouteKey>,
    remove_after_insert: Vec<ployz_ebpf_common::RouteKey>,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, PartialEq, Eq)]
enum RouteReplacementPlanError {
    DesiredSetTooLarge { desired: usize, capacity: usize },
    ObservedSetTooLarge { observed: usize, capacity: usize },
    StaleSlotsUnavailable { required: usize, available: usize },
}

#[cfg(any(target_os = "linux", test))]
impl std::fmt::Display for RouteReplacementPlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DesiredSetTooLarge { desired, capacity } => write!(
                formatter,
                "desired route count {desired} exceeds map capacity {capacity}"
            ),
            Self::ObservedSetTooLarge { observed, capacity } => write!(
                formatter,
                "observed route count {observed} exceeds map capacity {capacity}"
            ),
            Self::StaleSlotsUnavailable {
                required,
                available,
            } => write!(
                formatter,
                "replacement requires {required} stale slots but only {available} are available"
            ),
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn plan_route_replacement(
    observed: &[ployz_ebpf_common::RouteKey],
    desired: &[ployz_ebpf_common::RouteKey],
    capacity: usize,
) -> Result<RouteReplacementPlan, RouteReplacementPlanError> {
    let mut observed = observed.to_vec();
    observed.sort_unstable();
    observed.dedup();

    let mut desired = desired.to_vec();
    desired.sort_unstable();
    desired.dedup();

    if desired.len() > capacity {
        return Err(RouteReplacementPlanError::DesiredSetTooLarge {
            desired: desired.len(),
            capacity,
        });
    }
    if observed.len() > capacity {
        return Err(RouteReplacementPlanError::ObservedSetTooLarge {
            observed: observed.len(),
            capacity,
        });
    }

    let missing_desired = desired
        .iter()
        .filter(|key| observed.binary_search(key).is_err())
        .count();
    let free_slots = capacity - observed.len();
    let required_stale_slots = missing_desired.saturating_sub(free_slots);
    let mut stale = observed
        .into_iter()
        .filter(|key| desired.binary_search(key).is_err())
        .collect::<Vec<_>>();
    if stale.len() < required_stale_slots {
        return Err(RouteReplacementPlanError::StaleSlotsUnavailable {
            required: required_stale_slots,
            available: stale.len(),
        });
    }
    let remove_after_insert = stale.split_off(required_stale_slots);

    Ok(RouteReplacementPlan {
        remove_before_insert: stale,
        insert: desired,
        remove_after_insert,
    })
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{
        EbpfCtlOutcome, RouteReplacementPlan, plan_route_replacement, subnet_to_key,
        tc_filter_has_program, validate_bytecode,
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
        normalize_tc_detach_result(
            aya::programs::tc::qdisc_detach_program(bridge, attach_type, program),
            bridge,
            program,
        )
    }

    pub(super) fn normalize_tc_detach_result(
        result: Result<(), aya::programs::TcError>,
        bridge: &str,
        program: &str,
    ) -> Result<(), String> {
        match result {
            Ok(()) => Ok(()),
            Err(aya::programs::TcError::IoError(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(())
            }
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
        let observed = map
            .keys()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("list pinned routes: {error}"))?;
        let observed = observed.iter().map(|key| key.0).collect::<Vec<_>>();
        let RouteReplacementPlan {
            remove_before_insert,
            insert,
            remove_after_insert,
        } = plan_route_replacement(&observed, &desired, ployz_ebpf_common::MAX_ROUTES as usize)
            .map_err(|error| error.to_string())?;

        for key in remove_before_insert {
            map.remove(&PodRouteKey(key))
                .map_err(|error| format!("free stale route capacity: {error}"))?;
        }
        for key in insert {
            map.insert(PodRouteKey(key), PodRouteEntry(RouteEntry { ifindex }), 0)
                .map_err(|error| format!("insert desired route: {error}"))?;
        }
        for key in remove_after_insert {
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
    fn saturated_replacement_frees_only_the_required_stale_slot() {
        let observed = (0..ployz_ebpf_common::MAX_ROUTES)
            .map(test_route_key)
            .collect::<Vec<_>>();
        let desired = (1..=ployz_ebpf_common::MAX_ROUTES)
            .map(test_route_key)
            .collect::<Vec<_>>();

        let plan =
            plan_route_replacement(&observed, &desired, ployz_ebpf_common::MAX_ROUTES as usize)
                .expect("one stale slot makes the replacement possible");

        assert_eq!(plan.remove_before_insert, [test_route_key(0)]);
        assert_eq!(plan.insert, desired);
        assert!(plan.remove_after_insert.is_empty());
    }

    #[test]
    fn replacement_with_free_capacity_keeps_all_stale_deletions_after_inserts() {
        let retained = test_route_key(1);
        let stale = test_route_key(2);
        let new = test_route_key(3);

        let plan = plan_route_replacement(&[retained, stale], &[retained, new], 3)
            .expect("the missing desired route fits in the free slot");

        assert!(plan.remove_before_insert.is_empty());
        assert_eq!(plan.insert, [retained, new]);
        assert_eq!(plan.remove_after_insert, [stale]);
    }

    #[test]
    fn saturated_map_without_stale_capacity_rejects_oversized_desired_set() {
        let observed = (0..ployz_ebpf_common::MAX_ROUTES)
            .map(test_route_key)
            .collect::<Vec<_>>();
        let desired = (0..=ployz_ebpf_common::MAX_ROUTES)
            .map(test_route_key)
            .collect::<Vec<_>>();

        let error =
            plan_route_replacement(&observed, &desired, ployz_ebpf_common::MAX_ROUTES as usize)
                .expect_err("an oversized authoritative set must fail before mutation");

        assert_eq!(
            error,
            RouteReplacementPlanError::DesiredSetTooLarge {
                desired: ployz_ebpf_common::MAX_ROUTES as usize + 1,
                capacity: ployz_ebpf_common::MAX_ROUTES as usize,
            }
        );
    }

    #[test]
    fn replacement_plan_deduplicates_desired_routes() {
        let first = test_route_key(1);
        let second = test_route_key(2);

        let plan = plan_route_replacement(&[], &[second, first, second, first], 2)
            .expect("two distinct desired routes fit");

        assert_eq!(plan.insert, [first, second]);
    }

    #[test]
    fn replacement_retry_keeps_desired_routes_and_schedules_no_deletions() {
        let desired = [test_route_key(1), test_route_key(2)];

        let plan = plan_route_replacement(&desired, &desired, 2)
            .expect("an already-applied replacement remains valid");

        assert_eq!(plan.insert, desired);
        assert!(plan.remove_before_insert.is_empty());
        assert!(plan.remove_after_insert.is_empty());
    }

    fn test_route_key(network: u32) -> ployz_ebpf_common::RouteKey {
        ployz_ebpf_common::RouteKey {
            network,
            prefix_len: 32,
        }
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

    #[cfg(target_os = "linux")]
    #[test]
    fn classic_detach_treats_a_missing_owned_filter_as_already_detached() {
        let result = linux::normalize_tc_detach_result(
            Err(aya::programs::TcError::IoError(std::io::Error::from(
                std::io::ErrorKind::NotFound,
            ))),
            "br-ployz",
            "ployz_ingress",
        );

        assert_eq!(result, Ok(()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classic_detach_preserves_other_io_failures() {
        let result = linux::normalize_tc_detach_result(
            Err(aya::programs::TcError::IoError(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied,
            ))),
            "br-ployz",
            "ployz_ingress",
        );

        let error = result.expect_err("permission failures remain visible");
        assert!(error.contains("detach ployz_ingress from br-ployz"));
    }
}
