//! Region placement: what a Deploy run by a runner does to a Service's replicas as Servers
//! change region, join, leave and stop taking new starts, and as the Environment's region,
//! a Service's counts and its version change between Deploys.
//!
//! Each Deploy calls the real planner, `ployz::deploy::preview_deploy`, with the runner's
//! options (`block_unmountable`), on a `DeploySnapshot` built from the model's world. The
//! world then takes the operations it returns, each as one completed step, as an executor
//! that never fails would run them: each new Container is made from the spec in its
//! operation, and a replacement counts as work. So `Spread::hold`, `select_replicated`, `withdraw`,
//! `plan_one_service`, `spread_machines` and the Volume placement in `planning/volumes.rs`
//! are checked as written. Three pieces are copies, and drift unless kept in step:
//!
//! - lowering a Service's counts and the Environment's region into its spec, as
//!   `lower_snapshot` in ployz-core config/lowering.rs does (its `placement` and `regions`);
//! - the Store's rule that a Service with a Volume counts replicas in one region
//!   (`Problem::Regions` in `problems`, ployz-store rules.rs), by never offering such counts;
//! - the Store's record of the Deploy: each Service's node outcome (`node_outcomes` in
//!   ployz-store deployment/outcomes.rs), the Deployment's end (a Refusal fails it, `record`
//!   in deployment/record.rs), `all_waiting` (deployment/mod.rs) and `live`
//!   (deployment/history.rs), which skips a Deployment that applied nothing.
//!
//! Out of scope: execution failures, health checks and rollbacks; Bridge endpoint capacity
//! and host ports (unbounded and none); Global Services; dependencies; a Server that
//! doesn't answer; Drains and Volume moves.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    num::{NonZeroU8, NonZeroU32, NonZeroU64},
};

use ployz::deploy::{
    DeployIntent, DeployOperation, DeployPreview, DeploySnapshot, PlanOptions,
    ReplacementOperation, VolumeSnapshot, preview_deploy,
};
use ployz_core::config::{self, lower_placement};
use ployz_core::{
    BlockedKind, ContainerId, ContainerKind, ContainerObservation, ContainerObservationParts,
    ContainerPath, ContainerResources, ContainerRuntimeObservation, ContainerUpdateOperation,
    CopyRole, DockerVolume, DockerVolumeId, DockerVolumeName, DockerVolumeStorageObservation,
    HealthObservation, Machine, MachineId, MachineName, MachineObservation, MachinePath,
    MachineStorageObservation, MembershipObservation, Namespace, PullPolicy, Region,
    RequestedServiceSpec, ResolvedServiceSpec, ResolvedUpdateConfig, RestartPolicy,
    ServiceContainerSpec, ServiceId, ServiceMode, ServiceMount, ServiceName, ServiceVolume,
    ServiceVolumeReference, SpecChange, UpdateConfig, UpdateOrder, WireGuardPublicKey,
    compare_specs, labels,
};
use stateright::{Model, Property};

use crate::harness::Narrated;

const SERVERS: usize = 3;
const SERVER_NAMES: [&str; SERVERS] = ["one", "two", "three"];
const SERVICE_NAMES: [&str; 2] = ["api", "worker"];
/// The Server that holds the Volume's writer copy, in the scenarios with a Volume.
const HOLDER: usize = 0;
const VOLUME: &str = "data";

fn service_name(service: usize) -> &'static str {
    SERVICE_NAMES
        .get(service)
        .copied()
        .expect("a modelled Service")
}

fn server_name(server: usize) -> &'static str {
    SERVER_NAMES
        .get(server)
        .copied()
        .expect("a modelled Server")
}

/// A Server's `region` label: one of the two regions, or none.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Label {
    None,
    Fsn,
    Syd,
}

impl Label {
    const REGIONS: [Self; 2] = [Self::Fsn, Self::Syd];

    fn region(self) -> Option<Region> {
        match self {
            Self::None => None,
            Self::Fsn => Some(Region::parse("fsn").expect("a region")),
            Self::Syd => Some(Region::parse("syd").expect("a region")),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::None => "no region",
            Self::Fsn => "fsn",
            Self::Syd => "syd",
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) struct Server {
    present: bool,
    label: Label,
    /// `accepts_services`: whether it takes new starts. What runs there keeps running.
    accepts: bool,
}

/// A Service's `replicas`: one count, or a count in each region (`Replicas::PerRegion`
/// in ployz-core config/service.rs). A region at zero is not in the counts.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Replicas {
    Count(u8),
    By { fsn: u8, syd: u8 },
}

impl Replicas {
    /// It as a Service's settings hold it.
    fn authored(self) -> config::Replicas {
        match self {
            Self::Count(count) => config::Replicas::Count(count),
            Self::By { .. } => config::Replicas::PerRegion(
                config::RegionReplicas::new(Label::REGIONS.into_iter().filter_map(|label| {
                    Some((label.region()?, NonZeroU8::new(self.count_in(label)?)?))
                }))
                .expect("counts by region name a region"),
            ),
        }
    }

    const fn total(self) -> u8 {
        match self {
            Self::Count(count) => count,
            Self::By { fsn, syd } => fsn + syd,
        }
    }

    /// The count in `label`'s region, when the counts are by region.
    const fn count_in(self, label: Label) -> Option<u8> {
        match (self, label) {
            (Self::Count(_), _) | (_, Label::None) => None,
            (Self::By { fsn, .. }, Label::Fsn) => Some(fsn),
            (Self::By { syd, .. }, Label::Syd) => Some(syd),
        }
    }

    /// Whether going from `before` to this lowers the total or any region's count.
    fn shrinks_from(self, before: Self) -> bool {
        self.total() < before.total()
            || Label::REGIONS.into_iter().any(|label| {
                matches!(
                    (before.count_in(label), self.count_in(label)),
                    (Some(was), Some(now)) if now < was
                )
            })
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) struct ServiceConfig {
    replicas: Replicas,
    version: u8,
}

/// The Saved config a Deploy ships: the Environment's region and each Service's settings.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct Config {
    region: Label,
    services: Vec<ServiceConfig>,
}

impl Config {
    fn service(&self, service: usize) -> ServiceConfig {
        self.services
            .get(service)
            .copied()
            .expect("a modelled Service")
    }

    fn service_mut(&mut self, service: usize) -> &mut ServiceConfig {
        self.services.get_mut(service).expect("a modelled Service")
    }

    /// The regions the config keeps `service` in, `None` for any region:
    /// `configured_regions` in planning.rs, from what lowering puts in the spec.
    fn regions(&self, service: usize) -> Option<BTreeSet<Label>> {
        match self.service(service).replicas {
            Replicas::Count(_) => (self.region != Label::None).then(|| [self.region].into()),
            Replicas::By { fsn, syd } => Some(
                [(Label::Fsn, fsn), (Label::Syd, syd)]
                    .into_iter()
                    .filter(|(_, count)| *count > 0)
                    .map(|(label, _)| label)
                    .collect(),
            ),
        }
    }

    /// Whether `a` and `b` can ever run in one region, by config alone.
    fn meet(&self, a: usize, b: usize) -> bool {
        match (self.regions(a), self.regions(b)) {
            (Some(a), Some(b)) => !a.is_disjoint(&b),
            _ => true,
        }
    }
}

/// What a Container was made from: its version and the placement lowering gave it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Made {
    version: u8,
    replicas: Replicas,
    region: Label,
}

/// One running Container of a Service.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Container {
    server: u8,
    service: u8,
    made: Made,
}

/// What changed since the last Deploy.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub(crate) struct Since {
    labels: bool,
    added: bool,
    removed: bool,
    accepts: bool,
    config: bool,
    /// A count went down.
    shrank: bool,
}

