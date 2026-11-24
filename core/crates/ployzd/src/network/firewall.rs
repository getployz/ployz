use std::{
    io::{self, Write},
    path::Path,
    process::{Command, Output, Stdio},
    sync::{Mutex, MutexGuard, PoisonError},
};

use ployz_core::{
    AddressPart, CLOUDFLARE_EDGE_PORT, CLOUDFLARE_RANGES, MANAGEMENT_PORT, MachineSubnet,
    ManagementAddress, VOLUME_SEND_PORT, mesh_cidr,
};

use crate::dns;

use super::{
    CORROSION_GOSSIP_PORT, DOCKER_NETWORK_NAME, MACHINE_API_PORT, NetworkError,
    TUNNEL_NETWORK_NAME, UNREGISTRY_PORT, WIREGUARD_INTERFACE_NAME, WIREGUARD_PORT,
    docker_firewall::Ipv6Off,
};

const INPUT_CHAIN: &str = "PLOYZ-INPUT";
/// One chain per build slot, hooked into OUTPUT, so slots never rewrite each
/// other's fence.
const BUILD_CHAIN: &str = "PLOYZ-BUILD";
/// Hooked first in mangle FORWARD for traffic off any build bridge.
const BUILD_FORWARD_CHAIN: &str = "PLOYZ-BUILD-FORWARD";
/// Hooked first in mangle FORWARD for traffic off the Tunnel Door's bridge.
const TUNNEL_FORWARD_CHAIN: &str = "PLOYZ-TUNNEL-FORWARD";
/// Hooked first in DOCKER-USER for traffic into the ployz bridge.
const FENCE_CHAIN: &str = "PLOYZ-FENCE";

/// Each writer here moves hooks from a listing it read first, and a Build's
/// fence runs beside the network's periodic re-apply. One writer at a time
/// keeps that listing true when the restore lands. The count names the current
/// [`Installed`] firewall: every install and every removal moves it on.
static WRITER: Mutex<u64> = Mutex::new(0);

fn writer() -> MutexGuard<'static, u64> {
    WRITER.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the Docker bridge was fenced from the mesh before the firewall is
/// installed, so the install keeps that fence or says why it can't.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeFence {
    /// The fence went in before the bridge took IPv6. An install that can't keep it
    /// fails, since the bridge's IPv6 would otherwise reach the mesh unfenced.
    Required,
    /// The bridge holds no IPv6, so the install adds the fence only while Docker
    /// filters IPv6.
    IfAvailable,
}

/// A firewall [`apply_firewall_rules`] installed. It applies again only while it
/// is still the current one, so a re-apply queued before a teardown or a later
/// install never puts back rules for addresses this Machine no longer holds.
#[derive(Clone, Copy, Debug)]
pub struct Installed {
    generation: u64,
    subnet: MachineSubnet,
    management_address: ManagementAddress,
    fence: BridgeFence,
}

/// What a re-apply had to put back.
#[derive(Debug, Default)]
pub struct Restored {
    /// Each hook or ordered rule that was missing or no longer first, as
    /// `<table> <chain>: <rule>`, and the TCPMSS clamp when it was missing.
    pub displaced: Vec<String>,
    /// The mesh's nat POSTROUTING RETURN, which Docker displaces with its own
    /// MASQUERADE each time it creates a network or restarts.
    pub behind_docker_nat: Vec<String>,
}

impl Installed {
    /// Apply this firewall again, as [`apply_firewall_rules`] did. `None` when it
    /// was removed or replaced since, and nothing was written.
    ///
    /// # Errors
    ///
    /// Returns an error when an iptables command fails, as [`apply_firewall_rules`]
    /// does.
    pub fn reapply(&self) -> Result<Option<Restored>, NetworkError> {
        let generation = writer();
        if *generation != self.generation {
            return Ok(None);
        }
        install(self.subnet, self.management_address, self.fence).map(Some)
    }
}

/// Apply the Machine's ingress and forwarding policy, including the private
/// Machine API, Direct Image Transfer and Volume send endpoints on `management_address`.
///
/// Each address family's policy lands in one restore transaction, so a running
/// Machine never passes through a state that admits less or more than either the
/// old policy or the new one. Applying again converges: the Ployz chains are
/// rewritten whole, and each hook and ordered rule goes back to the top of its
/// chain when another tool has displaced or deleted it.
///
/// Returns the firewall now installed, which [`Installed::reapply`] applies
/// again until [`remove_firewall_rules`] or the next install.
///
/// # Errors
///
/// Returns an error when an iptables command cannot be executed or rejects a
/// required rule, or when `fence` is [`BridgeFence::Required`] and Docker no
/// longer filters IPv6.
pub fn apply_firewall_rules(
    subnet: MachineSubnet,
    management_address: ManagementAddress,
    fence: BridgeFence,
) -> Result<Installed, NetworkError> {
    let mut generation = writer();
    *generation += 1;
    install(subnet, management_address, fence)?;
    Ok(Installed {
        generation: *generation,
        subnet,
        management_address,
        fence,
    })
}

/// Called with [`writer`] held.
fn install(
    subnet: MachineSubnet,
    management_address: ManagementAddress,
    fence: BridgeFence,
) -> Result<Restored, NetworkError> {
    let mut restored = Restored::default();
    let hooks: Vec<Hook> = hooks(subnet).into_iter().chain(fence_hooks()).collect();
    let placed = |place| hooks.iter().filter(move |hook: &&Hook| hook.place == place);
    let gateway = subnet.gateway().0;
    let dns_port = dns::PORT;
    let mut ipv4 = mesh_ports();
    for protocol in ["udp", "tcp"] {
        ipv4.push(format!(
            "-i {DOCKER_NETWORK_NAME} -d {gateway} -p {protocol} --dport {dns_port} -j ACCEPT"
        ));
    }
    let mut ipv4_document =
        Chain::input().document("iptables", &ipv4, &[], &mut restored.displaced)?;
    ipv4_document.push_str(&Chain::fence().document(
        "iptables",
        &fence_rules(),
        &[outbound_fence().rule],
        &mut restored.displaced,
    )?);
    for hook in placed(Place::BehindDockerNat) {
        ipv4_document.push_str(&hook.document(&mut restored.behind_docker_nat)?);
    }
    let moved = &mut restored.displaced;
    for hook in placed(Place::First) {
        ipv4_document.push_str(&hook.document(moved)?);
    }
    // The tunnel's hook first, so the build hook's insert lands above it.
    ipv4_document.push_str(&tunnel_forward_document(
        "iptables",
        &ipv4_tunnel_forwarding(),
        moved,
    )?);
    ipv4_document.push_str(&build_forward_document(
        "iptables",
        &ipv4_build_forwarding(),
        moved,
    )?);
    restore("iptables", &ipv4_document)?;
    for hook in placed(Place::Appended) {
        hook.append(moved);
    }
    for hook in placed(Place::Retired) {
        hook.delete()?;
    }
    let mut ipv6_document = Chain::input().document(
        "ip6tables",
        &management_policy(management_address),
        &[],
        moved,
    )?;
    if keeps_fence(fence, docker_ipv6_firewall())? {
        for hook in placed(Place::Fence) {
            ipv6_document.push_str(&hook.document(moved)?);
        }
        let ahead = [container_mesh_fence().rule];
        for hook in placed(Place::BehindMeshFence) {
            ipv6_document.push_str(&hook.document_behind(&ahead, moved)?);
        }
    }
    // The tunnel's hook first, so the build hook's insert lands above it.
    ipv6_document.push_str(&tunnel_forward_document(
        "ip6tables",
        &ipv6_tunnel_forwarding(),
        moved,
    )?);
    ipv6_document.push_str(&build_forward_document(
        "ip6tables",
        &ipv6_build_forwarding(),
        moved,
    )?);
    restore("ip6tables", &ipv6_document)?;
    docker_forwards_through_user(moved)?;
    filter_bridged()?;
    Ok(restored)
}

/// A rule this firewall puts in a chain it doesn't own.
struct Hook {
    program: &'static str,
    table: &'static str,
    chain: &'static str,
    rule: String,
    place: Place,
}

