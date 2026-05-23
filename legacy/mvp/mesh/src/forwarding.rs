use std::collections::BTreeSet;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use mvp_bus::IslandId;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::{ContainerSubnet, MeshError, MeshResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForwardingObservation {
    Enabled,
    Disabled,
}

impl ForwardingObservation {
    #[must_use]
    pub fn as_bpfctl_arg(self) -> &'static str {
        match self {
            Self::Enabled => "on",
            Self::Disabled => "off",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayForwardingSnapshot {
    pub island: String,
    pub revision: u64,
    pub bridge_ifname: String,
    pub wireguard_ifname: String,
    pub local_subnet: ContainerSubnet,
    pub remote_subnets: Vec<ContainerSubnet>,
    pub observation: ForwardingObservation,
}

impl OverlayForwardingSnapshot {
    #[must_use]
    pub fn new(
        island: &IslandId,
        revision: u64,
        bridge_ifname: impl Into<String>,
        wireguard_ifname: impl Into<String>,
        local_subnet: ContainerSubnet,
        remote_subnets: impl IntoIterator<Item = ContainerSubnet>,
    ) -> Self {
        let mut remote_subnets = remote_subnets.into_iter().collect::<Vec<_>>();
        remote_subnets.sort();
        remote_subnets.dedup();
        Self {
            island: island.as_str().to_string(),
            revision,
            bridge_ifname: bridge_ifname.into(),
            wireguard_ifname: wireguard_ifname.into(),
            local_subnet,
            remote_subnets,
            observation: ForwardingObservation::Disabled,
        }
    }

    #[must_use]
    pub fn with_observation(mut self, observation: ForwardingObservation) -> Self {
        self.observation = observation;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayForwardingApplyReport {
    pub revision: u64,
    pub remote_route_count: usize,
}

pub trait OverlayForwardingBackend: Send + Sync {
    fn apply(
        &self,
        snapshot: &OverlayForwardingSnapshot,
    ) -> MeshResult<OverlayForwardingApplyReport>;
    fn detach(&self, bridge_ifname: &str) -> MeshResult<()>;
    fn last_applied(&self) -> MeshResult<Option<OverlayForwardingSnapshot>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxOverlayForwardingConfig {
    pub island: IslandId,
    pub snapshot_path: PathBuf,
    pub bpfctl_path: PathBuf,
    pub iptables_path: PathBuf,
}

impl LinuxOverlayForwardingConfig {
    #[must_use]
    pub fn new(island: IslandId, snapshot_path: impl Into<PathBuf>) -> Self {
        Self {
            island,
            snapshot_path: snapshot_path.into(),
            bpfctl_path: PathBuf::from("ployz-bpfctl"),
            iptables_path: PathBuf::from("iptables"),
        }
    }

    #[must_use]
    pub fn with_bpfctl_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.bpfctl_path = path.into();
        self
    }

    #[must_use]
    pub fn with_iptables_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.iptables_path = path.into();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxOverlayForwardingBackend {
    config: LinuxOverlayForwardingConfig,
}

impl LinuxOverlayForwardingBackend {
    #[must_use]
    pub fn new(config: LinuxOverlayForwardingConfig) -> Self {
        Self { config }
    }

    fn ensure_rules(&self, snapshot: &OverlayForwardingSnapshot) -> MeshResult<()> {
        for rule in iptables_rules(snapshot) {
            ensure_iptables_rule(&self.config.iptables_path, &rule)?;
        }
        Ok(())
    }

    fn execute_plan(&self, plan: LinuxForwardingPlan) -> MeshResult<()> {
        for command in plan.bpfctl_commands {
            run_command(&command.program, &command.args, "forwarding bpfctl")?;
        }
        Ok(())
    }
}

impl OverlayForwardingBackend for LinuxOverlayForwardingBackend {
    fn apply(
        &self,
        snapshot: &OverlayForwardingSnapshot,
    ) -> MeshResult<OverlayForwardingApplyReport> {
        if snapshot.island != self.config.island.as_str() {
            return Err(MeshError::InvalidSnapshot {
                path: self.config.snapshot_path.display().to_string(),
                message: format!(
                    "snapshot island {} did not match expected {}",
                    snapshot.island, self.config.island
                ),
            });
        }
        self.ensure_rules(snapshot)?;
        let previous = self.last_applied()?;
        let ifindex = resolve_ifindex(&snapshot.wireguard_ifname)?;
        let plan = LinuxForwardingPlan::new(&self.config, previous.as_ref(), snapshot, ifindex);
        self.execute_plan(plan)?;
        write_overlay_forwarding_snapshot(&self.config.snapshot_path, snapshot)?;
        Ok(OverlayForwardingApplyReport {
            revision: snapshot.revision,
            remote_route_count: snapshot.remote_subnets.len(),
        })
    }

    fn detach(&self, bridge_ifname: &str) -> MeshResult<()> {
        let command = ForwardingCommand::new(
            self.config.bpfctl_path.display().to_string(),
            ["detach".to_string(), bridge_ifname.to_string()],
        );
        run_command(&command.program, &command.args, "forwarding detach")
    }

    fn last_applied(&self) -> MeshResult<Option<OverlayForwardingSnapshot>> {
        load_optional_overlay_forwarding_snapshot(&self.config.snapshot_path, &self.config.island)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxForwardingPlan {
    pub iptables_rules: Vec<IptablesRule>,
    pub bpfctl_commands: Vec<ForwardingCommand>,
}

impl LinuxForwardingPlan {
    #[must_use]
    pub fn new(
        config: &LinuxOverlayForwardingConfig,
        previous: Option<&OverlayForwardingSnapshot>,
        desired: &OverlayForwardingSnapshot,
        wireguard_ifindex: u32,
    ) -> Self {
        let previous_routes = previous
            .map(|snapshot| {
                snapshot
                    .remote_subnets
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let desired_routes = desired
            .remote_subnets
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let mut bpfctl_commands = vec![ForwardingCommand::new(
            config.bpfctl_path.display().to_string(),
            [
                "attach".to_string(),
                desired.bridge_ifname.clone(),
                desired.wireguard_ifname.clone(),
            ],
        )];
        for stale in previous_routes.difference(&desired_routes) {
            bpfctl_commands.push(ForwardingCommand::new(
                config.bpfctl_path.display().to_string(),
                ["route".to_string(), "del".to_string(), stale.to_string()],
            ));
        }
        for subnet in desired_routes {
            bpfctl_commands.push(ForwardingCommand::new(
                config.bpfctl_path.display().to_string(),
                [
                    "route".to_string(),
                    "add".to_string(),
                    subnet.to_string(),
                    wireguard_ifindex.to_string(),
                ],
            ));
        }
        bpfctl_commands.push(ForwardingCommand::new(
            config.bpfctl_path.display().to_string(),
            [
                "observe".to_string(),
                desired.observation.as_bpfctl_arg().to_string(),
            ],
        ));
        Self {
            iptables_rules: iptables_rules(desired),
            bpfctl_commands,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardingCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl ForwardingCommand {
    #[must_use]
    pub fn new(program: impl Into<PathBuf>, args: impl IntoIterator<Item = String>) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IptablesRule {
    pub table: &'static str,
    pub chain: &'static str,
    pub args: Vec<String>,
}

fn iptables_rules(snapshot: &OverlayForwardingSnapshot) -> Vec<IptablesRule> {
    let subnet = snapshot.local_subnet.to_string();
    vec![
        IptablesRule {
            table: "raw",
            chain: "PREROUTING",
            args: vec![
                "-i".to_string(),
                snapshot.wireguard_ifname.clone(),
                "-d".to_string(),
                subnet.clone(),
                "-j".to_string(),
                "ACCEPT".to_string(),
            ],
        },
        IptablesRule {
            table: "filter",
            chain: "FORWARD",
            args: vec![
                "-i".to_string(),
                snapshot.wireguard_ifname.clone(),
                "-o".to_string(),
                snapshot.bridge_ifname.clone(),
                "-d".to_string(),
                subnet.clone(),
                "-j".to_string(),
                "ACCEPT".to_string(),
            ],
        },
        IptablesRule {
            table: "filter",
            chain: "FORWARD",
            args: vec![
                "-i".to_string(),
                snapshot.bridge_ifname.clone(),
                "-o".to_string(),
                snapshot.wireguard_ifname.clone(),
                "-s".to_string(),
                subnet.clone(),
                "-j".to_string(),
                "ACCEPT".to_string(),
            ],
        },
        IptablesRule {
            table: "nat",
            chain: "POSTROUTING",
            args: vec![
                "-s".to_string(),
                subnet,
                "-o".to_string(),
                snapshot.wireguard_ifname.clone(),
                "-j".to_string(),
                "ACCEPT".to_string(),
            ],
        },
    ]
}

fn ensure_iptables_rule(iptables_path: &Path, rule: &IptablesRule) -> MeshResult<()> {
    let mut check_args = vec![
        "-w".to_string(),
        "-t".to_string(),
        rule.table.to_string(),
        "-C".to_string(),
        rule.chain.to_string(),
    ];
    check_args.extend(rule.args.clone());
    if run_command_allow_failure(iptables_path, &check_args, "iptables check")?.success() {
        return Ok(());
    }

    let mut insert_args = vec![
        "-w".to_string(),
        "-t".to_string(),
        rule.table.to_string(),
        "-I".to_string(),
        rule.chain.to_string(),
        "1".to_string(),
    ];
    insert_args.extend(rule.args.clone());
    run_command(iptables_path, &insert_args, "iptables insert")
}

fn run_command(program: &Path, args: &[String], operation: &'static str) -> MeshResult<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|source| MeshError::Backend {
            operation,
            message: source.to_string(),
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(MeshError::Backend {
        operation,
        message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

fn run_command_allow_failure(
    program: &Path,
    args: &[String],
    operation: &'static str,
) -> MeshResult<std::process::ExitStatus> {
    Command::new(program)
        .args(args)
        .status()
        .map_err(|source| MeshError::Backend {
            operation,
            message: source.to_string(),
        })
}

fn resolve_ifindex(ifname: &str) -> MeshResult<u32> {
    let path = Path::new("/sys/class/net").join(ifname).join("ifindex");
    let value = fs::read_to_string(&path).map_err(|source| MeshError::Backend {
        operation: "resolve ifindex",
        message: format!("{}: {source}", path.display()),
    })?;
    value
        .trim()
        .parse::<u32>()
        .map_err(|source| MeshError::Backend {
            operation: "resolve ifindex",
            message: source.to_string(),
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForwardingRouteKey {
    pub network_be: u32,
    pub prefix_len: u32,
}

#[must_use]
pub fn forwarding_route_key(subnet: ContainerSubnet) -> ForwardingRouteKey {
    ForwardingRouteKey {
        network_be: u32::from(subnet.as_net().network()).to_be(),
        prefix_len: subnet.as_net().prefix_len() as u32,
    }
}

pub fn write_overlay_forwarding_snapshot(
    path: &Path,
    snapshot: &OverlayForwardingSnapshot,
) -> MeshResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| snapshot_io("create parent", parent, error))?;
    }
    let bytes =
        serde_json::to_vec_pretty(snapshot).map_err(|error| MeshError::InvalidSnapshot {
            path: path.display().to_string(),
            message: error.to_string(),
        })?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temp =
        NamedTempFile::new_in(parent).map_err(|error| snapshot_io("create temp", parent, error))?;
    temp.write_all(&bytes)
        .map_err(|error| snapshot_io("write temp", temp.path(), error))?;
    temp.as_file_mut()
        .sync_all()
        .map_err(|error| snapshot_io("sync temp", temp.path(), error))?;
    temp.persist(path)
        .map(|_file| ())
        .map_err(|error| snapshot_io("persist", path, error.error))
}

fn load_optional_overlay_forwarding_snapshot(
    path: &Path,
    expected_island: &IslandId,
) -> MeshResult<Option<OverlayForwardingSnapshot>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(snapshot_io("read", path, error)),
    };
    let snapshot =
        serde_json::from_slice::<OverlayForwardingSnapshot>(&bytes).map_err(|error| {
            MeshError::InvalidSnapshot {
                path: path.display().to_string(),
                message: error.to_string(),
            }
        })?;
    if snapshot.island != expected_island.as_str() {
        return Err(MeshError::InvalidSnapshot {
            path: path.display().to_string(),
            message: format!(
                "snapshot island {} did not match expected {}",
                snapshot.island, expected_island
            ),
        });
    }
    Ok(Some(snapshot))
}

fn snapshot_io(operation: &'static str, path: &Path, error: std::io::Error) -> MeshError {
    MeshError::SnapshotIo {
        operation,
        path: path.display().to_string(),
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use mvp_bus::IslandId;

    use crate::ContainerSubnet;

    use super::{
        ForwardingObservation, LinuxForwardingPlan, LinuxOverlayForwardingConfig,
        OverlayForwardingSnapshot, forwarding_route_key, iptables_rules,
    };

    fn subnet(value: &str) -> ContainerSubnet {
        ContainerSubnet::new(value.parse().expect("valid subnet")).expect("container subnet")
    }

    #[test]
    fn route_key_matches_bpfctl_network_byte_order() {
        let key = forwarding_route_key(subnet("10.210.7.0/24"));

        assert_eq!(key.network_be.to_ne_bytes(), [10, 210, 7, 0]);
        assert_eq!(key.prefix_len, 24);
    }

    #[test]
    fn forwarding_plan_attaches_reconciles_routes_and_sets_observation() {
        let config = LinuxOverlayForwardingConfig::new(IslandId::new("prod"), "forwarding.json")
            .with_bpfctl_path("/usr/local/bin/ployz-bpfctl");
        let previous = OverlayForwardingSnapshot::new(
            &IslandId::new("prod"),
            1,
            "br-old",
            "wg0",
            subnet("10.210.1.0/24"),
            [subnet("10.210.8.0/24"), subnet("10.210.9.0/24")],
        );
        let desired = OverlayForwardingSnapshot::new(
            &IslandId::new("prod"),
            2,
            "br-new",
            "wg0",
            subnet("10.210.1.0/24"),
            [subnet("10.210.9.0/24"), subnet("10.210.10.0/24")],
        )
        .with_observation(ForwardingObservation::Enabled);

        let plan = LinuxForwardingPlan::new(&config, Some(&previous), &desired, 42);
        let commands = plan
            .bpfctl_commands
            .iter()
            .map(|command| command.args.clone())
            .collect::<Vec<_>>();

        assert_eq!(
            commands,
            vec![
                vec!["attach", "br-new", "wg0"],
                vec!["route", "del", "10.210.8.0/24"],
                vec!["route", "add", "10.210.9.0/24", "42"],
                vec!["route", "add", "10.210.10.0/24", "42"],
                vec!["observe", "on"],
            ]
        );
    }

    #[test]
    fn iptables_rules_only_reference_interfaces_and_subnets() {
        let snapshot = OverlayForwardingSnapshot::new(
            &IslandId::new("prod"),
            2,
            "br-test",
            "wg-test",
            subnet("10.210.1.0/24"),
            [subnet("10.210.9.0/24")],
        );

        let rules = iptables_rules(&snapshot);

        assert_eq!(rules.len(), 4);
        assert!(
            rules
                .iter()
                .all(|rule| rule.args.contains(&"ACCEPT".to_string()))
        );
        assert!(
            rules
                .iter()
                .flat_map(|rule| rule.args.iter())
                .all(|arg| !arg.contains("web") && !arg.contains("api"))
        );
    }
}