impl Since {
    const fn nothing(self) -> bool {
        !self.labels && !self.added && !self.removed && !self.accepts && !self.config
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct State {
    servers: [Server; SERVERS],
    config: Config,
    containers: Vec<Container>,
    /// The Saved config `live` names: what the Servers run, as the Store reads it.
    live: Option<Config>,
    since: Since,
    /// The Services the last Deploy refused, cleared by a config change.
    refused: BTreeSet<u8>,
    changes_left: u8,
    /// The properties the last Deploy broke, and the outcomes it reached, by bit.
    broke: u32,
    reached: u32,
}

impl State {
    fn server(&self, server: usize) -> Server {
        self.servers
            .get(server)
            .copied()
            .expect("a modelled Server")
    }

    fn server_mut(&mut self, server: usize) -> &mut Server {
        self.servers.get_mut(server).expect("a modelled Server")
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) enum Action {
    Deploy,
    Relabel(u8, Label),
    Add(u8),
    Remove(u8),
    ToggleAccepts(u8),
    Counts(u8, Replicas),
    Region(Label),
    Bump(u8),
}

/// Which Services exist and what they mount.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) enum Scenario {
    /// `api` alone, no Volume.
    Plain,
    /// `api` mounts `data`, whose writer is on Server one.
    Volume,
    /// `api` and `worker` both mount `data`, whose writer is on Server one.
    Shared,
}

impl Scenario {
    const fn services(self) -> usize {
        match self {
            Self::Plain | Self::Volume => 1,
            Self::Shared => 2,
        }
    }

    const fn has_volume(self) -> bool {
        !matches!(self, Self::Plain)
    }
}

/// A change to the model's copy of the Store, to show a property fails without the rule
/// it guards.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Broken {
    /// `all_waiting` as it was before R5's fix (2e3364db24): a Deployment reads nothing
    /// applied only when a node was never attempted, so a Service left waiting in some
    /// regions with an Unchanged node moves Live.
    pub(crate) all_waiting_needs_unattempted: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct RegionPlacement {
    pub(crate) scenario: Scenario,
    pub(crate) broken: Broken,
    /// How many changes (anything but a Deploy) a path may make.
    pub(crate) changes: u8,
    pub(crate) start: Start,
}

/// The world a path starts from, before its first Deploy.
#[derive(Clone, Debug)]
pub(crate) struct Start {
    pub(crate) servers: [Server; SERVERS],
    pub(crate) config: Config,
}

pub(crate) const LABELS_KEEP: &str = "with no config change and no Server gone, a Deploy removes a running replica only past the counts, from a Server that takes no new starts, or as a spare whose waiting region starts its own";
pub(crate) const SURPLUS_GOES: &str =
    "after a Deploy, a Service it does not refuse runs no more replicas than its counts add up to";
pub(crate) const MET_STARTS_NOTHING: &str = "while another region waits, a region whose count is met on Servers that take new starts starts nothing new when only labels or new Servers changed";
pub(crate) const WAIT_NAMES_SHORT: &str = "a Wait names only regions still short of their count";
pub(crate) const SHORT_IS_NAMED: &str = "after a Deploy, a Service short of its counts waits, and a Wait about regions names each region still short";
pub(crate) const WAITS_WHILE_SHORT: &str =
    "a Service waits only while a region it names is short of its count";
pub(crate) const NOTHING_APPLIED: &str = "a Deploy that changed nothing on the Servers and left a Wait reads nothing applied and does not move Live";
pub(crate) const WAITS_ON_THE_WORLD: &str =
    "a Service waits only while a Server or label it needs is missing";
pub(crate) const REFUSAL_STICKS: &str = "a Refusal clears only with a config change";
pub(crate) const REFUSED_ONLY_APART: &str = "a Service is refused only when it shares the Volume with one whose configured regions can never meet its own";
pub(crate) const ON_THE_WRITER: &str =
    "a Service with a Volume runs only on the Volume writer's Server";
pub(crate) const NEVER_FEWER: &str = "a Deploy never leaves a Service fewer running replicas, unless a count went down or a Server went away";
pub(crate) const OLD_COUNTED: &str =
    "a Deploy that leaves replicas on the old version says how many";
pub(crate) const REPEAT_IS_NOOP: &str =
    "a Deploy repeated with nothing changed changes nothing on the Servers";

pub(crate) const PROPERTIES: [&str; 14] = [
    LABELS_KEEP,
    SURPLUS_GOES,
    MET_STARTS_NOTHING,
    WAIT_NAMES_SHORT,
    SHORT_IS_NAMED,
    WAITS_WHILE_SHORT,
    NOTHING_APPLIED,
    WAITS_ON_THE_WORLD,
    REFUSAL_STICKS,
    REFUSED_ONLY_APART,
    ON_THE_WRITER,
    NEVER_FEWER,
    OLD_COUNTED,
    REPEAT_IS_NOOP,
];

pub(crate) const PARTIAL_WAIT: &str = "a Service starts in one region while another waits";
pub(crate) const WHOLE_WAIT: &str = "a Service waits whole";
pub(crate) const REFUSAL: &str = "a Service is refused";
pub(crate) const SPARE_KEPT: &str =
    "a replica outside every counted region is kept for one that waits";
pub(crate) const HELD_BACK: &str = "a Deploy reads nothing applied and Live stays";
pub(crate) const PLAN_FAILS: &str = "the planner refuses the whole Deploy";
pub(crate) const OUTDATED_SPARE: &str = "a Wait counts a replica on the old version";

pub(crate) const REACHED: [&str; 7] = [
    PARTIAL_WAIT,
    WHOLE_WAIT,
    REFUSAL,
    SPARE_KEPT,
    HELD_BACK,
    PLAN_FAILS,
    OUTDATED_SPARE,
];

fn bit(names: &[&str], name: &str) -> u32 {
    1 << names
        .iter()
        .position(|candidate| *candidate == name)
        .expect("a named property")
}

impl RegionPlacement {
    pub(crate) fn new(scenario: Scenario, changes: u8) -> Self {
        let server = |label| Server {
            present: true,
            label,
            accepts: true,
        };
        let service = |replicas| ServiceConfig {
            replicas,
            version: 1,
        };
        let config = match scenario {
            Scenario::Plain => Config {
                region: Label::None,
                services: vec![service(Replicas::By { fsn: 1, syd: 1 })],
            },
            Scenario::Volume => Config {
                region: Label::Fsn,
                services: vec![service(Replicas::Count(1))],
            },
            Scenario::Shared => Config {
                region: Label::Fsn,
                services: vec![service(Replicas::Count(1)), service(Replicas::Count(1))],
            },
        };
        Self {
            scenario,
            broken: Broken::default(),
            changes,
            start: Start {
                servers: [server(Label::Fsn), server(Label::Syd), server(Label::None)],
                config,
            },
        }
    }

    fn counts_menu(&self) -> &'static [Replicas] {
        // A Service with a Volume counts replicas in one region: the Store refuses more.
        if self.scenario.has_volume() {
            &[
                Replicas::Count(1),
                Replicas::Count(2),
                Replicas::By { fsn: 1, syd: 0 },
                Replicas::By { fsn: 0, syd: 1 },
            ]
        } else {
            &[
                Replicas::Count(1),
                Replicas::Count(2),
                Replicas::By { fsn: 1, syd: 0 },
                Replicas::By { fsn: 0, syd: 1 },
                Replicas::By { fsn: 1, syd: 1 },
                Replicas::By { fsn: 2, syd: 1 },
                Replicas::By { fsn: 1, syd: 2 },
            ]
        }
    }

    /// Whether `service` mounts `data`: every Service does in the scenarios with a Volume.
    fn mounts_volume(&self, _service: usize) -> bool {
        self.scenario.has_volume()
    }

    fn namespace() -> Namespace {
        Namespace::parse("app").expect("a Namespace")
    }

    /// The spec lowering gives `service` under `config`'s region and counts, placed by
    /// [`lower_placement`].
    fn requested(&self, service: usize, made: Made) -> RequestedServiceSpec {
        let name = service_name(service);
        let (placement, regions) =
            lower_placement(&made.replicas.authored(), made.region.region().as_ref());
        let mut spec = RequestedServiceSpec {
            name: ServiceName::parse(name).expect("a Service name"),
            mode: ServiceMode::Replicated {
                replicas: NonZeroU32::new(u32::from(made.replicas.total()).max(1))
                    .expect("at least one replica"),
            },
            container: ServiceContainerSpec {
                shared_writes: Default::default(),
                image: format!("ghcr.io/getployz/{name}:{}", made.version),
                command: Vec::new(),
                entrypoint: Vec::new(),
                environment: BTreeMap::default(),
                sealed_identity: None,
                labels: ployz_core::ContainerLabels::default(),
                hostname: None,
                extra_hosts: Vec::new(),
                cap_add: Vec::new(),
                cap_drop: Vec::new(),
                healthcheck: None,
                pull_policy: PullPolicy::Missing,
                init: None,
                user: None,
                working_directory: None,
                tty: false,
                open_stdin: false,
                privileged: false,
                pid_mode: None,
                resources: ContainerResources::default(),
                stop_timeout_secs: None,
                stop_signal: None,
                sysctls: BTreeMap::default(),
                network: None,
                restart: RestartPolicy::default(),
            },
            placement,
            regions,
            ports: Vec::new(),
            mount_graph: ployz_core::ServiceMountGraph::default(),
            pre_deploy: None,
            update: UpdateConfig::default(),
        };
        if self.mounts_volume(service) {
            mount_data(&mut spec);
        }
        spec
    }

    fn made_now(config: &Config, service: usize) -> Made {
        let settings = config.service(service);
        Made {
            version: settings.version,
            replicas: settings.replicas,
            region: match settings.replicas {
                Replicas::Count(_) => config.region,
                Replicas::By { .. } => Label::None,
            },
        }
    }

    /// The Service and the [`Made`] of the spec an operation gives a Container, so the world
    /// runs what the planner sent, not what the config asks for.
    fn made_of(&self, spec: &ResolvedServiceSpec) -> (usize, Made) {
        let service = service_of(&spec.name);
        let version = spec
            .container
            .image
            .rsplit_once(':')
            .and_then(|(_, version)| version.parse().ok())
            .expect("an image the model names");
        let count = |label: Label| {
            label
                .region()
                .and_then(|region| spec.regions.get(&region))
                .map_or(0, |count| {
                    u8::try_from(count.get()).expect("a modelled count")
                })
        };
        let replicas = if spec.regions.is_empty() {
            let ServiceMode::Replicated { replicas } = &spec.mode else {
                panic!("a replicated Service")
            };
            Replicas::Count(u8::try_from(replicas.get()).expect("a modelled count"))
        } else {
            Replicas::By {
                fsn: count(Label::Fsn),
                syd: count(Label::Syd),
            }
        };
        let region = Label::REGIONS
            .into_iter()
            .find(|label| {
                label
                    .region()
                    .is_some_and(|region| spec.placement.constraints.contains(&region.constraint()))
            })
            .unwrap_or(Label::None);
        let made = Made {
            version,
            replicas,
            region,
        };
        assert!(
            compare_specs(spec, &scoped(&self.requested(service, made))) == SpecChange::UpToDate,
            "the planner sent {} a spec the model holds as {made:?}",
            spec.name
        );
        (service, made)
    }

    fn requested_now(&self, config: &Config, service: usize) -> RequestedServiceSpec {
        self.requested(service, Self::made_now(config, service))
    }

    fn observation(&self, id: usize, container: Container) -> ContainerObservation {
        let service = usize::from(container.service);
        let requested = scoped(&self.requested(service, container.made));
        ContainerObservation::try_from(ContainerObservationParts {
            rotation: None,
            r#gen: None,
            container_id: container_id(id),
            display_name: format!("{}-{id}", service_name(service)),
            created_at_unix_nanos: 0,
            machine_id: machine_id(usize::from(container.server)),
            namespace: Self::namespace(),
            kind: ContainerKind::ServiceContainer,
            runtime: ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            effective_healthcheck: None,
            resolved_spec: requested
                .to_resolved(
                    service_id(service),
                    ResolvedUpdateConfig {
                        order: UpdateOrder::StartFirst,
                        monitor_millis: requested.update.monitor_millis,
                    },
                )
                .expect("a scoped spec"),
            address: None,
            labels: BTreeMap::default(),
            stopped: false,
            run: None,
            log_driver: None,
            admission: None,
            replaced_by: None,
        })
        .expect("a Container observation")
    }

    /// The world as a runner's Deploy observes it.
    fn snapshot(&self, state: &State) -> DeploySnapshot {
        let present = (0..SERVERS).filter(|server| state.server(*server).present);
        let machines = present
            .clone()
            .map(|server| {
                let settings = state.server(server);
                let mut observation = MachineObservation::new(
                    Machine {
                        id: machine_id(server),
                        name: MachineName::parse(server_name(server)).expect("a Server name"),
                        labels: settings
                            .label
                            .region()
                            .map(|region| {
                                (
                                    Region::LABEL.parse().expect("a label key"),
                                    region.as_str().parse().expect("a label value"),
                                )
                            })
                            .into_iter()
                            .collect(),
                        accepts_builds: true,
                        accepts_services: settings.accepts,
                        accepts_ingress: true,
                        subnet: format!("10.210.{}.0/24", server + 1)
                            .parse()
                            .expect("a subnet"),
                        public_key: WireGuardPublicKey([u8::try_from(server + 1).unwrap_or(1); 32]),
                        public_ip: None,
                        advertised_endpoints: Vec::new(),
                        runtime: ployz_core::MachineRuntime {
                            capabilities: [
                                ployz_core::UPDATE_CONTAINER_LIMITS_CAPABILITY.to_owned(),
                                ployz_core::UPDATE_CONTAINER_PLACEMENT_CAPABILITY.to_owned(),
                            ]
                            .into(),
                            ..ployz_core::MachineRuntime::default()
                        },
                        join_approval: None,
                        build_concurrency: None,
                    },
                    MembershipObservation::Up,
                );
                if self.scenario.has_volume() {
                    observation.storage = Some(MachineStorageObservation::Ready);
                }
                observation
            })
            .collect();
        let mut snapshot = DeploySnapshot {
            machines,
            containers: state
                .containers
                .iter()
                .enumerate()
                .map(|(id, container)| self.observation(id, *container))
                .collect(),
            ..DeploySnapshot::default()
        };
        if self.scenario.has_volume() {
            snapshot.storage_capacity = present
                .map(|server| {
                    let mut copies = BTreeMap::new();
                    if server == HOLDER {
                        copies.insert(
                            data_name(),
                            ployz_core::ProvisionedCopy::new(
                                ployz_core::CopyPlace::of(CopyRole::Writer, None),
                                ployz_core::ProvisionedVolumeMaximumBytes::new(
                                    NonZeroU64::new(VOLUME_BYTES).expect("a size"),
                                ),
                                0,
                            ),
                        );
                    }
                    (
                        machine_id(server),
                        Ok(ployz_core::StorageCapacity {
                            backing: ployz_core::StorageBacking::Fixed {
                                pool_size_bytes: 1024 * ployz_core::STORAGE_GIB,
                            },
                            unmanaged_used_bytes: 0,
                            slot_bound_bytes: 0,
                            volumes: BTreeMap::new(),
                            copies,
                        }),
                    )
                })
                .collect();
            snapshot.volume_snapshot = VolumeSnapshot::try_from_observations(
                state
                    .server(HOLDER)
                    .present
                    .then(|| data_row(HOLDER))
                    .into_iter()
                    .collect::<Vec<_>>(),
            )
            .expect("a Volume Snapshot");
        }
        snapshot
    }

    /// What one Deploy plans on `state`.
    pub(crate) fn plan(&self, state: &State) -> Result<DeployPreview, ployz::deploy::PlanError> {
        let specs: Vec<_> = (0..self.scenario.services())
            .map(|service| self.requested_now(&state.config, service))
            .collect();
        let intent = DeployIntent::apply_all(
            Self::namespace(),
            &specs,
            PlanOptions {
                block_unmountable: true,
                ..PlanOptions::default()
            },
        );
        preview_deploy(&intent, &self.snapshot(state))
    }

    /// `state`'s Containers once `preview`'s operations ran, each one to completion.
    fn apply(&self, state: &State, preview: &DeployPreview) -> (Vec<Container>, Effects) {
        let mut containers: Vec<Option<Container>> =
            state.containers.iter().copied().map(Some).collect();
        let mut effects = Effects::default();
        let index_of = |id: &ContainerId| {
            (0..MOST_CONTAINERS)
                .find(|index| container_id(*index) == *id)
                .expect("a Container the model made")
        };
        let server_of = |machine: &MachineId| {
            (0..SERVERS)
                .find(|server| machine_id(*server) == *machine)
                .expect("a modelled Server")
        };
        for row in &preview.operations {
            match &row.operation {
                DeployOperation::RunContainer {
                    machine_id, spec, ..
                } => {
                    let (service, made) = self.made_of(spec);
                    effects.started.push((service, server_of(machine_id)));
                    containers.push(Some(Container {
                        server: u8::try_from(server_of(machine_id)).unwrap_or(0),
                        service: u8::try_from(service).unwrap_or(0),
                        made,
                    }));
                }
                DeployOperation::ReplaceContainer(ReplacementOperation {
                    machine_id,
                    old_container_id,
                    spec,
                    ..
                }) => {
                    let (service, made) = self.made_of(spec);
                    let old = index_of(old_container_id);
                    *containers.get_mut(old).expect("a running Container") = None;
                    effects.replaced.push((service, server_of(machine_id)));
                    containers.push(Some(Container {
                        server: u8::try_from(server_of(machine_id)).unwrap_or(0),
                        service: u8::try_from(service).unwrap_or(0),
                        made,
                    }));
                }
                DeployOperation::UpdateContainer(ContainerUpdateOperation {
                    container_id,
                    spec,
                    ..
                }) => {
                    let index = index_of(container_id);
                    let container = containers
                        .get_mut(index)
                        .and_then(Option::as_mut)
                        .expect("a running Container");
                    container.made = self.made_of(spec).1;
                    effects.updated += 1;
                }
                DeployOperation::RemoveContainer { container_id, .. } => {
                    let index = index_of(container_id);
                    let container = containers
                        .get_mut(index)
                        .and_then(Option::take)
                        .expect("a running Container");
                    effects.removed.push((
                        usize::from(container.service),
                        usize::from(container.server),
                    ));
                }
                DeployOperation::StopContainer { .. } => {
                    panic!("no Container in the model publishes a host port")
                }
                DeployOperation::PrepareVolumes { .. }
                | DeployOperation::WaitHealthy { .. }
                | DeployOperation::StopHook { .. }
                | DeployOperation::RunHook { .. }
                | DeployOperation::RemoveVolume { .. } => {}
            }
        }
        let mut after: Vec<Container> = containers.into_iter().flatten().collect();
        after.sort();
        (after, effects)
    }

    /// Whether `service` has every Server and label it needs in `state`: a present Server
    /// that takes new starts in each region its config keeps it in, and for a Service with
    /// the Volume, `one` taking new starts in its one configured region.
    fn can_run(&self, state: &State, service: usize) -> bool {
        let servers = &state.servers;
        let accepting = |label: Option<Label>| {
            servers.iter().any(|server| {
                server.present && server.accepts && label.is_none_or(|label| server.label == label)
            })
        };
        let regions = state.config.regions(service);
        let placed = match &regions {
            None => accepting(None),
            Some(regions) => regions.iter().all(|label| accepting(Some(*label))),
        };
        if !self.mounts_volume(service) {
            return placed;
        }
        let holder = servers[HOLDER];
        holder.present
            && holder.accepts
            && regions.is_none_or(|regions| regions.len() == 1 && regions.contains(&holder.label))
    }

    fn deploy(&self, state: &State) -> State {
        let mut next = state.clone();
        let planned = self.plan(state);
        let mut broke = 0;
        let mut reached = 0;
        let before = Counts::of(self, state, &state.containers);
        match &planned {
            Err(_) => {
                reached |= bit(&REACHED, PLAN_FAILS);
                if (0..self.scenario.services()).all(|service| self.can_run(state, service)) {
                    broke |= bit(&PROPERTIES, WAITS_ON_THE_WORLD);
                }
                next.refused.clear();
            }
            Ok(preview) => {
                let (after, effects) = self.apply(state, preview);
                next.containers = after;
                let verdict = Verdict {
                    model: self,
                    state,
                    next: &next,
                    preview,
                    effects: &effects,
                    before: &before,
                    after: &Counts::of(self, &next, &next.containers),
                };
                (broke, reached) = verdict.judge();
                let live = verdict.live();
                if live.is_none() && verdict.held_back() {
                    reached |= bit(&REACHED, HELD_BACK);
                }
                let refused = |service: usize| {
                    preview.waits().any(|wait| {
                        wait.service.as_str() == service_name(service)
                            && wait.kind == BlockedKind::Refusal
                    })
                };
                for service in 0..self.scenario.services() {
                    if state.refused.contains(&(service as u8)) && !refused(service) {
                        broke |= bit(&PROPERTIES, REFUSAL_STICKS);
                    }
                }
                next.refused = (0..self.scenario.services())
                    .filter(|service| refused(*service))
                    .map(|service| u8::try_from(service).unwrap_or(0))
                    .collect();
                if live.is_some() {
                    next.live = live;
                }
            }
        }
        next.since = Since::default();
        next.broke = broke;
        next.reached = reached;
        next
    }
}

const VOLUME_BYTES: u64 = 1_073_741_824;

fn service_of(name: &ServiceName) -> usize {
    SERVICE_NAMES
        .iter()
        .position(|candidate| *candidate == name.as_str())
        .expect("a modelled Service")
}
/// More Containers than any Deploy in the model leaves or makes.
const MOST_CONTAINERS: usize = 32;

fn machine_id(server: usize) -> MachineId {
    MachineId::parse(format!("{:032x}", server + 1)).expect("a Machine id")
}

fn container_id(id: usize) -> ContainerId {
    ContainerId::parse(format!("{:064x}", id + 1)).expect("a Container id")
}

fn service_id(service: usize) -> ServiceId {
    ServiceId::parse(format!("{:032x}", service + 0xa)).expect("a Service id")
}

fn data_name() -> DockerVolumeName {
    RegionPlacement::namespace()
        .volume_name(&DockerVolumeName::parse(VOLUME).expect("a Volume name"))
}

/// The writer's Docker Volume row on `server`, as `provisioned_row` in the deploy_plan
/// tests builds it.
fn data_row(server: usize) -> DockerVolume {
    DockerVolume {
        id: DockerVolumeId {
            machine_id: machine_id(server),
            name: data_name(),
        },
        options: BTreeMap::from([("size".into(), format!("{VOLUME_BYTES}b"))]),
        labels: BTreeMap::from([
            (labels::MANAGED.to_owned(), String::new()),
            (labels::NAMESPACE.to_owned(), "app".to_owned()),
        ]),
        storage: DockerVolumeStorageObservation::Provisioned {
            mountpoint: MachinePath::parse(format!("/var/lib/ployz-volumes/app_{VOLUME}"))
                .expect("a mountpoint"),
            bound_bytes: NonZeroU64::new(VOLUME_BYTES).expect("a size"),
            used_bytes: 0,
            role: None,
            slot_role: None,
        },
    }
}

/// Mount the provisioned Volume `data` at `/data`.
fn mount_data(spec: &mut RequestedServiceSpec) {
    let reference = ServiceVolumeReference::parse(VOLUME).expect("a reference");
    let source = ployz_core::RawVolumeSource::Provisioned {
        name: DockerVolumeName::parse(VOLUME).expect("a Volume name"),
        maximum_bytes: ployz_core::ProvisionedVolumeMaximumBytes::new(
            NonZeroU64::new(VOLUME_BYTES).expect("a size"),
        ),
        labels: BTreeMap::default(),
        birth: None,
    }
    .admit()
    .expect("a Volume declaration");
    let volumes = vec![ServiceVolume {
        reference: reference.clone(),
        source,
    }];
    let mounts = vec![ServiceMount {
        volume: reference,
        target: ContainerPath::parse(format!("/{VOLUME}")).expect("a path"),
        read_only: false,
        no_copy: false,
        subpath: None,
    }];
    spec.set_volume_graph(
        ployz_core::ServiceVolumeGraph::parse(volumes, mounts).expect("a Volume graph"),
    )
    .expect("a mount");
}

/// `spec` with its Volumes scoped to the Namespace, as a Container's spec records them.
fn scoped(spec: &RequestedServiceSpec) -> RequestedServiceSpec {
    let mut spec = spec.clone();
    let mut volumes = spec.volume_graph().volumes().to_vec();
    let mounts = spec.volume_graph().mounts().to_vec();
    for volume in &mut volumes {
        volume
            .source
            .scope_to_namespace(&RegionPlacement::namespace());
    }
    spec.set_volume_graph(
        ployz_core::ServiceVolumeGraph::parse(volumes, mounts).expect("a Volume graph"),
    )
    .expect("a mount");
    spec
}

/// What a Deploy's operations did, by Service.
#[derive(Default)]
struct Effects {
    /// Each new Container's Service and Server.
    started: Vec<(usize, usize)>,
    /// Each removed Container's Service and Server.
    removed: Vec<(usize, usize)>,
    /// Each replacement's Service and Server: one Container stopped and a new one started.
    replaced: Vec<(usize, usize)>,
    /// How many Containers changed in place.
    updated: usize,
}

impl Effects {
    fn touched(&self) -> bool {
        !self.started.is_empty()
            || !self.removed.is_empty()
            || !self.replaced.is_empty()
            || self.updated > 0
    }
}

/// One Service's running replicas: in all, on each region's Servers (by their labels now),
/// on those that take new starts, and how many run the config's version and placement.
#[derive(Clone, Default)]
struct Tally {
    running: usize,
    outdated: usize,
    in_region: BTreeMap<Label, usize>,
    accepting_in_region: BTreeMap<Label, usize>,
    settled_in_region: BTreeMap<Label, usize>,
}

/// Each Service's [`Tally`].
struct Counts(BTreeMap<usize, Tally>);

impl Counts {
    fn of(model: &RegionPlacement, state: &State, containers: &[Container]) -> Self {
        let mut counts = BTreeMap::<usize, Tally>::new();
        for (id, container) in containers.iter().enumerate() {
            let service = usize::from(container.service);
            let server = state.server(usize::from(container.server));
            let label = server.label;
            let tally = counts.entry(service).or_default();
            tally.running += 1;
            *tally.in_region.entry(label).or_default() += 1;
            if server.accepts {
                *tally.accepting_in_region.entry(label).or_default() += 1;
            }
            let requested = scoped(&model.requested_now(&state.config, service));
            let observation = model.observation(id, *container);
            match compare_specs(&observation.resolved_spec, &requested) {
                SpecChange::UpToDate | SpecChange::NeedsPlacementUpdate => {
                    *tally.settled_in_region.entry(label).or_default() += 1;
                }
                SpecChange::NeedsUpdate | SpecChange::NeedsRecreate => tally.outdated += 1,
            }
        }
        Self(counts)
    }