/// How an install puts a [`Hook`] in place.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Place {
    /// First in its chain, in its family's restore. Docker displaces it with its own
    /// MASQUERADE each time it creates a network or restarts, so its moves are
    /// recorded apart.
    BehindDockerNat,
    /// First in its chain, in its family's restore.
    First,
    /// First in its chain, in its family's restore, while the install keeps the fence
    /// (see [`keeps_fence`]). Removed while its chain exists.
    Fence,
    /// As [`Place::Fence`], but right behind the mesh fence, so neither moves the other.
    BehindMeshFence,
    /// Appended outside the restore (see [`Hook::append`]).
    Appended,
    /// An earlier daemon's: every install and every removal deletes it.
    Retired,
}

/// Every hook, in the order a removal deletes them.
fn hooks(subnet: MachineSubnet) -> Vec<Hook> {
    let mut hooks = vec![
        // Earlier daemons accepted the mesh into the bridge here. PLOYZ-FENCE now accepts
        // what of it passes the fence.
        Hook {
            program: "iptables",
            table: "filter",
            chain: "DOCKER-USER",
            rule: format!("-i {WIREGUARD_INTERFACE_NAME} -o {DOCKER_NETWORK_NAME} -j ACCEPT"),
            place: Place::Retired,
        },
        Hook {
            program: "iptables",
            table: "nat",
            chain: "POSTROUTING",
            rule: format!("-s {subnet} -o {WIREGUARD_INTERFACE_NAME} -j RETURN"),
            place: Place::BehindDockerNat,
        },
    ];
    hooks.extend(loose_build_forwarding().map(|rule| Hook {
        program: "iptables",
        table: "mangle",
        chain: "FORWARD",
        rule,
        place: Place::Retired,
    }));
    hooks.push(mss_clamp());
    hooks.push(container_mesh_fence());
    hooks
}

/// The fence's hooks outside PLOYZ-FENCE's own jump. Only [`remove_fence`] takes them
/// down, once the bridge they guard is gone or a release that doesn't know them takes over.
fn fence_hooks() -> [Hook; 2] {
    [outbound_fence(), container_ipv6_fence()]
}

/// A Container's traffic to a peer's Container passes the fence here too, before Docker's
/// MASQUERADE could give it this Server's address while the mesh's nat RETURN is
/// displaced.
fn outbound_fence() -> Hook {
    Hook {
        program: "iptables",
        table: "filter",
        chain: "DOCKER-USER",
        rule: format!("-i {DOCKER_NETWORK_NAME} -o {WIREGUARD_INTERFACE_NAME} -j {FENCE_CHAIN}"),
        place: Place::First,
    }
}

impl Hook {
    fn words(&self) -> Vec<&str> {
        self.rule.split(' ').collect()
    }

    /// Restore lines that put this hook first in its chain, in a section of their own;
    /// none when it already is.
    fn document(&self, moved: &mut Vec<String>) -> Result<String, NetworkError> {
        let moves = move_to_top(self.program, self.table, self.chain, &self.rule, moved)?;
        Ok(if moves.is_empty() {
            moves
        } else {
            format!("*{}\n{moves}COMMIT\n", self.table)
        })
    }

    /// As [`Self::document`], but right after the leading rules among `ahead`.
    fn document_behind(
        &self,
        ahead: &[String],
        moved: &mut Vec<String>,
    ) -> Result<String, NetworkError> {
        let moves = move_behind(
            self.program,
            self.table,
            self.chain,
            &self.rule,
            ahead,
            moved,
        )?;
        Ok(if moves.is_empty() {
            moves
        } else {
            format!("*{}\n{moves}COMMIT\n", self.table)
        })
    }

    /// Append this hook outside the policy's restore: a kernel without the TCPMSS target
    /// loses only the clamp, and the route MTU still covers the Machine's own TCP. An
    /// added hook is recorded in `moved`.
    fn append(&self, moved: &mut Vec<String>) {
        let words = self.words();
        let added = rule_exists(self.program, self.table, self.chain, &words).and_then(|exists| {
            if exists {
                return Ok(false);
            }
            let mut append = vec!["-t", self.table, "-A", self.chain];
            append.extend_from_slice(&words);
            xtables(self.program, &append).map(|_| true)
        });
        match added {
            Ok(true) => moved.push(format!("{} {}: {}", self.table, self.chain, self.rule)),
            Ok(false) => {}
            Err(error) => eprintln!(
                "failed to clamp container TCP to the mesh path MTU: {error}",
                error = ployz_core::error_chain::inline(&error),
            ),
        }
    }

    fn delete(&self) -> Result<(), NetworkError> {
        delete_rule(self.program, self.table, self.chain, &self.words())
    }
}

/// Whether an install adds the mesh fence, given what the Docker probe found.
fn keeps_fence(fence: BridgeFence, probe: Result<(), Ipv6Off>) -> Result<bool, NetworkError> {
    match (fence, probe) {
        (_, Ok(())) => Ok(true),
        (BridgeFence::IfAvailable, Err(_)) => Ok(false),
        (BridgeFence::Required, Err(off)) => Err(NetworkError::BridgeUnfenced {
            cause: off.to_string(),
        }),
    }
}

/// A Service Container's TCP to a peer's Service crosses this Machine's router. Its SYN
/// and SYN-ACK leave over WireGuard clamped to the MTU of the peer's route, which the
/// path probe lowers on a path that loses large packets, so the far end sends this
/// container segments that cross. The container's own segments are bounded by the
/// route MTU as they're forwarded. On a full route it changes nothing. Container IPv6 never enters the mesh
/// (see [`container_mesh_fence`]), so IPv4 is the whole of it.
fn mss_clamp() -> Hook {
    Hook {
        program: "iptables",
        table: "mangle",
        chain: "FORWARD",
        rule: format!(
            "-o {WIREGUARD_INTERFACE_NAME} -p tcp -m tcp --tcp-flags SYN,RST SYN -j TCPMSS --clamp-mss-to-pmtu"
        ),
        place: Place::Appended,
    }
}

/// Docker's NAT66 would rewrite a container's IPv6 into the mesh to this Machine's
/// management address, which every peer trusts. Nothing in the mesh dials a
/// container over IPv6, so even replies drop: a container can forge the state of a
/// DNAT'd connection through hairpin NAT. Container IPv4 into the mesh keeps its own
/// source and reaches only containers, so it stays open.
fn container_mesh_fence() -> Hook {
    Hook {
        program: "ip6tables",
        table: "filter",
        chain: "DOCKER-USER",
        rule: format!("-i {DOCKER_NETWORK_NAME} -o {WIREGUARD_INTERFACE_NAME} -j DROP"),
        place: Place::Fence,
    }
}

/// Container IPv6 addresses aren't static and Internal DNS answers IPv4 alone, so the
/// fence can't tell a Container's part over IPv6. No Container reaches another on the
/// bridge over IPv6; outside clients still reach the Ingress Proxy over it.
fn container_ipv6_fence() -> Hook {
    Hook {
        program: "ip6tables",
        table: "filter",
        chain: "DOCKER-USER",
        rule: format!("-i {DOCKER_NETWORK_NAME} -o {DOCKER_NETWORK_NAME} -j DROP"),
        place: Place::BehindMeshFence,
    }
}

/// `-s` or `-d` over every Machine Subnet's addresses in `part`, by the last octet alone:
/// the bridge reaches only this Server's subnet, and WireGuard binds each peer's traffic
/// to its own.
fn part_match(flag: &str, part: AddressPart) -> String {
    let (value, mask) = part.last_octet_mask();
    format!("{flag} 0.0.0.{value}/0.0.0.{mask}")
}

