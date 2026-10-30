//! Pure ordering of one Deploy's Service graph: events in, next actions out,
//! and nothing here touches a Machine.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    DeployOperation, MachineId, RemoveContainerPurpose, ServiceName, ServiceOutcome, ServiceResult,
};

/// New Containers one Machine starts at once for one Deploy. Containers still
/// draining from an earlier Deploy are not tasks here, so they never count.
pub(crate) const MACHINE_WINDOW: usize = 5;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Task {
    pub(crate) machine: MachineId,
    pub(crate) kind: TaskKind,
}

/// What a task waits for besides its step order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TaskKind {
    /// Creates a new Container: takes a slot in its Machine's window and waits
    /// until every live proxy lists each changed dependency's new copy.
    Start,
    /// Runs a hook against the dependencies, so it waits for them to be routed
    /// like a start but takes no window slot.
    Hook,
    /// Removes a copy the Service's previous pointer named, so it waits until
    /// every Machine routes the new pointer.
    Unroute,
    /// Changes a serving copy in place: it starts nothing, so it takes no window
    /// slot and waits on no route.
    Update,
    Other,
}

impl TaskKind {
    pub(crate) fn of(operation: &DeployOperation) -> Self {
        match operation {
            DeployOperation::RunHook { .. } => Self::Hook,
            DeployOperation::RemoveContainer {
                purpose: RemoveContainerPurpose::Rollout,
                ..
            } => Self::Unroute,
            DeployOperation::RunContainer { .. } | DeployOperation::ReplaceContainer(_) => {
                Self::Start
            }
            DeployOperation::UpdateContainer(_) => Self::Update,
            DeployOperation::PrepareVolumes { .. }
            | DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::StopHook { .. }
            | DeployOperation::RemoveVolume { .. } => Self::Other,
        }
    }
}

/// Operation indices that run one after another, or together.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Step {
    Serial(usize),
    Parallel(Vec<usize>),
}