    fn tally(&self, service: usize) -> Tally {
        self.0.get(&service).cloned().unwrap_or_default()
    }

    fn running(&self, service: usize) -> usize {
        self.tally(service).running
    }

    fn outdated(&self, service: usize) -> usize {
        self.tally(service).outdated
    }

    fn in_region(&self, service: usize, label: Label) -> usize {
        self.tally(service)
            .in_region
            .get(&label)
            .copied()
            .unwrap_or(0)
    }

    /// Replicas on `label`'s Servers that take new starts, or on any such Server for `None`.
    fn accepting_in(&self, service: usize, label: Option<Label>) -> usize {
        let tally = self.tally(service);
        match label {
            Some(label) => tally.accepting_in_region.get(&label).copied().unwrap_or(0),
            None => tally.accepting_in_region.values().sum(),
        }
    }

    fn settled(&self, service: usize) -> usize {
        self.tally(service).settled_in_region.values().sum()
    }

    fn settled_in_region(&self, service: usize, label: Label) -> usize {
        self.tally(service)
            .settled_in_region
            .get(&label)
            .copied()
            .unwrap_or(0)
    }
}

/// Where a Service is short of its counts after a Deploy.
enum Short {
    /// One count with no Environment region: short or not, in no region in particular.
    Anywhere(bool),
    /// The counted regions short of their count.
    In(BTreeSet<Label>),
}

impl Short {
    fn any(&self) -> bool {
        match self {
            Self::Anywhere(short) => *short,
            Self::In(regions) => !regions.is_empty(),
        }
    }