/// What reaches a Container on the ployz bridge, by the part of its address and the
/// part of the source's. A Container on this Server arrives from the bridge and one on
/// a peer from the mesh; either reaches a signed data Container only from Open or
/// signed data. Anything else reaches one only through a port its signed spec
/// publishes. Docker's own rules then decide what passes. Traffic from the bridge into
/// the mesh meets the same bridge rules on its way to a peer's Container.
fn fence_rules() -> Vec<String> {
    let into_data = part_match("-d", AddressPart::SignedData);
    let unsigned = [AddressPart::Preview, AddressPart::Free].map(|part| part_match("-s", part));
    let mut rules = vec!["-m conntrack --ctstate RELATED,ESTABLISHED -j RETURN".to_owned()];
    for (interface, otherwise) in [
        (DOCKER_NETWORK_NAME, "RETURN"),
        (WIREGUARD_INTERFACE_NAME, "ACCEPT"),
    ] {
        rules.extend(
            unsigned
                .iter()
                .map(|source| format!("-i {interface} {source} {into_data} -j DROP")),
        );
        rules.push(format!("-i {interface} -j {otherwise}"));
    }
    rules.push("-m conntrack --ctstate DNAT -j RETURN".to_owned());
    rules.push(format!("{into_data} -j DROP"));
    rules
}

/// Where the fence lives for the boot to restore before Docker starts any Container. Its
/// presence says this Server has had a ployz bridge.
pub const FENCE_FILE: &str = "fence.rules";

/// The fence's restore document in each family: PLOYZ-FENCE and its jump, and the IPv6
/// bridge drop.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FenceFile {
    iptables: String,
    ip6tables: String,
}

impl FenceFile {
    fn new() -> Self {
        let chain = Chain::fence();
        let mut iptables = format!("*filter\n:{FENCE_CHAIN} - [0:0]\n");
        for rule in fence_rules() {
            iptables.push_str(&format!("-A {FENCE_CHAIN} {rule}\n"));
        }
        let outbound = outbound_fence();
        iptables.push_str(&format!(
            "-I {} 1 {}\n-I {} 1 {}\nCOMMIT\n",
            chain.hook, chain.jump, outbound.chain, outbound.rule
        ));
        let drop = container_ipv6_fence();
        Self {
            iptables,
            ip6tables: format!("*filter\n-I {} 1 {}\nCOMMIT\n", drop.chain, drop.rule),
        }
    }
}

/// Write the fence for the boot to restore, before the bridge first exists.
pub(super) fn write_fence_file(data_dir: &Path) -> io::Result<()> {
    let body = serde_json::to_vec_pretty(&FenceFile::new()).map_err(io::Error::other)?;
    crate::filesystem::atomic_write(&data_dir.join(FENCE_FILE), &body, 0o600)
}

/// Put the fence back before Docker starts at boot, from the host alone: Docker isn't
/// running, so its IPv6 probe is never asked. Traffic between two Containers on the bridge
/// passes iptables only through `br_netfilter`, so it is loaded first. A Server with no
/// fence file never had a bridge, and needs nothing more.
///
/// # Errors
///
/// Returns why the fence or bridge filtering could not be put in place; Docker must not
/// start then.
pub fn restore_fence(data_dir: &Path) -> Result<(), NetworkError> {
    let ipv6 = Path::new("/proc/net/if_inet6").exists();
    filter_bridged()?;
    let path = data_dir.join(FENCE_FILE);
    let unreadable = |error: &dyn std::fmt::Display| {
        io::Error::other(format!(
            "the fence file {} can't be read: {error}",
            path.display()
        ))
    };
    let body = match std::fs::read(&path) {
        Ok(body) => body,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(unreadable(&error).into()),
    };
    let file: FenceFile = serde_json::from_slice(&body).map_err(|error| unreadable(&error))?;
    // Docker's nftables backend never sends forwarded IPv4 through DOCKER-USER, and Docker
    // restores its Containers the moment it starts, so it must not start at all.
    if let Some(cause) = super::docker_firewall::configured_ipv4_off() {
        return Err(NetworkError::DockerFirewallBackend { cause: Some(cause) });
    }
    let _writer = writer();
    for (program, document, on) in [
        ("iptables", &file.iptables, true),
        ("ip6tables", &file.ip6tables, ipv6),
    ] {
        if !on {
            continue;
        }
        if !command_succeeds(program, &["-t", "filter", "-S", "DOCKER-USER"]) {
            xtables(program, &["-t", "filter", "-N", "DOCKER-USER"])?;
        }
        restore(program, &moving_inserts(program, document)?)?;
    }
    // Docker puts its own jump first when it starts; until then, this one sends forwarded
    // IPv4 through the fence ahead of anything a host firewall accepts.
    if !matches!(user_jump(&forward_listing()?), UserJump::Ahead) {
        put_user_jump_first()?;
    }
    Ok(())
}

/// `document` with each first-place insert into DOCKER-USER taking the place of a copy
/// already there, so a restore run again leaves one.
fn moving_inserts(program: &str, document: &str) -> Result<String, NetworkError> {
    let mut moved = String::new();
    for line in document.lines() {
        if let Some(rule) = line.strip_prefix("-I DOCKER-USER 1 ")
            && rule_exists(
                program,
                "filter",
                "DOCKER-USER",
                &rule.split(' ').collect::<Vec<_>>(),
            )?
        {
            moved.push_str(&format!("-D DOCKER-USER {rule}\n"));
        }
        moved.push_str(line);
        moved.push('\n');
    }
    Ok(moved)
}