/// One Service's operations and the Services it starts after.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceGroup {
    /// `None` with no dependencies is a prelude every Service waits for, such
    /// as Volume preparation; `None` with dependencies is the namespace tail.
    pub service: Option<ServiceName>,
    pub dependencies: Vec<ServiceName>,
    pub steps: Vec<Step>,
    /// Flip this Service's pointer once its steps complete. A group without a
    /// Service never publishes.
    pub publish: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Action {
    Start(usize),
    Publish(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Event {
    Completed(usize),
    Failed(usize),
    Published(usize),
    PublishFailed(usize),
    PublishUnconfirmed(usize),
    /// Published, but a gap Server's proxy never confirmed it, so the Service may
    /// serve either side.
    PublishGapped(usize),
    /// A newer Deploy's copies serve the group's Service.
    NewerServing(usize),
    /// Every live proxy lists `service`'s new copies.
    Routed(ServiceName),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Phase {
    Waiting,
    Active {
        step: usize,
        pending: Vec<usize>,
        running: BTreeSet<usize>,
    },
    Publishing,
    Done,
    Failed,
}

struct Slot {
    group: ServiceGroup,
    phase: Phase,
}

pub(crate) struct Schedule {
    slots: Vec<Slot>,
    tasks: Vec<Task>,
    published: BTreeSet<ServiceName>,
    publish_requested: BTreeSet<usize>,
    routed: BTreeSet<ServiceName>,
    /// Starts dispatched and not yet returned, whatever their group's phase.
    starting: BTreeSet<usize>,
    unconfirmed: BTreeSet<usize>,
    newer_serving: BTreeSet<usize>,
    failed: bool,
}

impl ServiceGroup {
    fn is_prelude(&self) -> bool {
        self.service.is_none() && self.dependencies.is_empty()
    }

    fn changes_its_service(&self) -> bool {
        self.publish || !self.steps.is_empty()
    }

    /// The Service whose pointer this group flips, if any.
    pub(crate) fn publishing_service(&self) -> Option<&ServiceName> {
        self.publish.then_some(self.service.as_ref()).flatten()
    }
}

impl Schedule {
    pub(crate) fn new(groups: Vec<ServiceGroup>, tasks: Vec<Task>) -> Self {
        let slots = groups
            .into_iter()
            .map(|group| Slot {
                group,
                phase: Phase::Waiting,
            })
            .collect();
        Self {
            slots,
            tasks,
            published: BTreeSet::new(),
            publish_requested: BTreeSet::new(),
            routed: BTreeSet::new(),
            starting: BTreeSet::new(),
            unconfirmed: BTreeSet::new(),
            newer_serving: BTreeSet::new(),
            failed: false,
        }
    }

    fn slot(&self, group: usize) -> &Slot {
        self.slots
            .get(group)
            .expect("group indices come from this schedule")
    }

    fn slot_mut(&mut self, group: usize) -> &mut Slot {
        self.slots
            .get_mut(group)
            .expect("group indices come from this schedule")
    }

    fn task(&self, task: usize) -> &Task {
        self.tasks
            .get(task)
            .expect("task indices come from this schedule")
    }

    pub(crate) fn group(&self, index: usize) -> &ServiceGroup {
        &self.slot(index).group
    }

    pub(crate) fn group_of(&self, task: usize) -> usize {
        self.slots
            .iter()
            .position(|slot| {
                slot.group.steps.iter().any(|step| match step {
                    Step::Serial(index) => *index == task,
                    Step::Parallel(indices) => indices.contains(&task),
                })
            })
            .expect("every task belongs to one group")
    }

    #[cfg(test)]
    pub(crate) fn failed(&self) -> bool {
        self.failed
    }

    pub(crate) fn failed_groups(&self) -> Vec<usize> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| matches!(slot.phase, Phase::Failed))
            .map(|(group, _)| group)
            .collect()
    }

    pub(crate) fn services(&self) -> Vec<ServiceOutcome> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.group.changes_its_service())
            .filter_map(|(index, Slot { group, phase })| {
                let result = match phase {
                    _ if self.newer_serving.contains(&index) => ServiceResult::Newer,
                    _ if self.unconfirmed.contains(&index) => ServiceResult::Unconfirmed,
                    Phase::Done if self.updates_in_place(group) => ServiceResult::Updated,
                    Phase::Done => ServiceResult::New,
                    Phase::Waiting => ServiceResult::NotStarted,
                    Phase::Active { .. } | Phase::Publishing | Phase::Failed => ServiceResult::Old,
                };
                group
                    .service
                    .clone()
                    .map(|service| ServiceOutcome { service, result })
            })
            .collect()
    }

    /// The group changes its serving copies in place and starts none.
    fn updates_in_place(&self, group: &ServiceGroup) -> bool {
        let kinds = || {
            group
                .steps
                .iter()
                .flat_map(step_tasks)
                .map(|task| self.task(task).kind)
        };
        kinds().any(|kind| kind == TaskKind::Update) && kinds().all(|kind| kind != TaskKind::Start)
    }

    /// The group's tasks, or the following group's when a Service only
    /// removes Containers and its removals run after its flip.
    fn tasks_of(&self, group: usize) -> impl Iterator<Item = usize> + '_ {
        self.slots
            .iter()
            .skip(group)
            .map(|slot| {
                slot.group
                    .steps
                    .iter()
                    .flat_map(step_tasks)
                    .collect::<Vec<_>>()
            })
            .find(|tasks| !tasks.is_empty())
            .into_iter()
            .flatten()
    }

    /// The row a failed publish of `group` is reported under: the Service's last
    /// operation, since a publish has no row of its own.
    pub(crate) fn publish_failure_task(&self, group: usize) -> usize {
        self.tasks_of(group)
            .max()
            .expect("a publishing group has a task of its own or after it")
    }

    #[cfg(test)]
    pub(crate) fn finished(&self) -> bool {
        self.slots.iter().all(|slot| match slot.phase {
            Phase::Done | Phase::Failed => true,
            Phase::Waiting => self.failed,
            Phase::Active { .. } | Phase::Publishing => false,
        })
    }

    pub(crate) fn waiting_on_routes(&self) -> Vec<(usize, ServiceName)> {
        let mut blocked = Vec::new();
        for Slot { group, phase } in &self.slots {
            let pending = match phase {
                Phase::Waiting if !self.failed && self.dependencies_done(group) => {
                    group.steps.first().map_or(Vec::new(), step_tasks)
                }
                Phase::Active { pending, .. } => pending.clone(),
                Phase::Waiting | Phase::Publishing | Phase::Done | Phase::Failed => Vec::new(),
            };
            for task in pending {
                blocked.extend(
                    self.unrouted_dependencies(group, task)
                        .map(|service| (task, service.clone())),
                );
            }
        }
        blocked
    }

    pub(crate) fn apply(&mut self, event: Event) {
        match event {
            Event::Completed(task) => {
                self.starting.remove(&task);
                let group = self.group_of(task);
                if let Phase::Active { running, .. } = &mut self.slot_mut(group).phase {
                    running.remove(&task);
                }
                self.advance(group);
            }
            Event::Failed(task) => {
                self.starting.remove(&task);
                let group = self.group_of(task);
                self.slot_mut(group).phase = Phase::Failed;
                self.failed = true;
            }
            Event::Published(group) => {
                let slot = self.slot_mut(group);
                slot.phase = Phase::Done;
                if let Some(service) = slot.group.service.clone() {
                    self.published.insert(service);
                }
            }
            Event::PublishFailed(group) => {
                self.slot_mut(group).phase = Phase::Failed;
                self.failed = true;
            }
            Event::PublishUnconfirmed(group) => {
                self.unconfirmed.insert(group);
                self.apply(Event::PublishFailed(group));
            }
            Event::PublishGapped(group) => {
                self.unconfirmed.insert(group);
                self.apply(Event::Published(group));
            }
            Event::NewerServing(group) => {
                self.newer_serving.insert(group);
            }
            Event::Routed(service) => {
                self.routed.insert(service);
            }
        }
    }

    pub(crate) fn next(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut windows = self.windows();
        for group in 0..self.slots.len() {
            if matches!(self.slot(group).phase, Phase::Waiting)
                && !self.failed
                && self.dependencies_done(&self.slot(group).group)
            {
                let first = self.step_phase(group, 0);
                self.slot_mut(group).phase = first;
            }
            let Phase::Active {
                step,
                pending,
                running,
            } = &self.slot(group).phase
            else {
                continue;
            };
            let (step, mut pending, mut running) = (*step, pending.clone(), running.clone());
            if self.failed && step == 0 && running.is_empty() && !self.began(group, &pending) {
                self.slot_mut(group).phase = Phase::Waiting;
                continue;
            }
            let serial = matches!(
                self.slot(group).group.steps.get(step),
                Some(Step::Serial(_))
            );
            let mut still_pending = Vec::new();
            for task in pending.drain(..) {
                if serial && !running.is_empty() {
                    still_pending.push(task);
                    continue;
                }
                let Task { machine, kind } = self.task(task);
                let starts = *kind == TaskKind::Start;
                let window = windows.entry(*machine).or_insert(0);
                let room = !starts || *window < MACHINE_WINDOW;
                if room
                    && self
                        .unrouted_dependencies(&self.slot(group).group, task)
                        .next()
                        .is_none()
                {
                    if starts {
                        *window += 1;
                        self.starting.insert(task);
                    }
                    running.insert(task);
                    actions.push(Action::Start(task));
                } else {
                    still_pending.push(task);
                }
            }
            self.slot_mut(group).phase = Phase::Active {
                step,
                pending: still_pending,
                running,
            };
        }
        for group in 0..self.slots.len() {
            if matches!(self.slot(group).phase, Phase::Publishing)
                && self.publish_requested.insert(group)
            {
                actions.push(Action::Publish(group));
            }
        }
        actions
    }

    /// Some task of the group's first step already ran.
    fn began(&self, group: usize, pending: &[usize]) -> bool {
        self.group(group)
            .steps
            .first()
            .is_some_and(|first| step_tasks(first).len() != pending.len())
    }

    fn windows(&self) -> BTreeMap<MachineId, usize> {
        let mut windows = BTreeMap::new();
        for task in &self.starting {
            *windows.entry(self.task(*task).machine).or_insert(0) += 1;
        }
        windows
    }

    fn step_phase(&self, group: usize, step: usize) -> Phase {
        let group = self.group(group);
        match group.steps.get(step) {
            Some(tasks) => Phase::Active {
                step,
                pending: step_tasks(tasks),
                running: BTreeSet::new(),
            },
            None if group.publishing_service().is_some() => Phase::Publishing,
            None => Phase::Done,
        }
    }

    fn advance(&mut self, group: usize) {
        let Phase::Active {
            step,
            pending,
            running,
        } = &self.slot(group).phase
        else {
            return;
        };
        if pending.is_empty() && running.is_empty() {
            let next = self.step_phase(group, step + 1);
            self.slot_mut(group).phase = next;
        }
    }

    fn dependencies_done(&self, group: &ServiceGroup) -> bool {
        let preludes_done = group.is_prelude()
            || self
                .slots
                .iter()
                .filter(|slot| slot.group.is_prelude())
                .all(|slot| matches!(slot.phase, Phase::Done));
        preludes_done
            && group.dependencies.iter().all(|dependency| {
                self.slots
                    .iter()
                    .find(|slot| slot.group.service.as_ref() == Some(dependency))
                    .is_none_or(|slot| matches!(slot.phase, Phase::Done))
            })
    }

    fn unrouted_dependencies<'a>(
        &'a self,
        group: &'a ServiceGroup,
        task: usize,
    ) -> impl Iterator<Item = &'a ServiceName> {
        let waits = !matches!(self.task(task).kind, TaskKind::Other | TaskKind::Update);
        group.dependencies.iter().filter(move |dependency| {
            waits && self.published.contains(*dependency) && !self.routed.contains(*dependency)
        })
    }
}