    /// Whether a Service that `waits` with `named` regions is told about each shortfall.
    fn is_named(&self, named: &BTreeSet<Label>, waits: bool) -> bool {
        match self {
            Self::Anywhere(short) => !short || waits,
            Self::In(regions) => regions.is_subset(named),
        }
    }
}

/// One Deploy judged against every property, from the world before and after it.
struct Verdict<'a> {
    model: &'a RegionPlacement,
    state: &'a State,
    next: &'a State,
    preview: &'a DeployPreview,
    effects: &'a Effects,
    before: &'a Counts,
    after: &'a Counts,
}

impl Verdict<'_> {
    fn waits(&self, service: usize) -> Option<ployz_core::Wait<'_>> {
        self.preview
            .waits()
            .find(|wait| wait.service.as_str() == service_name(service))
    }

    fn kind(&self, service: usize) -> Option<BlockedKind> {
        self.waits(service).map(|wait| wait.kind)
    }

    /// Whether the Store's node for `service` reads Deployed: the preview planned work for
    /// it and, the executor never failing, all of it completed (`node_outcomes`).
    fn advanced(&self, service: usize) -> bool {
        !self
            .preview
            .blocked
            .iter()
            .any(|blocked| blocked.service.as_str() == service_name(service))
            && self.preview.operations.iter().any(|row| {
                row.service_name().map(ServiceName::as_str) == Some(service_name(service))
            })
    }

    /// The Deployment ends Applied unless it refused a Service.
    fn applied(&self) -> bool {
        !self
            .preview
            .waits()
            .any(|wait| wait.kind == BlockedKind::Refusal)
    }

    /// `all_waiting`: Applied, with a Service left waiting, and no Service advanced.
    fn all_waiting(&self) -> bool {
        let services = self.model.scenario.services();
        let left = if self.model.broken.all_waiting_needs_unattempted {
            !self.preview.blocked.is_empty()
        } else {
            self.preview.waits().next().is_some()
        };
        self.applied() && left && !(0..services).any(|service| self.advanced(service))
    }

    /// The config `live` names once this Deploy is recorded, when it moves.
    fn live(&self) -> Option<Config> {
        (self.applied() && !self.all_waiting()).then(|| self.state.config.clone())
    }

    fn held_back(&self) -> bool {
        self.all_waiting()
    }

    /// `service`'s count in `label`'s region: its count there when the counts are by
    /// region, its one count in the Environment's region, and none elsewhere.
    fn count_in(&self, service: usize, label: Label) -> Option<usize> {
        let config = &self.state.config;
        match config.service(service).replicas {
            Replicas::Count(count) => {
                (label != Label::None && label == config.region).then_some(usize::from(count))
            }
            replicas @ Replicas::By { .. } => replicas.count_in(label).map(usize::from),
        }
    }

    /// Whether `service` is short of its count in `label`'s region after the Deploy, `None`
    /// when no count covers that region. One count with no Environment region covers
    /// every region, and is short only when the Service is.
    fn short_in(&self, service: usize, label: Label) -> Option<bool> {
        let config = &self.state.config;
        match (config.service(service).replicas, config.region) {
            (Replicas::Count(count), Label::None) => {
                Some(self.after.settled(service) < usize::from(count))
            }
            _ => self
                .count_in(service, label)
                .map(|count| self.after.settled_in_region(service, label) < count),
        }
    }

    /// Where `service` is short of its counts after the Deploy, from the config and the
    /// world alone, whatever the planner said.
    fn short(&self, service: usize) -> Short {
        let config = &self.state.config;
        match (config.service(service).replicas, config.region) {
            (Replicas::Count(count), Label::None) => {
                Short::Anywhere(self.after.settled(service) < usize::from(count))
            }
            _ => Short::In(
                Label::REGIONS
                    .into_iter()
                    .filter(|label| self.short_in(service, *label) == Some(true))
                    .collect(),
            ),
        }
    }

    /// Whether `service`'s label is one its counts or the Environment's region keep it in.
    fn counted(&self, service: usize, label: Label) -> bool {
        self.state
            .config
            .regions(service)
            .is_none_or(|regions| regions.contains(&label))
    }

    /// Whether this Deploy removed a running replica of `service` that none of the
    /// documented reasons removes: it was past the counts; its Server takes no new starts
    /// (manage-servers.md, "until your next deploy moves it"); it moved, a replica starting
    /// in its region in the same Deploy (regions.md, "spreads ... as usual"); or it was a
    /// spare, outside the counted regions or past its own region's count, and a region
    /// short of its count starts one (regions.md, "removed once the waiting region starts
    /// its own").
    fn removes_needlessly(&self, service: usize) -> bool {
        let total = usize::from(self.state.config.service(service).replicas.total());
        let mut surplus = self.before.running(service).saturating_sub(total);
        let short_region_starts = self.effects.started.iter().any(|(s, server)| {
            let label = self.state.server(*server).label;
            *s == service
                && self
                    .count_in(service, label)
                    .is_some_and(|count| self.before.in_region(service, label) < count)
        });
        self.effects
            .removed
            .iter()
            .filter(|(s, _)| *s == service)
            .any(|(_, server)| {
                let settings = self.state.server(*server);
                if surplus > 0 {
                    surplus -= 1;
                    return false;
                }
                let label = settings.label;
                let spare = !self.counted(service, label)
                    || self
                        .count_in(service, label)
                        .is_some_and(|count| self.before.in_region(service, label) > count);
                let anywhere = self.state.config.regions(service).is_none();
                let moved = self.effects.started.iter().any(|(s, to)| {
                    *s == service && (anywhere || self.state.server(*to).label == label)
                });
                settings.accepts && !moved && !(spare && short_region_starts)
            })
    }

    /// Whether `wait` waits whole on the one reason that names what to fix in place of a
    /// region, and that reason is true here: `service` mounts the Volume and its writer's
    /// Server takes no services. Any other Wait must name each region still short.
    fn waits_on_the_writer(&self, service: usize, wait: &ployz_core::Wait<'_>) -> bool {
        let holder = self.state.server(HOLDER);
        wait.kind == BlockedKind::Wait
            && wait.partial.is_none()
            && wait.error.details.get("reason")
                == Some(&serde_json::json!("volume_server_takes_no_services"))
            && self.model.mounts_volume(service)
            && holder.present
            && !holder.accepts
    }

    fn judge(&self) -> (u32, u32) {
        let mut broke = 0;
        let mut reached = 0;
        let config = &self.state.config;
        let since = self.state.since;
        let services = self.model.scenario.services();
        let mut fail = |name| broke |= bit(&PROPERTIES, name);
        for service in 0..services {
            let replicas = config.service(service).replicas;
            let total = usize::from(replicas.total());
            let kind = self.kind(service);
            let refused = kind == Some(BlockedKind::Refusal);
            let before = self.before.running(service);
            let after = self.after.running(service);
            if !since.config && !since.removed {
                if after < before.min(total) {
                    fail(LABELS_KEEP);
                }
                if self.removes_needlessly(service) {
                    fail(LABELS_KEEP);
                }
            }
            if !refused && after > total {
                fail(SURPLUS_GOES);
            }
            let waits = self.kind(service) == Some(BlockedKind::Wait);
            if !since.config && !since.removed && !since.accepts && waits {
                let starts_in = |label: Option<Label>| {
                    self.effects.started.iter().any(|(s, server)| {
                        *s == service
                            && label.is_none_or(|label| self.state.server(*server).label == label)
                    })
                };
                match replicas {
                    Replicas::Count(count) => {
                        let label = (config.region != Label::None).then_some(config.region);
                        if self.before.accepting_in(service, label) >= usize::from(count)
                            && starts_in(label)
                        {
                            fail(MET_STARTS_NOTHING);
                        }
                    }
                    Replicas::By { .. } => {
                        for label in Label::REGIONS {
                            let count = usize::from(replicas.count_in(label).unwrap_or(0));
                            if count > 0
                                && self.before.accepting_in(service, Some(label)) >= count
                                && starts_in(Some(label))
                            {
                                fail(MET_STARTS_NOTHING);
                            }
                        }
                    }
                }
            }
            let short = self.short(service);
            let named = self
                .waits(service)
                .map(|wait| named(&wait))
                .unwrap_or_default();
            let whole = self
                .waits(service)
                .is_some_and(|wait| self.waits_on_the_writer(service, &wait));
            if !refused && !whole && !short.is_named(&named, waits) {
                fail(SHORT_IS_NAMED);
            }
            if let Some(wait) = self.waits(service) {
                let met = named
                    .iter()
                    .map(|label| self.short_in(service, *label))
                    .any(|short| short == Some(false));
                if named
                    .iter()
                    .any(|label| self.short_in(service, *label).is_none())
                    || met && short.any()
                {
                    fail(WAIT_NAMES_SHORT);
                } else if met {
                    fail(WAITS_WHILE_SHORT);
                }
                if let Some(partial) = wait.partial {
                    reached |= bit(&REACHED, PARTIAL_WAIT);
                    if partial.outdated() > 0 {
                        reached |= bit(&REACHED, OUTDATED_SPARE);
                    }
                } else if wait.kind == BlockedKind::Wait {
                    reached |= bit(&REACHED, WHOLE_WAIT);
                }
                match wait.kind {
                    BlockedKind::Wait => {
                        if self.model.can_run(self.state, service) {
                            fail(WAITS_ON_THE_WORLD);
                        }
                    }
                    BlockedKind::Refusal => {
                        reached |= bit(&REACHED, REFUSAL);
                        let apart = self.model.mounts_volume(service)
                            && (0..services).any(|other| {
                                other != service
                                    && self.model.mounts_volume(other)
                                    && !config.meet(service, other)
                            });
                        if !apart {
                            fail(REFUSED_ONLY_APART);
                        }
                    }
                }
                if wait.kind == BlockedKind::Wait
                    && let Replicas::By { .. } = replicas
                {
                    let counted: Vec<Label> = Label::REGIONS
                        .into_iter()
                        .filter(|label| replicas.count_in(*label).unwrap_or(0) > 0)
                        .collect();
                    let spare = self.next.containers.iter().any(|container| {
                        usize::from(container.service) == service
                            && !counted
                                .contains(&self.next.server(usize::from(container.server)).label)
                    });
                    if spare {
                        reached |= bit(&REACHED, SPARE_KEPT);
                    }
                }
            }
            if self.model.mounts_volume(service)
                && self.next.containers.iter().any(|container| {
                    usize::from(container.service) == service
                        && usize::from(container.server) != HOLDER
                })
            {
                fail(ON_THE_WRITER);
            }
            if !since.shrank && !since.removed && after < before.min(total) {
                fail(NEVER_FEWER);
            }
            if !refused {
                let outdated = self.after.outdated(service);
                let said = self
                    .waits(service)
                    .and_then(|wait| wait.partial)
                    .map_or(0, |partial| partial.outdated() as usize);
                if outdated != said {
                    fail(OLD_COUNTED);
                }
            }
        }
        let unchanged = !self.effects.touched();
        if unchanged && self.preview.waits().next().is_some() && self.live().is_some() {
            fail(NOTHING_APPLIED);
        }
        if since.nothing() && !unchanged {
            fail(REPEAT_IS_NOOP);
        }
        (broke, reached)
    }
}