/// Bridged traffic between two Containers on this Server reaches iptables only through
/// `br_netfilter` and its two sysctls. Docker leaves the module unloaded on some hosts, and
/// a sysctl.d file applied when it loads, or later, can turn a sysctl off, so each install
/// checks both and says what it had to turn back on. A host without IPv6 may have no
/// IPv6 sysctl.
fn filter_bridged() -> Result<(), NetworkError> {
    let sysctl = |family| format!("/proc/sys/net/bridge/bridge-nf-call-{family}");
    let mut turned_on = Vec::new();
    if !Path::new(&sysctl("iptables")).exists() {
        super::checked_command("modprobe", &["br_netfilter"])?;
        turned_on.push("loaded br_netfilter".to_owned());
    }
    for family in ["iptables", "ip6tables"] {
        let path = sysctl(family);
        let value = match std::fs::read_to_string(&path) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound && family == "ip6tables" => {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if value.trim() != "1" {
            turn_on(family, &path)?;
            turned_on.push(format!("set net.bridge.bridge-nf-call-{family}=1"));
        }
    }
    if !turned_on.is_empty() {
        eprintln!(
            "traffic between Containers on the {DOCKER_NETWORK_NAME} network skipped the firewall, so Ployz turned it back on: {}",
            turned_on.join("; "),
        );
    }
    Ok(())
}

/// Under an older release's `ployz.service`, which an older upgrade worker leaves in place,
/// /proc/sys is read-only to the daemon, so systemd sets the sysctl from outside the sandbox.
fn turn_on(family: &str, path: &str) -> Result<(), NetworkError> {
    match std::fs::write(path, "1") {
        Err(error) if error.kind() == io::ErrorKind::ReadOnlyFilesystem => {
            let setting = format!("net.bridge.bridge-nf-call-{family}=1");
            super::checked_command(
                "systemd-run",
                &["--wait", "--collect", "--quiet", "sysctl", "-w", &setting],
            )
            .map(drop)
        }
        written => Ok(written?),
    }
}

/// Docker sends forwarded IPv4 through DOCKER-USER, where the fence hooks, only with its
/// iptables firewall backend. Without that jump the fence is a chain nothing reaches, and
/// behind a rule that accepts bridge traffic it is one that traffic skips, so the jump goes
/// back first, noted in `moved`.
fn docker_forwards_through_user(moved: &mut Vec<String>) -> Result<(), NetworkError> {
    // Docker keeps the old jump when it moves to nftables, so the jump alone proves nothing.
    if let Some(cause) = super::docker_firewall::ipv4_off() {
        return Err(NetworkError::DockerFirewallBackend { cause: Some(cause) });
    }
    match user_jump(&forward_listing()?) {
        UserJump::Ahead => Ok(()),
        UserJump::Missing => Err(NetworkError::DockerFirewallBackend { cause: None }),
        UserJump::Shadowed => {
            put_user_jump_first()?;
            moved.push(format!("-A FORWARD {USER_JUMP}"));
            Ok(())
        }
    }
}

const USER_JUMP: &str = "-j DOCKER-USER";

fn forward_listing() -> Result<String, NetworkError> {
    let listing = xtables("iptables", &["-t", "filter", "-S", "FORWARD"])?;
    Ok(String::from_utf8_lossy(&listing.stdout).into_owned())
}

/// One restore, so forwarded traffic never passes FORWARD with no jump.
fn put_user_jump_first() -> Result<(), NetworkError> {
    let delete = if rule_exists("iptables", "filter", "FORWARD", &["-j", "DOCKER-USER"])? {
        format!("-D FORWARD {USER_JUMP}\n")
    } else {
        String::new()
    };
    restore(
        "iptables",
        &format!("*filter\n{delete}-I FORWARD 1 {USER_JUMP}\nCOMMIT\n"),
    )
}

/// Where FORWARD jumps to DOCKER-USER, by `iptables -S FORWARD`.
#[derive(Debug, PartialEq)]
enum UserJump {
    /// Ahead of every rule that could accept traffic into the bridge.
    Ahead,
    /// Behind such a rule, which that traffic takes instead.
    Shadowed,
    Missing,
}

fn user_jump(listing: &str) -> UserJump {
    let mut shadowed = false;
    for rule in listing
        .lines()
        .filter_map(|line| line.trim_end().strip_prefix("-A FORWARD "))
    {
        if rule == USER_JUMP {
            return if shadowed {
                UserJump::Shadowed
            } else {
                UserJump::Ahead
            };
        }
        shadowed |= may_accept_into_bridge(rule);
    }
    UserJump::Missing
}

/// Whether a FORWARD rule can accept traffic out to the bridge, or hand it to a chain that
/// may: any target but a drop, a reject or a log, unless it matches only another output
/// interface.
fn may_accept_into_bridge(rule: &str) -> bool {
    let words = rule.split_whitespace().collect::<Vec<_>>();
    let after = |flag: &str| {
        words
            .iter()
            .position(|word| *word == flag)
            .and_then(|at| Some((at, *words.get(at + 1)?)))
    };
    let target = after("-j")
        .or_else(|| after("-g"))
        .map(|(_, target)| target);
    if matches!(target, None | Some("DROP" | "REJECT" | "LOG")) {
        return false;
    }
    let Some((at, out)) = after("-o") else {
        return true;
    };
    let negated = at.checked_sub(1).and_then(|before| words.get(before)) == Some(&"!");
    let names_bridge = match out.strip_suffix('+') {
        Some(prefix) => DOCKER_NETWORK_NAME.starts_with(prefix),
        None => out == DOCKER_NETWORK_NAME,
    };
    names_bridge != negated
}

/// Whether PLOYZ-FENCE is in iptables now.
#[must_use]
pub fn fence_active() -> bool {
    command_succeeds("iptables", &["-t", "filter", "-S", FENCE_CHAIN])
}

/// Take the fence out of both families: once the bridge it guards is gone, or for a release
/// that doesn't know it.
///
/// # Errors
///
/// Returns the iptables failures, joined.
pub fn remove_fence() -> Result<(), NetworkError> {
    let _writer = writer();
    let mut failures = Vec::new();
    // The outbound hook jumps to PLOYZ-FENCE, which the kernel won't delete while
    // anything still jumps to it, so every hook goes first.
    let mut results = Vec::new();
    for hook in fence_hooks() {
        results.push(hook.delete());
    }
    results.push(Chain::fence().remove("iptables"));
    for result in results {
        if let Err(error) = result {
            failures.push(ployz_core::error_chain::inline(&error));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(NetworkError::Io(io::Error::other(failures.join("; "))))
    }
}

/// Docker filters IPv6 itself, so DOCKER-USER exists for the mesh fence. Without it
/// the bridge gets no IPv6. The chain outlives a Docker restart that turns
/// ip6tables off, so dockerd's own settings decide, and name the cause first.
pub(super) fn docker_ipv6_firewall() -> Result<(), Ipv6Off> {
    super::docker_firewall::docker_filters_ipv6()?;
    if docker_user_chain() {
        Ok(())
    } else {
        Err(Ipv6Off::NoDockerUserChain)
    }
}

fn docker_user_chain() -> bool {
    command_succeeds("ip6tables", &["-t", "filter", "-S", "DOCKER-USER"])
}

/// Install the mesh fence on its own, ahead of the rest of the firewall.
///
/// # Errors
///
/// Returns an error when ip6tables rejects the rule.
pub(super) fn fence_container_ipv6() -> Result<(), NetworkError> {
    let _writer = writer();
    let document = container_mesh_fence().document(&mut Vec::new())?;
    if document.is_empty() {
        Ok(())
    } else {
        restore("ip6tables", &document)
    }
}

/// Build steps route to the internet, never into the Machine's services, across
/// the mesh, to a cloud metadata service or to a private network. Mangle runs
/// before BuildKit's CNI accepts its own bridge's traffic in filter. DNS passes
/// to any resolver, since the host's may sit on the LAN or on link-local. A port
/// published on a public address passes, DNAT'd to its container; one the step
/// reached through a private address drops on that original destination.
/// Untracked traffic has no original destination, so the closing drops match
/// its destination instead.
fn ipv4_build_forwarding() -> Vec<String> {
    let mut rules = vec![
        format!("-o {WIREGUARD_INTERFACE_NAME} -j DROP"),
        format!("-o {DOCKER_NETWORK_NAME} -m conntrack ! --ctstate DNAT -j DROP"),
    ];
    rules.extend(dns_returns());
    rules.extend(
        PRIVATE_V4
            .iter()
            .map(|destination| format!("-m conntrack --ctorigdst {destination} -j DROP")),
    );
    rules.push("-m conntrack --ctstate DNAT -j RETURN".to_owned());
    rules.extend(
        PRIVATE_V4
            .iter()
            .map(|destination| format!("-d {destination} -j DROP")),
    );
    rules
}

/// Link-local, RFC1918, CGNAT, and Azure's WireServer, which hands out VM
/// configuration from a public address.
const PRIVATE_V4: [&str; 6] = [
    LINK_LOCAL,
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "100.64.0.0/10",
    "168.63.129.16/32",
];

/// Unique local and link-local addresses.
const PRIVATE_V6: [&str; 2] = ["fc00::/7", "fe80::/10"];

/// Build steps have no IPv6 route today; this holds if they ever get one.
fn ipv6_build_forwarding() -> Vec<String> {
    let mut rules = vec![format!("-o {WIREGUARD_INTERFACE_NAME} -j DROP")];
    rules.extend(dns_returns());
    rules.extend(
        PRIVATE_V6
            .iter()
            .map(|destination| format!("-d {destination} -j DROP")),
    );
    rules
}

fn dns_returns() -> impl Iterator<Item = String> {
    ["udp", "tcp"]
        .into_iter()
        .map(|protocol| format!("-p {protocol} --dport {DNS_PORT} -j RETURN"))
}

/// Loose rules main hooked into mangle FORWARD before this chain. A RETURN
/// from the chain resumes FORWARD, where their link-local drop would still
/// catch DNS to a cloud's link-local resolver.
fn loose_build_forwarding() -> [String; 3] {
    let bridges = build_bridges();
    [
        format!("-i {bridges} -o {DOCKER_NETWORK_NAME} -m conntrack ! --ctstate DNAT -j DROP"),
        format!("-i {bridges} -o {WIREGUARD_INTERFACE_NAME} -j DROP"),
        format!("-i {bridges} -d 169.254.0.0/16 -j DROP"),
    ]
}

/// Every build slot's bridge, as iptables names them.
fn build_bridges() -> String {
    format!("{}+", ployz_build::BUILD_BRIDGE_PREFIX)
}

fn build_forward_document(
    program: &str,
    rules: &[String],
    moved: &mut Vec<String>,
) -> Result<String, NetworkError> {
    Chain::build_forward().document(program, rules, &[], moved)
}

/// The Tunnel Connector dials whatever origin its tunnel's routes name, and a tunnel
/// managed in Cloudflare's dashboard takes its routes from Cloudflare. So the bridge
/// its Tunnel Origin owns reaches only what the connector itself needs: DNS to the resolver Docker forwards
/// to, which may sit on the LAN or on link-local, and Cloudflare's edge on the
/// tunnel port. Nothing on this Machine, on the mesh or on any other bridge answers
/// it, DNS included.
fn ipv4_tunnel_forwarding() -> Vec<String> {
    let mut rules: Vec<String> = [
        DOCKER_NETWORK_NAME,
        WIREGUARD_INTERFACE_NAME,
        &build_bridges(),
        "docker0",
    ]
    .iter()
    .map(|output| format!("-o {output} -j DROP"))
    .collect();
    rules.extend(dns_returns());
    rules.extend(edge_returns(|range| !range.contains(':')));
    rules.push("-j DROP".to_owned());
    rules
}

/// The bridge takes no IPv6, and nothing it would reach over IPv6 is the connector's.
fn ipv6_tunnel_forwarding() -> Vec<String> {
    vec!["-j DROP".to_owned()]
}

fn edge_returns(family: impl Fn(&str) -> bool) -> impl Iterator<Item = String> {
    CLOUDFLARE_RANGES
        .into_iter()
        .filter(move |range| family(range))
        .flat_map(|range| {
            ["udp", "tcp"].into_iter().map(move |protocol| {
                format!("-d {range} -p {protocol} --dport {CLOUDFLARE_EDGE_PORT} -j RETURN")
            })
        })
}

/// The tunnel's hook goes first in mangle FORWARD but for the build bridges' hook,
/// which matches other interfaces, so neither reapply moves the other. Its document
/// goes before the build one, whose insert at the top then lands above it.
fn tunnel_forward_document(
    program: &str,
    rules: &[String],
    moved: &mut Vec<String>,
) -> Result<String, NetworkError> {
    Chain::tunnel_forward().document(program, rules, &[Chain::build_forward().jump], moved)
}

const LINK_LOCAL: &str = "169.254.0.0/16";
const DNS_PORT: u16 = 53;

/// Fence one build slot's BuildKit worker. BuildKit runs on the host network,
/// so a Dockerfile `ADD` of a URL would otherwise reach everything the host
/// reaches. Fenced, it gets the internet, DNS, its own image contexts and the
/// mesh image servers, and nothing else on the host or the mesh. Like a build
/// step, it reaches no private address except for DNS. Its `FROM` pulls go
/// through the same fence, so a registry or mirror at a private address is out
/// of reach too.
///
/// Each family's chain and its OUTPUT hook land in one restore transaction.
/// The hook goes first in OUTPUT, ahead of any loopback accept.
///
/// # Errors
///
/// Returns why the fence could not be installed. The Build must not solve.
pub fn fence_builder(fence: &ployz_build::Fence) -> Result<(), String> {
    let _writer = writer();
    let mut moved = Vec::new();
    let chain = Chain::build_fence(fence.slot);
    for (program, rules, forwarding) in [
        ("iptables", ipv4_fence(fence), ipv4_build_forwarding()),
        ("ip6tables", ipv6_fence(fence), ipv6_build_forwarding()),
    ] {
        chain
            .document(program, &rules, &[], &mut moved)
            .and_then(
                |fence| Ok(fence + &build_forward_document(program, &forwarding, &mut moved)?),
            )
            .and_then(|document| restore(program, &document))
            .map_err(|error| ployz_core::error_chain::inline(&error))?;
    }
    Ok(())
}

/// Loopback DNS passes, then `drops` fall on the worker's sockets alone, then
/// DNS to any other address passes, since the host's resolver may sit on the
/// LAN or on link-local, then `private` drops. The cgroup sits on each drop,
/// not on a leading RETURN: a SYN-ACK or reset the kernel sends without a full
/// socket never matches a cgroup, so a negated match would fence every host
/// listener's replies.
fn fenced(
    fence: &ployz_build::Fence,
    loopback: &str,
    allowed: Vec<String>,
    drops: &[String],
    private: &[&str],
) -> Vec<String> {
    let mut rules: Vec<String> = ["udp", "tcp"]
        .iter()
        .map(|protocol| format!("-d {loopback} -p {protocol} --dport {DNS_PORT} -j RETURN"))
        .collect();
    rules.extend(allowed.into_iter().map(|rule| format!("{rule} -j RETURN")));
    let cgroup = fence.cgroup.as_str();
    rules.push(format!(
        "-m cgroup --path {cgroup} -m addrtype --dst-type LOCAL -j DROP"
    ));
    rules.extend(
        drops
            .iter()
            .map(|rule| format!("-m cgroup --path {cgroup} {rule} -j DROP")),
    );
    rules.extend(dns_returns());
    rules.extend(
        private
            .iter()
            .map(|destination| format!("-m cgroup --path {cgroup} -d {destination} -j DROP")),
    );
    rules
}

/// Docker's embedded DNS, which a host on a user-defined Docker network
/// resolves through, DNATs port 53 to a random port on the same address.
/// Filter OUTPUT sees the new port, so only that address may keep its DNS
/// through a redirect: one anywhere else could open any port to the worker.
const DOCKER_EMBEDDED_DNS: &str = "127.0.0.11/32";

fn ipv4_fence(fence: &ployz_build::Fence) -> Vec<String> {
    let allowed = ["udp", "tcp"]
        .iter()
        .map(|protocol| {
            format!(
                "-d {DOCKER_EMBEDDED_DNS} -p {protocol} -m conntrack --ctorigdst {DOCKER_EMBEDDED_DNS} --ctorigdstport {DNS_PORT}"
            )
        })
        .chain(
            fence
                .image_ports
                .iter()
                .map(|port| format!("-d 127.0.0.1/32 -p tcp --dport {port}")),
        )
        .collect();
    let mut drops: Vec<String> = [
        &build_bridges(),
        DOCKER_NETWORK_NAME,
        WIREGUARD_INTERFACE_NAME,
        "docker0",
    ]
    .iter()
    .map(|output| format!("-o {output}"))
    .collect();
    drops.push(format!("-d {}", ployz_core::DEFAULT_CLUSTER_NETWORK));
    fenced(fence, "127.0.0.0/8", allowed, &drops, &PRIVATE_V4)
}

fn ipv6_fence(fence: &ployz_build::Fence) -> Vec<String> {
    let mesh = mesh_cidr();
    let allowed = vec![format!("-d {mesh} -p tcp --dport {UNREGISTRY_PORT}")];
    let drops = [format!("-d {mesh}")];
    fenced(fence, "::1/128", allowed, &drops, &PRIVATE_V6)
}

/// Build steps run on their slot's bridge and get only the internet, and the Tunnel
/// Connector its bridge: nothing on the host answers either, on any address.
fn mesh_ports() -> Vec<String> {
    let mut rules = vec![
        format!("-i {} -j DROP", build_bridges()),
        format!("-i {TUNNEL_NETWORK_NAME} -j DROP"),
    ];
    rules.extend(
        [WIREGUARD_PORT, MANAGEMENT_PORT]
            .into_iter()
            .map(|port| format!("-p udp --dport {port} -j ACCEPT")),
    );
    rules
}

/// Accepts come first: the drops close the management ports to everything the
/// accepts don't name.
fn management_policy(management_address: ManagementAddress) -> Vec<String> {
    let management = format!("{}/128", management_address.0);
    let mesh = mesh_cidr();
    let mut rules = mesh_ports();
    for (protocol, port) in [("tcp", MACHINE_API_PORT), ("udp", CORROSION_GOSSIP_PORT)] {
        rules.push(format!(
            "-i {WIREGUARD_INTERFACE_NAME} -s {mesh} -p {protocol} --dport {port} -j ACCEPT"
        ));
    }
    for port in [UNREGISTRY_PORT, VOLUME_SEND_PORT] {
        rules.push(format!(
            "-i {WIREGUARD_INTERFACE_NAME} -s {mesh} -d {management} -p tcp --dport {port} -j ACCEPT"
        ));
    }
    rules.push(format!(
        "-i lo -s {management} -d {management} -p tcp --sport {UNREGISTRY_PORT} -j ACCEPT"
    ));
    rules.push(format!(
        "-i lo -d {management} -p tcp --dport {UNREGISTRY_PORT} -j ACCEPT"
    ));
    for port in [MACHINE_API_PORT, UNREGISTRY_PORT, VOLUME_SEND_PORT] {
        rules.push(format!("-d {management} -p tcp --dport {port} -j DROP"));
    }
    rules.push(format!(
        "-d {management} -p udp --dport {CORROSION_GOSSIP_PORT} -j DROP"
    ));
    rules
}

/// A Ployz chain, entered from `hook` in `table` by `jump`.
struct Chain {
    table: &'static str,
    hook: &'static str,
    name: String,
    jump: String,
}

impl Chain {
    fn input() -> Self {
        Self {
            table: "filter",
            hook: "INPUT",
            name: INPUT_CHAIN.to_owned(),
            jump: format!("-j {INPUT_CHAIN}"),
        }
    }

    fn build_forward() -> Self {
        Self {
            table: "mangle",
            hook: "FORWARD",
            name: BUILD_FORWARD_CHAIN.to_owned(),
            jump: format!("-i {} -j {BUILD_FORWARD_CHAIN}", build_bridges()),
        }
    }

    fn fence() -> Self {
        Self {
            table: "filter",
            hook: "DOCKER-USER",
            name: FENCE_CHAIN.to_owned(),
            jump: format!("-o {DOCKER_NETWORK_NAME} -j {FENCE_CHAIN}"),
        }
    }

    fn tunnel_forward() -> Self {
        Self {
            table: "mangle",
            hook: "FORWARD",
            name: TUNNEL_FORWARD_CHAIN.to_owned(),
            jump: format!("-i {TUNNEL_NETWORK_NAME} -j {TUNNEL_FORWARD_CHAIN}"),
        }
    }

    fn build_fence(slot: impl std::fmt::Display) -> Self {
        let name = format!("{BUILD_CHAIN}{slot}");
        Self {
            table: "filter",
            hook: "OUTPUT",
            jump: format!("-j {name}"),
            name,
        }
    }

    /// The chains every install writes but the fence's, in the order a removal takes them
    /// down.
    fn installed() -> [Self; 3] {
        [Self::input(), Self::build_forward(), Self::tunnel_forward()]
    }

    /// Declaring the chain in a `--noflush` restore replaces its rules in the same
    /// commit that fills it again. The jump goes first in the hook but for the
    /// leading rules among `ahead`.
    fn document(
        &self,
        program: &str,
        rules: &[String],
        ahead: &[String],
        moved: &mut Vec<String>,
    ) -> Result<String, NetworkError> {
        let Self {
            table,
            hook,
            name,
            jump,
        } = self;
        let mut document = format!("*{table}\n:{name} - [0:0]\n");
        for rule in rules {
            document.push_str(&format!("-A {name} {rule}\n"));
        }
        document.push_str(&move_behind(program, table, hook, jump, ahead, moved)?);
        document.push_str("COMMIT\n");
        Ok(document)
    }

    fn remove(&self, program: &str) -> Result<(), NetworkError> {
        let Self {
            table,
            hook,
            name,
            jump,
        } = self;
        let jump: Vec<&str> = jump.split(' ').collect();
        delete_rule(program, table, hook, &jump)?;
        if command_succeeds(program, &["-t", table, "-n", "-L", name]) {
            xtables(program, &["-t", table, "-F", name])?;
            xtables(program, &["-t", table, "-X", name])?;
        }
        Ok(())
    }
}

/// Restore lines that make `rule` the first in `chain`; none when it already is.
/// A rule that needs them is recorded in `moved`.
fn move_to_top(
    program: &str,
    table: &str,
    chain: &str,
    rule: &str,
    moved: &mut Vec<String>,
) -> Result<String, NetworkError> {
    move_behind(program, table, chain, rule, &[], moved)
}

/// Restore lines that put `rule` in `chain` right after the leading rules that are
/// among `ahead`, so first but for them; none when it already is there.
fn move_behind(
    program: &str,
    table: &str,
    chain: &str,
    rule: &str,
    ahead: &[String],
    moved: &mut Vec<String>,
) -> Result<String, NetworkError> {
    let listing = xtables(program, &["-t", table, "-S", chain])?;
    let listing = String::from_utf8_lossy(&listing.stdout);
    let appended = |rule: &str| format!("-A {chain} {rule}");
    let rules = listing
        .lines()
        .filter(|line| line.starts_with("-A "))
        .collect::<Vec<_>>();
    let leading = rules
        .iter()
        .take_while(|line| ahead.iter().any(|rule| **line == appended(rule)))
        .count();
    if rules.get(leading).copied() == Some(appended(rule).as_str()) {
        return Ok(String::new());
    }
    moved.push(format!("{table} {chain}: {rule}"));
    let words: Vec<&str> = rule.split(' ').collect();
    let mut moves = String::new();
    if rule_exists(program, table, chain, &words)? {
        moves.push_str(&format!("-D {chain} {rule}\n"));
    }
    moves.push_str(&format!("-I {chain} {} {rule}\n", leading + 1));
    Ok(moves)
}

fn restore(program: &str, document: &str) -> Result<(), NetworkError> {
    let program = format!("{program}-restore");
    let mut child = Command::new(&program)
        .args(["--noflush", &format!("--wait={LOCK_WAIT}")])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(document.as_bytes())?;
    let output = child.wait_with_output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(NetworkError::failed(program.as_ref(), &output))
    }
}

/// Remove the firewall but the fence, and retire the [`Installed`] one so no re-apply puts
/// it back. The fence stays while its bridge may: Docker keeps the bridge's Containers
/// running. [`remove_fence`] takes it down once the bridge is gone.
pub(super) fn remove_firewall_rules(subnet: MachineSubnet) -> Result<(), NetworkError> {
    let mut generation = writer();
    *generation += 1;
    let mut failures = Vec::new();
    let mut attempt = |result: Result<(), NetworkError>| {
        if let Err(error) = result {
            failures.push(ployz_core::error_chain::inline(&error));
        }
    };
    for hook in hooks(subnet) {
        // A fence left in a chain Docker abandoned is still ours to remove.
        if hook.place == Place::Fence && !docker_user_chain() {
            continue;
        }
        attempt(hook.delete());
    }
    for program in ["iptables", "ip6tables"] {
        for chain in Chain::installed() {
            attempt(chain.remove(program));
        }
        attempt(remove_build_fences(program));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(NetworkError::Io(std::io::Error::other(failures.join("; "))))
    }
}

/// Every slot's fence, whichever slots this Machine ever ran.
fn remove_build_fences(program: &str) -> Result<(), NetworkError> {
    let listing = xtables(program, &["-t", "filter", "-S"])?;
    let listing = String::from_utf8_lossy(&listing.stdout);
    let hooks = build_fence_hooks(&listing);
    for chain in &hooks.hooked {
        delete_rule(program, "filter", "OUTPUT", &["-j", chain])?;
    }
    for chain in &hooks.chains {
        xtables(program, &["-t", "filter", "-F", chain])?;
        xtables(program, &["-t", "filter", "-X", chain])?;
    }
    Ok(())
}

#[derive(Debug, Default, Eq, PartialEq)]
struct BuildFenceHooks {
    hooked: Vec<String>,
    chains: Vec<String>,
}

fn build_fence_hooks(listing: &str) -> BuildFenceHooks {
    let slot_chain = |name: &str| {
        name.strip_prefix(BUILD_CHAIN)
            .is_some_and(|slot| !slot.is_empty() && slot.bytes().all(|b| b.is_ascii_digit()))
    };
    let mut hooks = BuildFenceHooks::default();
    for line in listing.lines() {
        if let Some(chain) = line.strip_prefix("-A OUTPUT -j ")
            && slot_chain(chain)
        {
            hooks.hooked.push(chain.to_owned());
        } else if let Some(chain) = line.strip_prefix("-N ")
            && slot_chain(chain)
        {
            hooks.chains.push(chain.to_owned());
        }
    }
    hooks
}

fn delete_rule(program: &str, table: &str, chain: &str, rule: &[&str]) -> Result<(), NetworkError> {
    while rule_exists(program, table, chain, rule)? {
        let mut delete = vec!["-t", table, "-D", chain];
        delete.extend_from_slice(rule);
        xtables(program, &delete)?;
    }
    Ok(())
}

/// Seconds an iptables command waits for the xtables lock before it fails. A
/// held lock fails the step, and the next re-apply tries again.
const LOCK_WAIT: &str = "10";

fn xtables(program: &str, args: &[&str]) -> Result<Output, NetworkError> {
    super::checked_command(program, &waiting(args))
}

fn command_succeeds(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(waiting(args))
        .output()
        .is_ok_and(|output| output.status.success())
}

fn waiting<'arg>(args: &[&'arg str]) -> Vec<&'arg str> {
    let mut waiting = vec!["-w", LOCK_WAIT];
    waiting.extend_from_slice(args);
    waiting
}