/// A new Container of the Service starts here, so the task takes a window
/// slot and the Deploy removes the Container if the Service never switches.
pub(crate) fn starts_container(operation: &DeployOperation) -> bool {
    TaskKind::of(operation) == TaskKind::Start
}

fn step_tasks(step: &Step) -> Vec<usize> {
    match step {
        Step::Serial(index) => vec![*index],
        Step::Parallel(indices) => indices.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(hex: char) -> MachineId {
        MachineId::parse(hex.to_string().repeat(32)).unwrap()
    }

    fn name(service: &str) -> ServiceName {
        ServiceName::parse(service).unwrap()
    }

    fn group(service: &str, dependencies: &[&str], steps: Vec<Step>) -> ServiceGroup {
        ServiceGroup {
            service: Some(name(service)),
            dependencies: dependencies.iter().map(|name| self::name(name)).collect(),
            steps,
            publish: true,
        }
    }

    fn start(machine: char) -> Task {
        Task {
            machine: self::machine(machine),
            kind: TaskKind::Start,
        }
    }

    fn hook(machine: char) -> Task {
        Task {
            machine: self::machine(machine),
            kind: TaskKind::Hook,
        }
    }

    fn starts(schedule: &mut Schedule) -> Vec<usize> {
        schedule
            .next()
            .into_iter()
            .filter_map(|action| match action {
                Action::Start(task) => Some(task),
                Action::Publish(_) => None,
            })
            .collect()
    }

    fn publishes(schedule: &mut Schedule) -> Vec<usize> {
        schedule
            .next()
            .into_iter()
            .filter_map(|action| match action {
                Action::Publish(group) => Some(group),
                Action::Start(_) => None,
            })
            .collect()
    }

    fn routed(service: &str) -> Event {
        Event::Routed(name(service))
    }

    #[test]
    fn a_chain_starts_each_service_after_its_dependency_publishes_and_is_routed() {
        let mut schedule = Schedule::new(
            vec![
                group("db", &[], vec![Step::Parallel(vec![0])]),
                group("api", &["db"], vec![Step::Parallel(vec![1])]),
                group("web", &["api"], vec![Step::Parallel(vec![2])]),
            ],
            vec![start('1'), start('1'), start('1')],
        );
        assert_eq!(starts(&mut schedule), vec![0]);
        assert!(starts(&mut schedule).is_empty());
        schedule.apply(Event::Completed(0));
        assert_eq!(publishes(&mut schedule), vec![0]);
        assert!(starts(&mut schedule).is_empty(), "api waits on the publish");
        schedule.apply(Event::Published(0));
        assert!(
            starts(&mut schedule).is_empty(),
            "api waits until every proxy lists db"
        );
        assert_eq!(schedule.waiting_on_routes(), vec![(1, name("db"))]);
        schedule.apply(routed("db"));
        assert_eq!(starts(&mut schedule), vec![1]);
        schedule.apply(Event::Completed(1));
        schedule.apply(Event::Published(1));
        schedule.apply(routed("api"));
        assert_eq!(starts(&mut schedule), vec![2]);
        schedule.apply(Event::Completed(2));
        schedule.apply(Event::Published(2));
        assert!(schedule.finished());
        assert!(!schedule.failed());
    }

    #[test]
    fn a_diamond_starts_the_join_only_after_both_sides_publish() {
        let mut schedule = Schedule::new(
            vec![
                group("db", &[], vec![Step::Parallel(vec![0])]),
                group("cache", &["db"], vec![Step::Parallel(vec![1])]),
                group("queue", &["db"], vec![Step::Parallel(vec![2])]),
                group("api", &["cache", "queue"], vec![Step::Parallel(vec![3])]),
            ],
            vec![start('1'), start('1'), start('2'), start('2')],
        );
        assert_eq!(starts(&mut schedule), vec![0]);
        schedule.apply(Event::Completed(0));
        schedule.apply(Event::Published(0));
        schedule.apply(routed("db"));
        assert_eq!(starts(&mut schedule), vec![1, 2]);
        schedule.apply(Event::Completed(1));
        schedule.apply(Event::Published(1));
        schedule.apply(routed("cache"));
        assert!(starts(&mut schedule).is_empty(), "queue has not published");
        schedule.apply(Event::Completed(2));
        schedule.apply(Event::Published(2));
        assert!(
            starts(&mut schedule).is_empty(),
            "no proxy is known to list queue yet"
        );
        schedule.apply(routed("queue"));
        assert_eq!(starts(&mut schedule), vec![3]);
    }

    #[test]
    fn independent_services_start_together() {
        let mut schedule = Schedule::new(
            vec![
                group("a", &[], vec![Step::Parallel(vec![0])]),
                group("b", &[], vec![Step::Parallel(vec![1])]),
                group("c", &[], vec![Step::Parallel(vec![2])]),
            ],
            vec![start('1'), start('2'), start('3')],
        );
        assert_eq!(starts(&mut schedule), vec![0, 1, 2]);
    }

    #[test]
    fn a_dependency_with_no_change_counts_as_complete() {
        let mut schedule = Schedule::new(
            vec![group("api", &["db"], vec![Step::Parallel(vec![0])])],
            vec![start('1')],
        );
        assert_eq!(starts(&mut schedule), vec![0]);
        assert!(schedule.waiting_on_routes().is_empty());
    }

    #[test]
    fn a_machine_starts_at_most_five_new_containers_at_once() {
        let steps = vec![Step::Parallel((0..7).collect())];
        let mut schedule = Schedule::new(
            vec![group("api", &[], steps)],
            (0..7).map(|_| start('1')).collect(),
        );
        assert_eq!(starts(&mut schedule), vec![0, 1, 2, 3, 4]);
        assert!(starts(&mut schedule).is_empty());
        schedule.apply(Event::Completed(2));
        schedule.apply(Event::Completed(4));
        assert_eq!(starts(&mut schedule), vec![5, 6]);
    }

    #[test]
    fn the_window_counts_only_this_deploys_starts_and_is_per_machine() {
        let steps = vec![Step::Parallel((0..12).collect())];
        let mut tasks: Vec<Task> = (0..12)
            .map(|index| {
                if index % 2 == 0 {
                    start('1')
                } else {
                    start('2')
                }
            })
            .collect();
        tasks.push(Task {
            machine: machine('1'),
            kind: TaskKind::Other,
        });
        tasks.push(hook('2'));
        let mut schedule = Schedule::new(
            vec![
                group("api", &[], steps),
                group("web", &[], vec![Step::Parallel(vec![12, 13])]),
            ],
            tasks,
        );
        assert_eq!(
            starts(&mut schedule),
            vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 12, 13],
            "stops and hooks take no slot even on a full Machine"
        );
        assert!(starts(&mut schedule).is_empty());
        schedule.apply(Event::Completed(12));
        schedule.apply(Event::Completed(13));
        assert!(starts(&mut schedule).is_empty(), "non-starts free no slot");
        schedule.apply(Event::Completed(0));
        schedule.apply(Event::Completed(1));
        assert_eq!(starts(&mut schedule), vec![10, 11]);
    }

    #[test]
    fn a_window_starved_service_never_starts_after_a_failure() {
        let mut schedule = Schedule::new(
            vec![
                group("a", &[], vec![Step::Parallel((0..6).collect())]),
                group("b", &[], vec![Step::Parallel(vec![6])]),
            ],
            (0..7).map(|_| start('1')).collect(),
        );
        assert_eq!(starts(&mut schedule), vec![0, 1, 2, 3, 4]);
        schedule.apply(Event::Failed(0));
        assert!(starts(&mut schedule).is_empty(), "b started nothing yet");
        for task in 1..5 {
            schedule.apply(Event::Completed(task));
        }
        assert!(starts(&mut schedule).is_empty());
        assert!(schedule.finished());
    }

    #[test]
    fn a_failed_services_running_starts_keep_their_slots() {
        let mut schedule = Schedule::new(
            vec![
                group("a", &[], vec![Step::Parallel(vec![0, 1, 2])]),
                group("b", &[], vec![Step::Parallel((3..8).collect())]),
            ],
            (0..8).map(|_| start('1')).collect(),
        );
        assert_eq!(starts(&mut schedule), vec![0, 1, 2, 3, 4]);
        schedule.apply(Event::Failed(0));
        assert_eq!(
            starts(&mut schedule),
            vec![5],
            "a's two running starts still hold their slots"
        );
        schedule.apply(Event::Completed(1));
        assert_eq!(starts(&mut schedule), vec![6]);
        schedule.apply(Event::Completed(2));
        assert_eq!(starts(&mut schedule), vec![7]);
    }

    #[test]
    fn a_routed_dependency_admits_every_later_step_of_its_dependent() {
        let mut schedule = Schedule::new(
            vec![
                group("db", &[], vec![Step::Parallel(vec![0])]),
                group("app", &["db"], vec![Step::Serial(1), Step::Serial(2)]),
            ],
            vec![start('1'), start('1'), start('1')],
        );
        assert_eq!(starts(&mut schedule), vec![0]);
        schedule.apply(Event::Completed(0));
        assert_eq!(publishes(&mut schedule), vec![0]);
        schedule.apply(Event::Published(0));
        schedule.apply(routed("db"));
        assert_eq!(starts(&mut schedule), vec![1]);
        schedule.apply(Event::Completed(1));
        assert_eq!(starts(&mut schedule), vec![2]);
        assert!(schedule.waiting_on_routes().is_empty());
    }

    #[test]
    fn a_join_reads_every_dependency_it_lacks_in_one_round() {
        let mut schedule = Schedule::new(
            vec![
                group("db", &[], vec![Step::Parallel(vec![0])]),
                group("cache", &[], vec![Step::Parallel(vec![1])]),
                group("app", &["db", "cache"], vec![Step::Parallel(vec![2])]),
            ],
            vec![start('1'), start('1'), start('1')],
        );
        assert_eq!(starts(&mut schedule), vec![0, 1]);
        schedule.apply(Event::Completed(0));
        schedule.apply(Event::Completed(1));
        assert_eq!(publishes(&mut schedule), vec![0, 1]);
        schedule.apply(Event::Published(0));
        schedule.apply(Event::Published(1));
        assert_eq!(
            schedule.waiting_on_routes(),
            vec![(2, name("db")), (2, name("cache"))]
        );
        for (_, service) in schedule.waiting_on_routes() {
            schedule.apply(Event::Routed(service));
        }
        assert_eq!(starts(&mut schedule), vec![2]);
    }

    #[test]
    fn a_hook_waits_until_a_changed_dependency_is_routed() {
        let mut schedule = Schedule::new(
            vec![
                group("db", &[], vec![Step::Parallel(vec![0])]),
                group("app", &["db"], vec![Step::Serial(1), Step::Serial(2)]),
            ],
            vec![start('1'), hook('2'), start('2')],
        );
        assert_eq!(starts(&mut schedule), vec![0]);
        schedule.apply(Event::Completed(0));
        assert_eq!(publishes(&mut schedule), vec![0]);
        schedule.apply(Event::Published(0));
        assert!(
            starts(&mut schedule).is_empty(),
            "the migration would run against the old db"
        );
        assert_eq!(schedule.waiting_on_routes(), vec![(1, name("db"))]);
        schedule.apply(routed("db"));
        assert_eq!(starts(&mut schedule), vec![1]);
    }

    #[test]
    fn serial_steps_run_one_at_a_time_and_a_non_start_takes_no_window_slot() {
        let steps = vec![Step::Serial(0), Step::Serial(1), Step::Parallel(vec![2, 3])];
        let tasks = vec![
            Task {
                machine: machine('1'),
                kind: TaskKind::Other,
            },
            start('1'),
            start('1'),
            start('1'),
        ];
        let mut schedule = Schedule::new(vec![group("api", &[], steps)], tasks);
        assert_eq!(starts(&mut schedule), vec![0]);
        assert!(starts(&mut schedule).is_empty());
        schedule.apply(Event::Completed(0));
        assert_eq!(starts(&mut schedule), vec![1]);
        schedule.apply(Event::Completed(1));
        assert_eq!(starts(&mut schedule), vec![2, 3]);
        schedule.apply(Event::Completed(2));
        schedule.apply(Event::Completed(3));
        assert_eq!(publishes(&mut schedule), vec![0]);
    }

    #[test]
    fn after_a_failure_no_new_service_starts_while_running_ones_finish() {
        let mut schedule = Schedule::new(
            vec![
                group("a", &[], vec![Step::Parallel(vec![0, 1])]),
                group("b", &[], vec![Step::Parallel(vec![2])]),
                group("c", &["b"], vec![Step::Parallel(vec![3])]),
                group("d", &[], vec![Step::Serial(4), Step::Serial(5)]),
            ],
            vec![
                start('1'),
                start('1'),
                start('2'),
                start('2'),
                start('3'),
                start('3'),
            ],
        );
        assert_eq!(starts(&mut schedule), vec![0, 1, 2, 4]);
        schedule.apply(Event::Failed(0));
        assert!(schedule.failed());
        assert!(!schedule.finished(), "1, 2 and 4 still run");
        schedule.apply(Event::Completed(2));
        assert_eq!(
            publishes(&mut schedule),
            vec![1],
            "b finishes with its publish"
        );
        schedule.apply(Event::Published(1));
        schedule.apply(routed("b"));
        assert!(starts(&mut schedule).is_empty(), "c never starts");
        schedule.apply(Event::Completed(4));
        assert_eq!(starts(&mut schedule), vec![5], "d's next step still runs");
        schedule.apply(Event::Completed(5));
        schedule.apply(Event::Published(3));
        schedule.apply(Event::Completed(1));
        assert!(
            publishes(&mut schedule).is_empty(),
            "the failed Service never publishes"
        );
        assert!(schedule.finished());
    }

    #[test]
    fn a_service_handing_over_a_hostname_never_starts_when_the_one_taking_it_fails() {
        let mut schedule = Schedule::new(
            vec![
                group("api", &[], vec![Step::Parallel(vec![0])]),
                group("web", &["api"], vec![Step::Parallel(vec![1])]),
            ],
            vec![start('1'), start('1')],
        );
        assert_eq!(starts(&mut schedule), vec![0]);
        schedule.apply(Event::Failed(0));
        assert!(
            starts(&mut schedule).is_empty(),
            "web keeps the copies that publish the hostname"
        );
        assert!(schedule.finished());
        assert_eq!(
            schedule.services(),
            vec![
                ServiceOutcome {
                    service: name("api"),
                    result: ServiceResult::Old,
                },
                ServiceOutcome {
                    service: name("web"),
                    result: ServiceResult::NotStarted,
                },
            ]
        );
    }

    #[test]
    fn a_publish_only_a_gap_servers_proxy_did_not_confirm_lets_dependents_start() {
        let mut schedule = Schedule::new(
            vec![
                group("a", &[], vec![Step::Parallel(vec![0])]),
                group("b", &["a"], vec![Step::Parallel(vec![1])]),
            ],
            vec![start('1'), start('1')],
        );
        assert_eq!(starts(&mut schedule), vec![0]);
        schedule.apply(Event::Completed(0));
        assert_eq!(publishes(&mut schedule), vec![0]);
        schedule.apply(routed("a"));
        schedule.apply(Event::PublishGapped(0));
        assert_eq!(starts(&mut schedule), vec![1]);
        schedule.apply(Event::Completed(1));
        schedule.apply(Event::Published(1));
        assert!(schedule.finished());
        assert!(!schedule.failed());
        assert_eq!(
            schedule.services(),
            vec![
                ServiceOutcome {
                    service: name("a"),
                    result: ServiceResult::Unconfirmed,
                },
                ServiceOutcome {
                    service: name("b"),
                    result: ServiceResult::New,
                },
            ]
        );
    }

    #[test]
    fn a_lost_publish_fails_the_deploy() {
        let mut schedule = Schedule::new(
            vec![
                group("a", &[], vec![Step::Parallel(vec![0])]),
                group("b", &["a"], vec![Step::Parallel(vec![1])]),
            ],
            vec![start('1'), start('1')],
        );
        assert_eq!(starts(&mut schedule), vec![0]);
        schedule.apply(Event::Completed(0));
        assert_eq!(publishes(&mut schedule), vec![0]);
        schedule.apply(Event::PublishFailed(0));
        assert!(schedule.failed());
        assert!(starts(&mut schedule).is_empty());
        assert!(schedule.finished());
    }

    #[test]
    fn every_service_waits_for_the_prelude() {
        let mut schedule = Schedule::new(
            vec![
                ServiceGroup {
                    service: None,
                    dependencies: Vec::new(),
                    steps: vec![Step::Serial(0)],
                    publish: false,
                },
                group("a", &[], vec![Step::Serial(1)]),
                group("b", &[], vec![Step::Serial(2)]),
            ],
            vec![
                Task {
                    machine: machine('1'),
                    kind: TaskKind::Other,
                },
                start('1'),
                start('1'),
            ],
        );
        assert_eq!(starts(&mut schedule), vec![0]);
        assert_eq!(starts(&mut schedule), Vec::<usize>::new());
        schedule.apply(Event::Completed(0));
        assert_eq!(starts(&mut schedule), vec![1, 2]);
    }

    #[test]
    fn the_namespace_tail_waits_for_every_service() {
        let mut schedule = Schedule::new(
            vec![
                group("a", &[], vec![Step::Parallel(vec![0])]),
                ServiceGroup {
                    service: None,
                    dependencies: vec![name("a")],
                    steps: vec![Step::Serial(1)],
                    publish: false,
                },
            ],
            vec![
                start('1'),
                Task {
                    machine: machine('1'),
                    kind: TaskKind::Other,
                },
            ],
        );
        assert_eq!(starts(&mut schedule), vec![0]);
        schedule.apply(Event::Completed(0));
        schedule.apply(Event::Published(0));
        assert_eq!(starts(&mut schedule), vec![1]);
        schedule.apply(Event::Completed(1));
        assert!(schedule.finished());
    }

    #[test]
    fn a_group_without_a_service_never_publishes() {
        let mut schedule = Schedule::new(
            vec![ServiceGroup {
                service: None,
                dependencies: Vec::new(),
                steps: vec![Step::Serial(0)],
                publish: true,
            }],
            vec![start('1')],
        );
        assert_eq!(starts(&mut schedule), vec![0]);
        schedule.apply(Event::Completed(0));
        assert_eq!(publishes(&mut schedule), Vec::<usize>::new());
        assert!(schedule.finished());
    }
}