impl Model for RegionPlacement {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        let empty = State {
            servers: self.start.servers,
            config: self.start.config.clone(),
            containers: Vec::new(),
            live: None,
            since: Since::default(),
            refused: BTreeSet::new(),
            changes_left: self.changes,
            broke: 0,
            reached: 0,
        };
        let mut first = self.deploy(&empty);
        first.broke = 0;
        first.reached = 0;
        vec![first]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        actions.push(Action::Deploy);
        if state.changes_left == 0 {
            return;
        }
        for (server, settings) in state.servers.iter().enumerate() {
            let index = u8::try_from(server).unwrap_or(0);
            if settings.present {
                for label in [Label::None, Label::Fsn, Label::Syd] {
                    if label != settings.label {
                        actions.push(Action::Relabel(index, label));
                    }
                }
                actions.push(Action::ToggleAccepts(index));
                if !(self.scenario.has_volume() && server == HOLDER) {
                    actions.push(Action::Remove(index));
                }
            } else {
                actions.push(Action::Add(index));
            }
        }
        for service in 0..self.scenario.services() {
            let index = u8::try_from(service).unwrap_or(0);
            for replicas in self.counts_menu() {
                if *replicas != state.config.service(service).replicas {
                    actions.push(Action::Counts(index, *replicas));
                }
            }
            if state.config.service(service).version == 1 {
                actions.push(Action::Bump(index));
            }
        }
        for label in [Label::None, Label::Fsn, Label::Syd] {
            if label != state.config.region {
                actions.push(Action::Region(label));
            }
        }
    }

    fn next_state(&self, state: &State, action: Action) -> Option<State> {
        if action == Action::Deploy {
            return Some(self.deploy(state));
        }
        let mut next = state.clone();
        next.changes_left -= 1;
        next.broke = 0;
        next.reached = 0;
        match action {
            Action::Deploy => unreachable!("handled above"),
            Action::Relabel(server, label) => {
                next.server_mut(usize::from(server)).label = label;
                next.since.labels = true;
            }
            Action::Add(server) => {
                *next.server_mut(usize::from(server)) = Server {
                    present: true,
                    label: Label::None,
                    accepts: true,
                };
                next.since.added = true;
            }
            Action::Remove(server) => {
                next.server_mut(usize::from(server)).present = false;
                next.containers
                    .retain(|container| container.server != server);
                next.since.removed = true;
            }
            Action::ToggleAccepts(server) => {
                let settings = next.server_mut(usize::from(server));
                settings.accepts = !settings.accepts;
                next.since.accepts = true;
            }
            Action::Counts(service, replicas) => {
                let settings = next.config.service_mut(usize::from(service));
                if replicas.shrinks_from(settings.replicas) {
                    next.since.shrank = true;
                }
                settings.replicas = replicas;
                next.since.config = true;
                next.refused.clear();
            }
            Action::Region(label) => {
                next.config.region = label;
                next.since.config = true;
                next.refused.clear();
            }
            Action::Bump(service) => {
                next.config.service_mut(usize::from(service)).version = 2;
                next.since.config = true;
                next.refused.clear();
            }
        }
        Some(next)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        macro_rules! always {
            ($name:expr) => {
                Property::always($name, |_: &Self, state: &State| {
                    state.broke & bit(&PROPERTIES, $name) == 0
                })
            };
        }
        macro_rules! sometimes {
            ($name:expr) => {
                Property::sometimes($name, |_: &Self, state: &State| {
                    state.reached & bit(&REACHED, $name) != 0
                })
            };
        }
        vec![
            always!(LABELS_KEEP),
            always!(SURPLUS_GOES),
            always!(MET_STARTS_NOTHING),
            always!(WAIT_NAMES_SHORT),
            always!(SHORT_IS_NAMED),
            always!(WAITS_WHILE_SHORT),
            always!(NOTHING_APPLIED),
            always!(WAITS_ON_THE_WORLD),
            always!(REFUSAL_STICKS),
            always!(REFUSED_ONLY_APART),
            always!(ON_THE_WRITER),
            always!(NEVER_FEWER),
            always!(OLD_COUNTED),
            always!(REPEAT_IS_NOOP),
            sometimes!(PARTIAL_WAIT),
            sometimes!(WHOLE_WAIT),
            sometimes!(REFUSAL),
            sometimes!(SPARE_KEPT),
            sometimes!(HELD_BACK),
            sometimes!(PLAN_FAILS),
            sometimes!(OUTDATED_SPARE),
        ]
    }
}