/// Whether `rule` is in `chain`. iptables exits 1 when the rule or the chain is
/// missing, and 2 when the rule jumps to a chain that doesn't exist yet, so
/// neither can hold it. Any other failure, such as a held lock, is an error
/// rather than a missing rule.
fn rule_exists(
    program: &str,
    table: &str,
    chain: &str,
    rule: &[&str],
) -> Result<bool, NetworkError> {
    let mut args = waiting(&["-t", table, "-C", chain]);
    args.extend_from_slice(rule);
    let output = Command::new(program).args(&args).output()?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1 | 2) => Ok(false),
        _ => Err(NetworkError::failed(program.as_ref(), &output)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fence(image_ports: Vec<u16>) -> ployz_build::Fence {
        ployz_build::Fence {
            slot: 2,
            cgroup: ployz_build::CgroupPath::new("/system.slice/docker-abc.scope").unwrap(),
            image_ports,
        }
    }

    /// A host firewall reload can put a bridge ACCEPT ahead of Docker's jump, and then
    /// same-bridge traffic reaches a Volume holder without passing the fence.
    #[test]
    fn a_jump_behind_a_rule_that_accepts_bridge_traffic_is_shadowed() {
        let docker = "-P FORWARD DROP\n-A FORWARD -j DOCKER-USER\n-A FORWARD -j DOCKER-FORWARD\n";
        assert_eq!(user_jump(docker), UserJump::Ahead);
        assert_eq!(user_jump("-P FORWARD ACCEPT\n"), UserJump::Missing);
        for ahead in [
            "-A FORWARD -o ployz -j ACCEPT",
            "-A FORWARD -i ployz -j ACCEPT",
            "-A FORWARD -o ploy+ -j ACCEPT",
            "-A FORWARD ! -o eth0 -j ACCEPT",
            "-A FORWARD -j ufw-before-forward",
        ] {
            assert_eq!(
                user_jump(&format!("{ahead}\n{docker}")),
                UserJump::Shadowed,
                "{ahead}"
            );
        }
        for harmless in [
            "-A FORWARD -o eth0 -j ACCEPT",
            "-A FORWARD ! -o ployz -j ACCEPT",
            "-A FORWARD -o ployz -j DROP",
            "-A FORWARD -m conntrack --ctstate INVALID -j DROP",
        ] {
            assert_eq!(
                user_jump(&format!("{harmless}\n{docker}")),
                UserJump::Ahead,
                "{harmless}"
            );
        }
    }

    #[test]
    fn a_required_fence_fails_the_install_when_docker_stops_filtering_ipv6() {
        let error = keeps_fence(BridgeFence::Required, Err(Ipv6Off::NoDockerd)).unwrap_err();
        assert!(matches!(error, NetworkError::BridgeUnfenced { .. }));
        assert_eq!(
            error.to_string(),
            format!(
                "the Docker bridge was fenced from the mesh, but the fence can no longer be kept: {}",
                Ipv6Off::NoDockerd
            )
        );
    }

    #[test]
    fn an_available_fence_is_skipped_when_docker_does_not_filter_ipv6() {
        assert!(!keeps_fence(BridgeFence::IfAvailable, Err(Ipv6Off::NoDockerd)).unwrap());
        for fence in [BridgeFence::Required, BridgeFence::IfAvailable] {
            assert!(keeps_fence(fence, Ok(())).unwrap());
        }
    }

    #[test]
    fn the_worker_reaches_dns_and_its_image_contexts_but_nothing_else_on_the_host_or_a_private_network()
     {
        assert_eq!(
            ipv4_fence(&fence(vec![41_234])),
            [
                "-d 127.0.0.0/8 -p udp --dport 53 -j RETURN",
                "-d 127.0.0.0/8 -p tcp --dport 53 -j RETURN",
                "-d 127.0.0.11/32 -p udp -m conntrack --ctorigdst 127.0.0.11/32 --ctorigdstport 53 -j RETURN",
                "-d 127.0.0.11/32 -p tcp -m conntrack --ctorigdst 127.0.0.11/32 --ctorigdstport 53 -j RETURN",
                "-d 127.0.0.1/32 -p tcp --dport 41234 -j RETURN",
                "-m cgroup --path /system.slice/docker-abc.scope -m addrtype --dst-type LOCAL -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -o ployz-build+ -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -o ployz -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -o ployz-wg -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -o docker0 -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d 10.210.0.0/16 -j DROP",
                "-p udp --dport 53 -j RETURN",
                "-p tcp --dport 53 -j RETURN",
                "-m cgroup --path /system.slice/docker-abc.scope -d 169.254.0.0/16 -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d 10.0.0.0/8 -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d 172.16.0.0/12 -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d 192.168.0.0/16 -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d 100.64.0.0/10 -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d 168.63.129.16/32 -j DROP",
            ]
        );
    }

    #[test]
    fn the_loose_build_forwarding_rules_are_the_ones_earlier_daemons_hooked() {
        assert_eq!(
            loose_build_forwarding(),
            [
                "-i ployz-build+ -o ployz -m conntrack ! --ctstate DNAT -j DROP",
                "-i ployz-build+ -o ployz-wg -j DROP",
                "-i ployz-build+ -d 169.254.0.0/16 -j DROP",
            ]
        );
    }

    #[test]
    fn every_build_rule_names_the_bridges_by_the_prefix_builds_create_them_under() {
        let bridges = format!("{}+", ployz_build::BUILD_BRIDGE_PREFIX);
        for rule in loose_build_forwarding() {
            assert!(rule.starts_with(&format!("-i {bridges} ")), "{rule}");
        }
        assert_eq!(
            Chain::build_forward().jump,
            format!("-i {bridges} -j {BUILD_FORWARD_CHAIN}")
        );
        assert_eq!(mesh_ports().first(), Some(&format!("-i {bridges} -j DROP")));
    }

    #[test]
    fn the_worker_reaches_only_the_image_servers_across_the_mesh() {
        assert_eq!(
            ipv6_fence(&fence(vec![41_234])),
            [
                "-d ::1/128 -p udp --dport 53 -j RETURN",
                "-d ::1/128 -p tcp --dport 53 -j RETURN",
                "-d fdcc::/16 -p tcp --dport 7572 -j RETURN",
                "-m cgroup --path /system.slice/docker-abc.scope -m addrtype --dst-type LOCAL -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d fdcc::/16 -j DROP",
                "-p udp --dport 53 -j RETURN",
                "-p tcp --dport 53 -j RETURN",
                "-m cgroup --path /system.slice/docker-abc.scope -d fc00::/7 -j DROP",
                "-m cgroup --path /system.slice/docker-abc.scope -d fe80::/10 -j DROP",
            ]
        );
    }

    #[test]
    fn build_steps_reach_published_ports_and_dns_but_not_the_mesh_metadata_or_a_private_network() {
        assert_eq!(
            ipv4_build_forwarding(),
            [
                "-o ployz-wg -j DROP",
                "-o ployz -m conntrack ! --ctstate DNAT -j DROP",
                "-p udp --dport 53 -j RETURN",
                "-p tcp --dport 53 -j RETURN",
                "-m conntrack --ctorigdst 169.254.0.0/16 -j DROP",
                "-m conntrack --ctorigdst 10.0.0.0/8 -j DROP",
                "-m conntrack --ctorigdst 172.16.0.0/12 -j DROP",
                "-m conntrack --ctorigdst 192.168.0.0/16 -j DROP",
                "-m conntrack --ctorigdst 100.64.0.0/10 -j DROP",
                "-m conntrack --ctorigdst 168.63.129.16/32 -j DROP",
                "-m conntrack --ctstate DNAT -j RETURN",
                "-d 169.254.0.0/16 -j DROP",
                "-d 10.0.0.0/8 -j DROP",
                "-d 172.16.0.0/12 -j DROP",
                "-d 192.168.0.0/16 -j DROP",
                "-d 100.64.0.0/10 -j DROP",
                "-d 168.63.129.16/32 -j DROP",
            ]
        );
    }

    #[test]
    fn containers_never_reach_the_mesh_over_ipv6() {
        assert_eq!(container_mesh_fence().rule, "-i ployz -o ployz-wg -j DROP");
    }

    #[test]
    fn build_steps_never_reach_the_mesh_or_a_private_network_over_ipv6() {
        assert_eq!(
            ipv6_build_forwarding(),
            [
                "-o ployz-wg -j DROP",
                "-p udp --dport 53 -j RETURN",
                "-p tcp --dport 53 -j RETURN",
                "-d fc00::/7 -j DROP",
                "-d fe80::/10 -j DROP",
            ]
        );
    }

    #[test]
    fn teardown_finds_every_slots_fence_and_nothing_else() {
        let listing = "-P OUTPUT ACCEPT\n-N PLOYZ-BUILD0\n-N PLOYZ-BUILD12\n-N PLOYZ-BUILDX\n-N PLOYZ-BUILD-FORWARD\n-N PLOYZ-INPUT\n-A OUTPUT -j PLOYZ-BUILD12\n-A OUTPUT -j PLOYZ-BUILD0\n-A OUTPUT -j ufw-before-output\n-A FORWARD -i ployz-build+ -j PLOYZ-BUILD-FORWARD\n-A INPUT -j PLOYZ-INPUT\n";
        assert_eq!(
            build_fence_hooks(listing),
            BuildFenceHooks {
                hooked: vec!["PLOYZ-BUILD12".into(), "PLOYZ-BUILD0".into()],
                chains: vec!["PLOYZ-BUILD0".into(), "PLOYZ-BUILD12".into()],
            }
        );
    }

    /// Runs `test` as root in a fresh network namespace with
    /// `PLOYZ_TEARDOWN_CHILD` set, then lists each of `tables` there for both
    /// families.
    fn in_namespace(test: &str, tables: &[&str]) -> Vec<String> {
        let short = test.rsplit("::").next().unwrap().get(..8).unwrap();
        let namespace = format!("ployz-{short}-{}", std::process::id());
        let sudo = |args: &[&str]| Command::new("sudo").arg("-n").args(args).output().unwrap();
        assert!(sudo(&["ip", "netns", "add", &namespace]).status.success());
        let exe = std::env::current_exe().unwrap();
        let child = sudo(&[
            "ip",
            "netns",
            "exec",
            &namespace,
            "env",
            "PLOYZ_TEARDOWN_CHILD=1",
            exe.to_str().unwrap(),
            "--ignored",
            "--exact",
            test,
        ]);
        let left: Vec<_> = ["iptables", "ip6tables"]
            .into_iter()
            .flat_map(|program| tables.iter().map(move |table| (program, *table)))
            .map(|(program, table)| {
                sudo(&[
                    "ip", "netns", "exec", &namespace, program, "-t", table, "-S",
                ])
            })
            .collect();
        sudo(&["ip", "netns", "del", &namespace]);
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stdout)
        );
        left.into_iter()
            .map(|listing| {
                assert!(
                    listing.status.success(),
                    "{}",
                    String::from_utf8_lossy(&listing.stderr)
                );
                String::from_utf8(listing.stdout).unwrap()
            })
            .collect()
    }

    fn in_child() -> bool {
        std::env::var_os("PLOYZ_TEARDOWN_CHILD").is_some()
    }

    /// An earlier daemon's loose rules sit alongside this daemon's chain.
    #[test]
    #[ignore = "needs root and ip netns"]
    fn teardown_removes_the_chain_and_loose_rules_from_earlier_daemons() {
        if in_child() {
            for rule in loose_build_forwarding() {
                let mut insert = vec!["-t", "mangle", "-I", "FORWARD", "1"];
                insert.extend(rule.split(' '));
                xtables("iptables", &insert).unwrap();
            }
            restore(
                "iptables",
                &build_forward_document("iptables", &ipv4_build_forwarding(), &mut Vec::new())
                    .unwrap(),
            )
            .unwrap();
            restore(
                "ip6tables",
                &build_forward_document("ip6tables", &ipv6_build_forwarding(), &mut Vec::new())
                    .unwrap(),
            )
            .unwrap();
            for (program, rules) in [
                ("iptables", ipv4_tunnel_forwarding()),
                ("ip6tables", ipv6_tunnel_forwarding()),
            ] {
                restore(
                    program,
                    &tunnel_forward_document(program, &rules, &mut Vec::new()).unwrap(),
                )
                .unwrap();
            }
            remove_firewall_rules("10.210.1.0/24".parse().unwrap()).unwrap();
            return;
        }
        for rules in in_namespace(
            "network::firewall::tests::teardown_removes_the_chain_and_loose_rules_from_earlier_daemons",
            &["mangle"],
        ) {
            assert!(
                rules.lines().all(|line| line.starts_with("-P ")),
                "a rule survived teardown:\n{rules}"
            );
        }
    }

    /// The periodic re-apply finds every hook where the install put it, the build
    /// bridges' and the tunnel's in mangle FORWARD included, so it moves none.
    #[test]
    #[ignore = "needs root and ip netns"]
    fn reapplying_an_untouched_firewall_moves_nothing() {
        if in_child() {
            xtables("iptables", &["-N", "DOCKER-USER"]).unwrap();
            let installed = apply_firewall_rules(
                "10.210.1.0/24".parse().unwrap(),
                ManagementAddress("fdcc::1".parse().unwrap()),
                BridgeFence::IfAvailable,
            )
            .unwrap();
            for _ in 0..2 {
                let restored = installed.reapply().unwrap().unwrap();
                assert_eq!(restored.displaced, Vec::<String>::new());
            }
            return;
        }
        in_namespace(
            "network::firewall::tests::reapplying_an_untouched_firewall_moves_nothing",
            &["mangle"],
        );
    }

    /// A re-apply that was queued while reset removed the firewall runs last.
    #[test]
    #[ignore = "needs root and ip netns"]
    fn a_reapply_that_runs_after_teardown_puts_nothing_back() {
        if in_child() {
            xtables("iptables", &["-N", "DOCKER-USER"]).unwrap();
            let subnet = "10.210.1.0/24".parse().unwrap();
            let installed = apply_firewall_rules(
                subnet,
                ManagementAddress("fdcc::1".parse().unwrap()),
                BridgeFence::IfAvailable,
            )
            .unwrap();
            remove_firewall_rules(subnet).unwrap();
            assert!(installed.reapply().unwrap().is_none());
            return;
        }
        let listings = in_namespace(
            "network::firewall::tests::a_reapply_that_runs_after_teardown_puts_nothing_back",
            &["filter", "nat", "mangle"],
        );
        for rules in listings {
            assert!(
                rules
                    .lines()
                    .all(|line| line.starts_with("-P ") || line == "-N DOCKER-USER"),
                "a hook or chain survived teardown:\n{rules}"
            );
        }
    }
}
