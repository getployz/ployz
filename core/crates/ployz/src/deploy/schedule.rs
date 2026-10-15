//! Pure ordering of one Deploy's Service graph: events in, next actions out,
//! and nothing here touches a Machine.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    ContainerId, DeployOperation, MachineId, RemoveContainerPurpose, ServiceName, ServiceOutcome,
    ServiceResult,
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
    /// until its Machine routes every changed dependency.
    Start,
    /// Runs a hook against the dependencies, so it waits for its Machine's
    /// acks like a start but takes no window slot.
    Hook,
    /// Removes a copy the Service's previous pointer named, so it waits until
    /// every Machine routes the new pointer.
    Unroute,
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
    /// Current Containers of the Service no operation of this Deploy names;
    /// the flip keeps pointing at them.
    pub kept: Vec<ContainerId>,
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
    /// `machine` routes `service` at this Deploy's pointer.
    Acked {
        service: ServiceName,
        machine: MachineId,
    },
    /// Every Machine routes `service` at this Deploy's pointer.
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
    acked: BTreeSet<(ServiceName, MachineId)>,
    routed: BTreeSet<ServiceName>,
    /// Starts dispatched and not yet returned, whatever their group's phase.
    starting: BTreeSet<usize>,
    unconfirmed: BTreeSet<usize>,
    failed: bool,
}

impl ServiceGroup {
    fn is_prelude(&self) -> bool {
        self.service.is_none() && self.dependencies.is_empty()
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
            acked: BTreeSet::new(),
            routed: BTreeSet::new(),
            starting: BTreeSet::new(),
            unconfirmed: BTreeSet::new(),
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

    /// How each Service this Deploy changed ended. A Service the plan leaves
    /// unchanged keeps its group for ordering but owns no operation, so it has
    /// no result.
    pub(crate) fn services(&self) -> Vec<ServiceOutcome> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, Slot { group, .. })| group.publish || !group.steps.is_empty())
            .filter_map(|(index, Slot { group, phase })| {
                let result = match phase {
                    _ if self.unconfirmed.contains(&index) => ServiceResult::Unconfirmed,
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

    pub(crate) fn first_task(&self, group: usize) -> usize {
        self.tasks_of(group)
            .min()
            .expect("a publishing group has a task of its own or after it")
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

    pub(crate) fn blocked_on_acks(&self) -> Vec<(usize, ServiceName)> {
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
                    self.missing_acks(group, task)
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
            Event::Acked { service, machine } => {
                self.acked.insert((service, machine));
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
                        .missing_acks(&self.slot(group).group, task)
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
        // An ack admits one round. A same-Deploy amendment can change what a
        // row names, so a later round reads the acks again.
        self.acked.clear();
        self.routed.clear();
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

    fn missing_acks<'a>(
        &'a self,
        group: &'a ServiceGroup,
        task: usize,
    ) -> impl Iterator<Item = &'a ServiceName> {
        let Task { machine, kind } = self.task(task);
        group.dependencies.iter().filter(move |dependency| {
            self.published.contains(*dependency)
                && match kind {
                    TaskKind::Start | TaskKind::Hook => {
                        !self.acked.contains(&((*dependency).clone(), *machine))
                    }
                    TaskKind::Unroute => !self.routed.contains(*dependency),
                    TaskKind::Other => false,
                }
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
            kept: Vec::new(),
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

    fn ack(service: &str, machine: char) -> Event {
        Event::Acked {
            service: name(service),
            machine: self::machine(machine),
        }
    }

    #[test]
    fn a_chain_starts_each_service_after_its_dependency_publishes_and_its_machine_acks() {
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
            "api waits on Machine 1's ack"
        );
        assert_eq!(schedule.blocked_on_acks(), vec![(1, name("db"))]);
        schedule.apply(ack("db", '1'));
        assert_eq!(starts(&mut schedule), vec![1]);
        schedule.apply(Event::Completed(1));
        schedule.apply(Event::Published(1));
        schedule.apply(ack("api", '1'));
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
        schedule.apply(ack("db", '1'));
        assert_eq!(
            starts(&mut schedule),
            vec![1],
            "queue's Machine 2 has not acked db"
        );
        schedule.apply(ack("db", '2'));
        assert_eq!(starts(&mut schedule), vec![2]);
        schedule.apply(Event::Completed(1));
        schedule.apply(Event::Published(1));
        schedule.apply(ack("cache", '2'));
        assert!(starts(&mut schedule).is_empty(), "queue has not published");
        schedule.apply(Event::Completed(2));
        schedule.apply(Event::Published(2));
        assert!(
            starts(&mut schedule).is_empty(),
            "Machine 2 has not acked queue"
        );
        schedule.apply(ack("cache", '2'));
        schedule.apply(ack("queue", '2'));
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
        assert!(schedule.blocked_on_acks().is_empty());
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
    fn an_ack_admits_one_round_and_a_later_start_reads_it_again() {
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
        schedule.apply(ack("db", '1'));
        assert_eq!(starts(&mut schedule), vec![1]);
        schedule.apply(Event::Completed(1));
        assert!(
            starts(&mut schedule).is_empty(),
            "an amended db row may not be routed yet"
        );
        assert_eq!(schedule.blocked_on_acks(), vec![(2, name("db"))]);
        schedule.apply(ack("db", '1'));
        assert_eq!(starts(&mut schedule), vec![2]);
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
            schedule.blocked_on_acks(),
            vec![(2, name("db")), (2, name("cache"))]
        );
        for (_, service) in schedule.blocked_on_acks() {
            schedule.apply(Event::Acked {
                service,
                machine: machine('1'),
            });
        }
        assert_eq!(starts(&mut schedule), vec![2]);
    }

    #[test]
    fn a_hook_waits_for_its_machine_to_route_a_changed_dependency() {
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
        assert_eq!(schedule.blocked_on_acks(), vec![(1, name("db"))]);
        schedule.apply(ack("db", '2'));
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
        schedule.apply(ack("b", '2'));
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
                    kept: Vec::new(),
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
                    kept: Vec::new(),
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
                kept: Vec::new(),
            }],
            vec![start('1')],
        );
        assert_eq!(starts(&mut schedule), vec![0]);
        schedule.apply(Event::Completed(0));
        assert_eq!(publishes(&mut schedule), Vec::<usize>::new());
        assert!(schedule.finished());
    }
}