impl RegionPlacement {
    fn world(&self, state: &State) -> String {
        let mut line = String::new();
        for (server, settings) in state.servers.iter().enumerate() {
            if !settings.present {
                continue;
            }
            let held: Vec<String> = state
                .containers
                .iter()
                .filter(|container| usize::from(container.server) == server)
                .map(|container| {
                    format!(
                        "{} v{}",
                        service_name(usize::from(container.service)),
                        container.made.version
                    )
                })
                .collect();
            let _ = write!(
                line,
                "{}({}{}): [{}]  ",
                server_name(server),
                settings.label.name(),
                if settings.accepts {
                    ""
                } else {
                    ", takes no new starts"
                },
                held.join(", ")
            );
        }
        let config: Vec<String> = state
            .config
            .services
            .iter()
            .enumerate()
            .map(|(service, settings)| {
                format!(
                    "{} {} v{}",
                    service_name(service),
                    match settings.replicas {
                        Replicas::Count(count) => format!("replicas={count}"),
                        Replicas::By { fsn, syd } => format!("replicas={{fsn:{fsn},syd:{syd}}}"),
                    },
                    settings.version
                )
            })
            .collect();
        let _ = write!(
            line,
            "| environment region {} | {}",
            state.config.region.name(),
            config.join(", ")
        );
        line
    }

