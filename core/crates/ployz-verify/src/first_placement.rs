//! A Managed Volume's first copy, under two Deploys of one Service whose lanes end at any
//! moment while their Prepare and Create are still on the wire (#4466).
//!
//! The Volume starts with no copy on three Servers. Each Deploy claims, reading the Server
//! the Store pinned for the first copy (`volume::first_copy::first_copies`, lowered into
//! `DeployIntent::first_copies`), plans, records its Deploy Preview, then sends Prepare and
//! Create. A runner may stop at any moment, a crash before or after its record included,
//! and every request it sent still lands later, once more for one resend. Two Deploys may
//! hold claims at once: the Store's queue keeps that from happening, so the record's
//! refusal below is a second fence the model checks on its own.
//!
//! Each plan calls the real planner, `ployz::deploy::preview_deploy`, with the runner's
//! options, on a `DeploySnapshot` of the Servers as they are at that step: storage answers
//! are live, so a plan sees every copy made so far. One Server may run out of room at any
//! moment, which the planner's storage budget reads. The rest is a hand copy and drifts
//! unless kept in step:
//!
//! - the Store's record of a Deploy Preview (`volume::first_copy::place`, called from
//!   `deployment::record`): one atomic step that pins the Server of a first copy, keeps the
//!   pin a later Preview names, and ends a Deployment whose Preview starts the Volume
//!   elsewhere, before it sends anything;
//! - Prepare on a Server (`prepare_volumes`): the plain-mount check, called as
//!   `ployz_core::admit_plain_mount` on the Server's copy and its view of the others, which
//!   may lag and show none; once it admits, the Volume is ensured, which makes a root when
//!   there is none;
//! - Create on a Server (`create_container`): a slot refuses (`create_volume`'s
//!   `InSlot`); otherwise the Volume is ensured, with no plain-mount check;
//! - one Move, while no Deploy holds the row: the root's Server keeps an old copy in a
//!   slot, and the target's root is a Writer Docker doesn't list until a Deploy registers
//!   it.
//!
//! The record's refusal keeps one Writer on its own: a planner that ignored the pin would
//! still start one root, and every later Deploy would be refused. The planner reading the
//! pin is what lets them go on, so its mutation breaks "a Deploy that read the pin starts the
//! Volume only there", and "never two Writers" only with the record's refusal gone too.
//!
//! A plan may also run on a view of the Cluster that doesn't list the pinned Server, as an
//! Entry Machine that hasn't heard of it yet does; the plan then waits for that Server.
//!
//! Out of scope: a Server away or forgotten (a plan refuses the Volume while any Server
//! doesn't answer; a forgotten Cluster drops its pins), a mirror rm and the forgotten-origin checks after it (`volume_runs`), Global
//! Services, Shared Writes, a Deploy that applies (its Volume then counts as deployed), and
//! a resize.

use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
};

use ployz::deploy::{
    DeployIntent, DeployOperation, DeploySnapshot, PlanOptions, VolumeSnapshot, preview_deploy,
};
use ployz_core::{
    CopyRole, DockerVolume, DockerVolumeId, DockerVolumeName, DockerVolumeStorageObservation,
    Machine, MachineId, MachineName, MachineObservation, MachinePath, MachineStorageObservation,
    MembershipObservation, Namespace, Placement, PullPolicy, RequestedServiceSpec, RestartPolicy,
    ServiceContainerSpec, ServiceMode, ServiceMount, ServiceName, ServiceVolume,
    ServiceVolumeReference, UpdateConfig, WireGuardPublicKey, labels,
};
use stateright::{Model, Property};

const SERVERS: usize = 3;
const IDS: [u8; SERVERS] = [0, 1, 2];
const NAMES: [&str; SERVERS] = ["a", "b", "c"];
const VOLUME: &str = "data";
const VOLUME_BYTES: u64 = 1_073_741_824;
/// Deploys Cloud starts.
const DEPLOYS: u8 = 2;