    fn said(&self, state: &State) -> String {
        match self.plan(state) {
            Err(error) => format!("the planner refuses the Deploy: {error}"),
            Ok(preview) => {
                let mut said = String::new();
                for row in &preview.operations {
                    let op = match &row.operation {
                        DeployOperation::RunContainer {
                            machine_id, spec, ..
                        } => {
                            format!("start {} on {}", spec.name, name_of(machine_id))
                        }
                        DeployOperation::ReplaceContainer(op) => {
                            format!("replace {} on {}", op.spec.name, name_of(&op.machine_id))
                        }
                        DeployOperation::UpdateContainer(op) => {
                            format!(
                                "update {} in place on {}",
                                op.spec.name,
                                name_of(&op.machine_id)
                            )
                        }
                        DeployOperation::RemoveContainer { machine_id, .. } => format!(
                            "remove {} on {}",
                            row.service_name().map_or("?", ServiceName::as_str),
                            name_of(machine_id)
                        ),
                        DeployOperation::PrepareVolumes { .. }
                        | DeployOperation::WaitHealthy { .. }
                        | DeployOperation::StopContainer { .. }
                        | DeployOperation::StopHook { .. }
                        | DeployOperation::RunHook { .. }
                        | DeployOperation::RemoveVolume { .. } => continue,
                    };
                    let _ = write!(said, "{op}; ");
                }
                for wait in preview.waits() {
                    let counts = wait.partial.map(|partial| {
                        partial
                            .regions
                            .iter()
                            .map(|(region, wait)| {
                                format!(
                                    "{region}: {} waiting, {} on the old version",
                                    wait.waiting, wait.outdated
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    });
                    let _ = write!(
                        said,
                        "{} {:?}: {}{}; ",
                        wait.service,
                        wait.kind,
                        wait.error.message,
                        counts.map_or(String::new(), |counts| format!(" ({counts})"))
                    );
                }
                if said.is_empty() {
                    said.push_str("nothing to do");
                }
                said
            }
        }
    }
}

/// The regions a Wait tells the user are short: those its error names (`RegionUnmatched`,
/// whole or partial, and the region of `VolumeOutsideRegion`) and those its partial
/// counts list.
fn named(wait: &ployz_core::Wait<'_>) -> BTreeSet<Label> {
    let label = |name: &str| {
        Label::REGIONS
            .into_iter()
            .find(|label| label.region().is_some_and(|region| region.as_str() == name))
            .expect("a modelled region")
    };
    let errored = wait
        .error
        .details
        .get("regions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|region| label(region.as_str().expect("a region name")));
    let outside = (wait.error.details.get("reason")
        == Some(&serde_json::json!("volume_outside_region")))
    .then(|| wait.error.details.get("region"))
    .flatten()
    .map(|region| label(region.as_str().expect("a region name")));
    let partial = wait
        .partial
        .into_iter()
        .flat_map(|partial| partial.regions.keys())
        .map(|region| label(region.as_str()));
    errored.chain(outside).chain(partial).collect()
}

fn name_of(machine: &MachineId) -> &'static str {
    (0..SERVERS)
        .find(|server| machine_id(*server) == *machine)
        .map_or("?", |server| server_name(server))
}

impl Narrated for RegionPlacement {
    fn story(&self, path: &[(State, Option<Action>)]) -> String {
        let mut story = String::new();
        if let Some((first, _)) = path.first() {
            let _ = writeln!(
                story,
                "  start, after the first Deploy: {}",
                self.world(first)
            );
        }
        for (state, action) in path {
            let Some(action) = action else { continue };
            let step = match action {
                Action::Deploy => format!("Deploy: {}", self.said(state)),
                Action::Relabel(server, label) => format!(
                    "label {} {}",
                    server_name(usize::from(*server)),
                    label.name()
                ),
                Action::Add(server) => format!("add {}", server_name(usize::from(*server))),
                Action::Remove(server) => {
                    format!("remove {}", server_name(usize::from(*server)))
                }
                Action::ToggleAccepts(server) => format!(
                    "{} {}",
                    server_name(usize::from(*server)),
                    if state.server(usize::from(*server)).accepts {
                        "stops taking new starts"
                    } else {
                        "takes new starts again"
                    }
                ),
                Action::Counts(service, replicas) => format!(
                    "set {} replicas to {replicas:?}",
                    service_name(usize::from(*service))
                ),
                Action::Region(label) => format!("set the environment region to {}", label.name()),
                Action::Bump(service) => {
                    format!(
                        "change {}'s image to v2",
                        service_name(usize::from(*service))
                    )
                }
            };
            let next = self.next_state(state, *action).expect("a next state");
            let broke: Vec<&str> = PROPERTIES
                .iter()
                .filter(|name| next.broke & bit(&PROPERTIES, name) != 0)
                .copied()
                .collect();
            let _ = writeln!(story, "  {step}\n    -> {}", self.world(&next));
            if !broke.is_empty() {
                let _ = writeln!(story, "    breaks: {}", broke.join("; "));
            }
        }
        story
    }
}

#[cfg(test)]
mod tests {
    use stateright::Checker;

    use super::*;
    use crate::harness::{check, follow, last};

    /// Check every state of `model`: each property in `findings` fails and every other
    /// holds, and every outcome in `REACHED` but `unreached` is reached.
    fn holds(model: RegionPlacement, findings: &[&str], unreached: &[&str]) {
        let scenario = model.scenario;
        let checker = check(model);
        println!("{scenario:?}: {} states", checker.unique_state_count());
        for name in PROPERTIES {
            let found = checker.discovery(name);
            if let Some(path) = &found {
                println!("{name} fails: {:?}", path.clone().into_actions());
            }
            assert_eq!(
                found.is_some(),
                findings.contains(&name),
                "{scenario:?}: {name} {}",
                if found.is_some() { "fails" } else { "holds" }
            );
        }
        for name in REACHED {
            assert_eq!(
                checker.discovery(name).is_some(),
                !unreached.contains(&name),
                "{scenario:?}: {name}"
            );
        }
    }

    /// Whether the last Deploy of `path` broke `property`.
    fn broke(path: &crate::harness::Path<RegionPlacement>, property: &str) -> bool {
        last(path).broke & bit(&PROPERTIES, property) != 0
    }

    #[test]
    fn plain_holds_but_for_its_findings() {
        holds(
            RegionPlacement::new(Scenario::Plain, 4),
            &[
                LABELS_KEEP,
                SURPLUS_GOES,
                WAITS_WHILE_SHORT,
                OLD_COUNTED,
                REPEAT_IS_NOOP,
            ],
            &[REFUSAL],
        );
    }

    #[test]
    fn volume_holds_but_for_its_findings() {
        holds(
            RegionPlacement::new(Scenario::Volume, 4),
            &[WAITS_WHILE_SHORT, SURPLUS_GOES, OLD_COUNTED],
            &[REFUSAL],
        );
    }

    #[test]
    fn shared_holds_but_for_its_findings() {
        holds(
            RegionPlacement::new(Scenario::Shared, 3),
            &[SURPLUS_GOES, WAITS_WHILE_SHORT, REFUSAL_STICKS, OLD_COUNTED],
            &[PARTIAL_WAIT, OUTDATED_SPARE],
        );
    }

    /// #4368: the fsn holder takes no new starts and syd's Server loses its label, so
    /// syd waits on a spare. Labelling an empty Server fsn starts a copy there and removes
    /// the spare, though syd still waits.
    #[test]
    fn a_relabel_removes_a_spare_its_region_still_waits_for() {
        let path = follow(
            RegionPlacement::new(Scenario::Plain, 4),
            &[
                Action::ToggleAccepts(0),
                Action::Relabel(1, Label::None),
                Action::Relabel(2, Label::Fsn),
                Action::Deploy,
            ],
        );
        assert!(broke(&path, LABELS_KEEP));
    }

    /// #4369: with no Server in either counted region, a new version waits whole and its
    /// line counts none of the replicas still on the old version.
    #[test]
    fn a_whole_wait_counts_no_replica_on_the_old_version() {
        let path = follow(
            RegionPlacement::new(Scenario::Plain, 4),
            &[
                Action::Relabel(0, Label::None),
                Action::Relabel(1, Label::None),
                Action::Bump(0),
                Action::Deploy,
            ],
        );
        assert!(broke(&path, OLD_COUNTED));
    }

    /// One count of 2 lowered to 1 while the Environment's region has no Server: the
    /// Service waits whole and both replicas keep running.
    #[test]
    fn a_whole_wait_keeps_the_surplus_of_a_smaller_count() {
        let path = follow(
            RegionPlacement::new(Scenario::Plain, 4),
            &[
                Action::Counts(0, Replicas::Count(1)),
                Action::Region(Label::Fsn),
                Action::Relabel(0, Label::None),
                Action::Deploy,
            ],
        );
        assert!(broke(&path, SURPLUS_GOES));
    }

    /// A spare kept for a waiting region keeps its old placement, so the next Deploy,
    /// with nothing changed, updates it in place.
    #[test]
    fn a_kept_spare_is_updated_by_the_next_deploy() {
        let path = follow(
            RegionPlacement::new(Scenario::Plain, 4),
            &[
                Action::Counts(0, Replicas::By { fsn: 1, syd: 2 }),
                Action::Deploy,
                Action::Relabel(0, Label::Syd),
                Action::Counts(0, Replicas::By { fsn: 1, syd: 1 }),
                Action::Deploy,
                Action::Deploy,
            ],
        );
        assert!(broke(&path, REPEAT_IS_NOOP));
    }

    /// api counted in syd and worker in fsn share data. With data's Server in fsn, api is
    /// refused; relabelling that Server syd deploys api and refuses worker instead.
    #[test]
    fn relabelling_the_volume_server_moves_the_refusal() {
        let path = follow(
            RegionPlacement::new(Scenario::Shared, 3),
            &[
                Action::Counts(0, Replicas::By { fsn: 0, syd: 1 }),
                Action::Deploy,
                Action::Relabel(0, Label::Syd),
                Action::Deploy,
            ],
        );
        assert!(broke(&path, REFUSAL_STICKS));
    }

    /// api runs its one count in fsn on the Volume's Server, `one`, which stops taking new
    /// starts. The Deploy waits on fsn, though fsn's replica runs up to date. Counted
    /// `{"fsn":1}`, the same world plans nothing.
    #[test]
    fn a_one_count_waits_on_a_region_its_replica_meets() {
        let path = follow(
            RegionPlacement::new(Scenario::Volume, 4),
            &[Action::ToggleAccepts(0), Action::Deploy],
        );
        assert!(broke(&path, WAITS_WHILE_SHORT));
    }

    /// The Store's `all_waiting` before R5 read only blocked Services, so a Deploy that
    /// left replicas waiting in one region and advanced nothing moved Live.
    #[test]
    fn nothing_applied_needs_the_partial_wait() {
        let model = RegionPlacement {
            broken: Broken {
                all_waiting_needs_unattempted: true,
            },
            ..RegionPlacement::new(Scenario::Plain, 4)
        };
        let checker = check(model);
        assert!(checker.discovery(NOTHING_APPLIED).is_some());
    }
}