const TWO_WRITERS: &str = "never two Writers";
const FOLLOWS: &str = "a Deploy that read the pin starts the Volume only there";
const MOVED_REFUSED: &str = "a moved Writer's registration is never refused";
const LATE: &str = "a step lands after its lane ended";
const STORE_REFUSED: &str = "the Store refuses a Preview that starts the Volume elsewhere";
const UNFIT: &str = "a Deploy waits for the pinned Server that is out of room";
const REGISTERED: &str = "a Deploy registers a moved Writer where it is";
const STARTED: &str = "the Volume gets its first copy";

/// One guard taken out.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub(crate) struct Broken {
    /// The planner ignores the pin it was lowered with.
    ignore_pin: bool,
    /// The runner sends Prepare and Create before it records its Preview.
    pin_after_send: bool,
    /// A Deployment's end, whatever it was, deletes the pin.
    retire_on_end: bool,
    /// The record keeps the first pin but lets a Preview that starts the Volume elsewhere go on.
    accept_mismatch: bool,
    /// A plan whose view of the Cluster doesn't list the pinned Server sets the pin aside,
    /// and the record lets it name another.
    replace_unlisted: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FirstPlacement {
    broken: Broken,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Held {
    Nothing,
    Root,
    Slot,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Host {
    held: Held,
    /// Docker lists the root: a Deploy's ensure registered it.
    registered: bool,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Step {
    Plan,
    Record,
    Prepare,
    Create,
    Over,
}

/// What a plan sends, and the Server its Preview makes the first copy on.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Plan {
    prepare: Option<u8>,
    create: Option<u8>,
    first: Option<u8>,
    /// It waits for the pinned Server, which can't take the Volume now.
    unfit: bool,
    /// It set aside a pin on a Server its view didn't list (`replace_unlisted`).
    replaced: bool,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Lane {
    step: Step,
    /// The pin the Store lowered into its Intent at claim.
    pin: Option<u8>,
    plan: Option<Plan>,
    last: Option<Request>,
    resent: bool,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Want {
    Prepare,
    Create,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Request {
    lane: u8,
    to: u8,
    want: Want,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct State {
    hosts: [Host; SERVERS],
    /// The Store's first-copy row.
    pin: Option<u8>,
    lanes: Vec<Lane>,
    requests: Vec<Request>,
    /// The Server out of room, once one is.
    full: Option<u8>,
    moved: bool,
    late: bool,
    store_refused: bool,
    refused_after_move: bool,
    unfit: bool,
    registered_moved: bool,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) enum Action {
    Claim,
    Step(u8),
    /// Lane `k` plans on a view of the Cluster that doesn't list the Server it read as
    /// pinned, as an Entry Machine that hasn't yet heard of a Server may.
    PlanUnlisted(u8),
    Resend(u8),
    End(u8),
    Land(usize),
    /// Land Prepare `i` against a view that shows no root elsewhere.
    LandLagging(usize),
    Fill(u8),
    Move(u8),
}

fn machine_id(server: u8) -> MachineId {
    MachineId::parse(format!("{:032x}", u32::from(server) + 1)).expect("a Machine id")
}

fn server_of(machine: &MachineId) -> u8 {
    IDS.into_iter()
        .find(|server| machine_id(*server) == *machine)
        .expect("a modelled Server")
}

fn namespace() -> Namespace {
    Namespace::parse("app").expect("a Namespace")
}

fn data_name() -> DockerVolumeName {
    namespace().volume_name(&DockerVolumeName::parse(VOLUME).expect("a Volume name"))
}

fn host(state: &State, server: u8) -> Host {
    *state
        .hosts
        .get(usize::from(server))
        .expect("a modelled Server")
}

fn host_mut(state: &mut State, server: u8) -> &mut Host {
    state
        .hosts
        .get_mut(usize::from(server))
        .expect("a modelled Server")
}

fn lane_mut(state: &mut State, lane: u8) -> &mut Lane {
    state.lanes.get_mut(usize::from(lane)).expect("a lane")
}

/// The Service every Deploy ships: one replica mounting `data`.
fn service() -> RequestedServiceSpec {
    let reference = ServiceVolumeReference::parse(VOLUME).expect("a reference");
    let mut spec = RequestedServiceSpec {
        name: ServiceName::parse("db").expect("a Service name"),
        mode: ServiceMode::Replicated {
            replicas: NonZeroU32::MIN,
        },
        container: ServiceContainerSpec {
            shared_writes: Default::default(),
            image: "ghcr.io/getployz/db:1".into(),
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
            resources: ployz_core::ContainerResources::default(),
            stop_timeout_secs: None,
            stop_signal: None,
            sysctls: BTreeMap::default(),
            network: None,
            restart: RestartPolicy::default(),
        },
        placement: Placement::default(),
        regions: BTreeMap::default(),
        ports: Vec::new(),
        mount_graph: ployz_core::ServiceMountGraph::default(),
        pre_deploy: None,
        update: UpdateConfig::default(),
    };
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
    spec.set_volume_graph(
        ployz_core::ServiceVolumeGraph::parse(
            vec![ServiceVolume {
                reference: reference.clone(),
                source,
            }],
            vec![ServiceMount {
                volume: reference,
                target: ployz_core::ContainerPath::parse("/data").expect("a path"),
                read_only: false,
                no_copy: false,
                subpath: None,
            }],
        )
        .expect("a Volume graph"),
    )
    .expect("a mount");
    spec
}

/// The registered root's Docker Volume on `server`.
fn data_row(server: u8) -> DockerVolume {
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

/// The Servers as a Deploy observes them now, leaving out `unlisted`.
fn snapshot(state: &State, unlisted: Option<u8>) -> DeploySnapshot {
    let listed = move || {
        IDS.into_iter()
            .filter(move |server| Some(*server) != unlisted)
    };
    let machines = listed()
        .map(|server| {
            let mut observation = MachineObservation::new(
                Machine {
                    id: machine_id(server),
                    name: MachineName::parse(
                        NAMES.get(usize::from(server)).copied().expect("a name"),
                    )
                    .expect("a name"),
                    labels: BTreeMap::default(),
                    accepts_builds: true,
                    accepts_services: true,
                    accepts_ingress: true,
                    subnet: format!("10.210.{}.0/24", server + 1)
                        .parse()
                        .expect("a subnet"),
                    public_key: WireGuardPublicKey([server + 1; 32]),
                    public_ip: None,
                    advertised_endpoints: Vec::new(),
                    runtime: ployz_core::MachineRuntime::default(),
                    join_approval: None,
                    build_concurrency: None,
                },
                MembershipObservation::Up,
            );
            observation.storage = Some(MachineStorageObservation::Ready);
            observation
        })
        .collect();
    let storage_capacity = listed()
        .map(|server| {
            let role = match host(state, server).held {
                Held::Nothing => None,
                Held::Root => Some(CopyRole::Writer),
                Held::Slot => Some(CopyRole::Slot),
            };
            let copies = role
                .map(|role| {
                    (
                        data_name(),
                        ployz_core::ProvisionedCopy::new(
                            ployz_core::CopyPlace::of(role, None),
                            ployz_core::ProvisionedVolumeMaximumBytes::new(
                                NonZeroU64::new(VOLUME_BYTES).expect("a size"),
                            ),
                            0,
                        ),
                    )
                })
                .into_iter()
                .collect();
            let pool_size_bytes = if state.full == Some(server) {
                ployz_core::STORAGE_GIB / 2
            } else {
                1024 * ployz_core::STORAGE_GIB
            };
            (
                machine_id(server),
                Ok(ployz_core::StorageCapacity {
                    backing: ployz_core::StorageBacking::Fixed { pool_size_bytes },
                    unmanaged_used_bytes: 0,
                    slot_bound_bytes: 0,
                    volumes: BTreeMap::new(),
                    copies,
                }),
            )
        })
        .collect();
    DeploySnapshot {
        machines,
        storage_capacity,
        volume_snapshot: VolumeSnapshot::try_from_observations(
            listed()
                .filter(|server| {
                    let host = host(state, *server);
                    host.held == Held::Root && host.registered
                })
                .map(data_row)
                .collect::<Vec<_>>(),
        )
        .expect("a Volume Snapshot"),
        ..DeploySnapshot::default()
    }
}

impl FirstPlacement {
    pub(crate) const fn broken(broken: Broken) -> Self {
        Self { broken }
    }

    /// What a Deploy that claimed with `pin` plans on `state`, as the runner plans it,
    /// with a view that leaves the pinned Server out when `unlisted`.
    fn plan(&self, state: &State, pin: Option<u8>, unlisted: bool) -> Plan {
        let unlisted = pin.filter(|_| unlisted);
        let replaced = unlisted.is_some() && self.broken.replace_unlisted;
        let pins = pin
            .filter(|_| !self.broken.ignore_pin && !replaced)
            .map(|server| (data_name(), machine_id(server)))
            .into_iter()
            .collect();
        let intent = DeployIntent::apply_all(
            namespace(),
            [&service()],
            PlanOptions {
                block_unmountable: true,
                ..PlanOptions::default()
            },
        )
        .with_first_copies(pins);
        let preview = match preview_deploy(&intent, &snapshot(state, unlisted)) {
            Ok(preview) => preview,
            // A root on the Server out of room fails the Deploy before it sends anything.
            Err(error) if error.to_string().contains("cannot admit this Deploy") => {
                return Plan {
                    prepare: None,
                    create: None,
                    first: None,
                    unfit: false,
                    replaced: false,
                };
            }
            Err(error) => panic!("the runner plans every state: {error}"),
        };
        let on = |prepare: bool| {
            preview
                .operations
                .iter()
                .find_map(|row| match (&row.operation, prepare) {
                    (DeployOperation::PrepareVolumes { machine_id, .. }, true)
                    | (DeployOperation::RunContainer { machine_id, .. }, false) => {
                        Some(server_of(machine_id))
                    }
                    _ => None,
                })
        };
        Plan {
            prepare: on(true),
            create: on(false),
            unfit: preview.blocked.iter().any(|blocked| {
                blocked.error.details.get("reason") == Some(&serde_json::json!("first_copy_unfit"))
            }),
            first: preview.volumes_to_create.iter().find_map(|create| {
                create.first_copy.as_ref()?;
                Some(server_of(&create.machine_id))
            }),
            replaced,
        }
    }

    /// The lane's next step after `step`, as the runner orders them.
    fn after(&self, step: Step, plan: Plan) -> Step {
        // The next send, or `done` once there is none.
        let sends = |from_prepare: bool, done: Step| {
            if !from_prepare && plan.prepare.is_some() {
                Step::Prepare
            } else if plan.create.is_some() {
                Step::Create
            } else {
                done
            }
        };
        match (self.broken.pin_after_send, step) {
            (false, Step::Plan) => Step::Record,
            (false, Step::Record) => sends(false, Step::Over),
            (false, Step::Prepare) => sends(true, Step::Over),
            (true, Step::Plan) => sends(false, Step::Record),
            (true, Step::Prepare) => sends(true, Step::Record),
            (true, Step::Create) => Step::Record,
            (true, Step::Record) | (false, Step::Create) | (_, Step::Over) => Step::Over,
        }
    }

    fn end(&self, state: &mut State, lane: u8) {
        lane_mut(state, lane).step = Step::Over;
        if self.broken.retire_on_end {
            state.pin = None;
        }
    }

    /// The Store's record of the lane's Preview: whether the Deployment goes on.
    fn record(&self, state: &mut State, plan: Plan) -> bool {
        let Some(on) = plan.first else {
            return true;
        };
        match state.pin {
            Some(pin) if pin != on && !plan.replaced => {
                if self.broken.accept_mismatch {
                    return true;
                }
                state.store_refused = true;
                state.refused_after_move |= state.moved;
                false
            }
            _ => {
                state.pin = Some(on);
                true
            }
        }
    }

    fn send(state: &mut State, k: u8, to: u8, want: Want) {
        let request = Request { lane: k, to, want };
        state.requests.push(request);
        lane_mut(state, k).last = Some(request);
    }

    fn step(&self, state: &mut State, k: u8, unlisted: bool) {
        let lane = *state.lanes.get(usize::from(k)).expect("a lane");
        match lane.step {
            Step::Plan => {
                let plan = self.plan(state, lane.pin, unlisted);
                let lane = lane_mut(state, k);
                lane.plan = Some(plan);
                lane.step = self.after(Step::Plan, plan);
                state.unfit |= plan.unfit;
            }
            Step::Record => {
                let plan = lane.plan.expect("a recorded plan");
                if self.record(state, plan) {
                    let next = self.after(Step::Record, plan);
                    lane_mut(state, k).step = next;
                    if next == Step::Over {
                        self.end(state, k);
                    }
                } else {
                    self.end(state, k);
                }
            }
            Step::Prepare | Step::Create => {
                let plan = lane.plan.expect("a plan");
                let (to, want) = if lane.step == Step::Prepare {
                    (plan.prepare, Want::Prepare)
                } else {
                    (plan.create, Want::Create)
                };
                Self::send(state, k, to.expect("a planned send"), want);
                let next = self.after(lane.step, plan);
                lane_mut(state, k).step = next;
                if next == Step::Over {
                    self.end(state, k);
                }
            }
            Step::Over => unreachable!("an over lane takes no step"),
        }
    }

    fn land(state: &mut State, index: usize, lagging: bool) {
        let request = state.requests.remove(index);
        state.late |= state
            .lanes
            .get(usize::from(request.lane))
            .is_some_and(|lane| lane.step == Step::Over);
        let moved = state.moved;
        let before = host(state, request.to);
        let ensured = match request.want {
            Want::Prepare => {
                let elsewhere = if lagging {
                    Vec::new()
                } else {
                    known_elsewhere(state, request.to)
                };
                ployz_core::admit_plain_mount(held_copy(before.held).as_ref(), &elsewhere).is_ok()
            }
            Want::Create => before.held != Held::Slot,
        };
        if ensured {
            state.registered_moved |= moved && before.held == Held::Root && !before.registered;
            *host_mut(state, request.to) = Host {
                held: Held::Root,
                registered: true,
            };
        }
    }
}

impl Model for FirstPlacement {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            hosts: [Host {
                held: Held::Nothing,
                registered: false,
            }; SERVERS],
            pin: None,
            lanes: Vec::new(),
            requests: Vec::new(),
            full: None,
            moved: false,
            late: false,
            store_refused: false,
            refused_after_move: false,
            unfit: false,
            registered_moved: false,
        }]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        if state.lanes.len() < usize::from(DEPLOYS) {
            actions.push(Action::Claim);
        }
        for (k, lane) in state.lanes.iter().enumerate() {
            let k = u8::try_from(k).expect("few lanes");
            if lane.step == Step::Plan && lane.pin.is_some() {
                actions.push(Action::PlanUnlisted(k));
            }
            if lane.step != Step::Over {
                actions.push(Action::Step(k));
                actions.push(Action::End(k));
            }
            if lane.step != Step::Over && lane.last.is_some() && !lane.resent {
                actions.push(Action::Resend(k));
            }
        }
        for (index, request) in state.requests.iter().enumerate() {
            actions.push(Action::Land(index));
            if request.want == Want::Prepare {
                actions.push(Action::LandLagging(index));
            }
        }
        if state.full.is_none() {
            actions.extend(IDS.into_iter().map(Action::Fill));
        }
        let roots = IDS
            .into_iter()
            .filter(|server| host(state, *server).held == Held::Root)
            .count();
        if !state.moved && roots == 1 && state.lanes.iter().all(|lane| lane.step == Step::Over) {
            actions.extend(
                IDS.into_iter()
                    .filter(|server| host(state, *server).held == Held::Nothing)
                    .map(Action::Move),
            );
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = last.clone();
        match action {
            Action::Claim => state.lanes.push(Lane {
                step: Step::Plan,
                pin: state.pin,
                plan: None,
                last: None,
                resent: false,
            }),
            Action::Step(k) => self.step(&mut state, k, false),
            Action::PlanUnlisted(k) => self.step(&mut state, k, true),
            Action::Resend(k) => {
                let lane = lane_mut(&mut state, k);
                lane.resent = true;
                let request = lane.last.expect("a sent request");
                state.requests.push(request);
            }
            Action::End(k) => self.end(&mut state, k),
            Action::Land(index) => Self::land(&mut state, index, false),
            Action::LandLagging(index) => Self::land(&mut state, index, true),
            Action::Fill(server) => state.full = Some(server),
            Action::Move(to) => {
                for server in IDS {
                    let host = host_mut(&mut state, server);
                    if host.held == Held::Root {
                        *host = Host {
                            held: Held::Slot,
                            registered: false,
                        };
                    }
                }
                *host_mut(&mut state, to) = Host {
                    held: Held::Root,
                    registered: false,
                };
                state.moved = true;
            }
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(TWO_WRITERS, |_, state: &State| {
                state
                    .hosts
                    .iter()
                    .filter(|host| host.held == Held::Root)
                    .count()
                    <= 1
            }),
            Property::always(FOLLOWS, |_, state: &State| {
                state.lanes.iter().all(|lane| {
                    let first = lane.plan.and_then(|plan| plan.first);
                    match (lane.pin, first) {
                        (Some(pin), Some(on)) => on == pin,
                        _ => true,
                    }
                })
            }),
            Property::always(MOVED_REFUSED, |_, state: &State| !state.refused_after_move),
            Property::sometimes(LATE, |_, state: &State| state.late),
            Property::sometimes(STORE_REFUSED, |_, state: &State| state.store_refused),
            Property::sometimes(UNFIT, |_, state: &State| state.unfit),
            Property::sometimes(REGISTERED, |_, state: &State| state.registered_moved),
            Property::sometimes(STARTED, |_, state: &State| {
                state.hosts.iter().any(|host| host.held == Held::Root)
            }),
        ]
    }
}

/// The copy a Server's plugin inspect answers with for `held`.
fn held_copy(held: Held) -> Option<ployz_core::VolumeCopy> {
    match held {
        Held::Nothing => None,
        Held::Root => Some(ployz_core::VolumeCopy::Root {
            readonly: false,
            newest: None,
            epoch: ployz_core::WriterEpoch::default(),
            seed: false,
            mark: None,
            origin: None,
            placed: None,
        }),
        Held::Slot => Some(ployz_core::VolumeCopy::Slot {
            slot: ployz_core::SlotId::new(1),
            readonly: true,
            newest: None,
            resume_token: None,
            pull: None,
            role: None,
            pull_state: None,
            origin: None,
            written_here: false,
        }),
    }
}

/// Every copy on a Server other than `me`, as its replicated view lists them.
fn known_elsewhere(state: &State, me: u8) -> Vec<ployz_core::KnownCopy> {
    IDS.into_iter()
        .filter(|server| *server != me)
        .filter_map(|server| {
            let role = match host(state, server).held {
                Held::Nothing => return None,
                Held::Root => CopyRole::Writer,
                Held::Slot => CopyRole::Slot,
            };
            Some(ployz_core::KnownCopy {
                machine: MachineName::parse(name(server)).expect("a Server name"),
                role,
            })
        })
        .collect()
}

fn name(server: u8) -> &'static str {
    NAMES
        .get(usize::from(server))
        .copied()
        .expect("a modelled Server")
}

/// The Servers' copies, the pin and the lanes, as one line.
fn world(state: &State) -> String {
    let hosts = IDS
        .into_iter()
        .map(|server| {
            let host = host(state, server);
            let held = match (host.held, host.registered) {
                (Held::Nothing, _) => "-",
                (Held::Root, true) => "root",
                (Held::Root, false) => "root (unlisted)",
                (Held::Slot, _) => "slot",
            };
            format!("{} {held}", name(server))
        })
        .collect::<Vec<_>>()
        .join(", ");
    let lanes = state
        .lanes
        .iter()
        .enumerate()
        .map(|(k, lane)| format!("D{k} {:?}", lane.step))
        .collect::<Vec<_>>()
        .join(", ");
    let wire = state
        .requests
        .iter()
        .map(|request| {
            format!(
                "D{} {:?} to {}",
                request.lane,
                request.want,
                name(request.to)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "[{hosts}] pin {} | {lanes} | on the wire: [{wire}]",
        state.pin.map_or("none", name)
    )
}

impl crate::harness::Narrated for FirstPlacement {
    fn story(&self, path: &[(State, Option<Action>)]) -> String {
        let mut story = String::new();
        for (state, action) in path {
            let Some(action) = action else { continue };
            let next = self.next_state(state, *action).expect("a next state");
            let step = if let Action::Step(k) | Action::PlanUnlisted(k) = action {
                let lane = next.lanes.get(usize::from(*k)).expect("a lane");
                format!(
                    "D{k} {:?} (read pin {}, plans {:?})",
                    state.lanes.get(usize::from(*k)).expect("a lane").step,
                    lane.pin.map_or("none", name),
                    lane.plan
                )
            } else {
                format!("{action:?}")
            };
            story.push_str(&format!("  {step}\n    -> {}\n", world(&next)));
        }
        story
    }
}

#[cfg(test)]
mod tests {
    use stateright::Checker;

    use super::*;
    use crate::harness::check;

    const ALWAYS: [&str; 3] = [TWO_WRITERS, FOLLOWS, MOVED_REFUSED];
    const SOMETIMES: [&str; 5] = [LATE, STORE_REFUSED, UNFIT, REGISTERED, STARTED];

    /// Check every state: the properties in `fail` break and the other `always` ones hold.
    fn holds(model: FirstPlacement, fail: &[&str]) -> impl Checker<FirstPlacement> {
        let checker = check(model);
        println!(
            "{:?}: {} states",
            model.broken,
            checker.unique_state_count()
        );
        for name in ALWAYS {
            let found = checker.discovery(name);
            if let Some(path) = &found {
                println!("{name} fails: {:?}", path.clone().into_actions());
            }
            assert_eq!(found.is_some(), fail.contains(&name), "{name}");
        }
        checker
    }

    #[test]
    fn a_first_copy_starts_once() {
        let checker = holds(FirstPlacement::default(), &[]);
        for name in SOMETIMES {
            assert!(checker.discovery(name).is_some(), "{name} is never reached");
        }
    }

    /// The record still refuses what such a plan sends, so one Writer holds, but every
    /// later Deploy is refused.
    #[test]
    fn a_planner_that_ignores_the_pin_strays_from_it() {
        holds(
            FirstPlacement::broken(Broken {
                ignore_pin: true,
                ..Broken::default()
            }),
            &[FOLLOWS],
        );
    }

    #[test]
    fn a_planner_that_ignores_the_pin_and_a_record_that_lets_it_go_on_start_two() {
        holds(
            FirstPlacement::broken(Broken {
                ignore_pin: true,
                accept_mismatch: true,
                ..Broken::default()
            }),
            &[TWO_WRITERS, FOLLOWS],
        );
    }

    #[test]
    fn a_pin_written_after_the_sends_starts_two() {
        holds(
            FirstPlacement::broken(Broken {
                pin_after_send: true,
                ..Broken::default()
            }),
            &[TWO_WRITERS],
        );
    }

    #[test]
    fn a_pin_an_outcome_retires_starts_two() {
        holds(
            FirstPlacement::broken(Broken {
                retire_on_end: true,
                ..Broken::default()
            }),
            &[TWO_WRITERS],
        );
    }

    /// #4470 review: a retry whose Entry Machine hasn't yet heard of the pinned Server
    /// must wait for it, not start the Volume beside a late step there.
    #[test]
    fn a_pin_set_aside_for_an_unlisted_server_starts_two() {
        holds(
            FirstPlacement::broken(Broken {
                replace_unlisted: true,
                ..Broken::default()
            }),
            &[TWO_WRITERS, FOLLOWS],
        );
    }

    #[test]
    fn a_record_that_lets_a_mismatched_preview_go_on_starts_two() {
        holds(
            FirstPlacement::broken(Broken {
                accept_mismatch: true,
                ..Broken::default()
            }),
            &[TWO_WRITERS],
        );
    }
}
