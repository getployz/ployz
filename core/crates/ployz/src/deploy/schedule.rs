//! Pure ordering of one Deploy's Service graph.
//!
//! The executor feeds completion, publish and ack events and asks for the next
//! actions; nothing here touches a Machine, so the tests run without Docker or
//! Corrosion.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{DeployOperation, DeploymentLogId, MachineId, ServiceName, UpdateOrder};

/// New Containers one Machine starts at once for one Deploy. Containers still
/// draining from an earlier Deploy are not tasks here, so they never count.
pub(crate) const MACHINE_WINDOW: usize = 5;

/// One planned operation's place in the schedule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Task {
    pub(crate) machine: MachineId,
    /// Creates a new Container, which takes a slot in its Machine's window.
    pub(crate) starts: bool,
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
    /// Flip this Service's pointer once its steps complete.
    pub publish: bool,
    /// Deploy the Service's current Containers carry, for a first publish's `previous`.
    pub previous: Option<DeploymentLogId>,
}

/// What the executor does next.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Action {
    Start(usize),
    Publish(usize),
}

/// What the executor saw.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Event {
    Completed(usize),
    Failed(usize),
    Published(usize),
    PublishFailed(usize),
    /// `machine` routes `service` at this Deploy's pointer.
    Acked {
        service: ServiceName,
        machine: MachineId,
    },
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
    failed: bool,
}

impl ServiceGroup {
    fn is_prelude(&self) -> bool {
        self.service.is_none() && self.dependencies.is_empty()
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

    /// Groups whose own task or publish failed.
    pub(crate) fn failed_groups(&self) -> Vec<usize> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| matches!(slot.phase, Phase::Failed))
            .map(|(group, _)| group)
            .collect()
    }

    /// The operation index a group's publish reports under.
    pub(crate) fn last_task(&self, group: usize) -> usize {
        self.group(group)
            .steps
            .iter()
            .flat_map(step_tasks)
            .max()
            .expect("a publishing group has at least one task")
    }

    /// Nothing runs and nothing more can start.
    pub(crate) fn finished(&self) -> bool {
        self.slots.iter().all(|slot| match slot.phase {
            Phase::Done | Phase::Failed => true,
            Phase::Waiting => self.failed,
            Phase::Active { .. } | Phase::Publishing => false,
        })
    }

    /// Tasks whose only block is a missing ack, with the Service each waits on.
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
                if let Some(service) = self.missing_ack(group, task) {
                    blocked.push((task, service));
                }
            }
        }
        blocked
    }

    pub(crate) fn apply(&mut self, event: Event) {
        match event {
            Event::Completed(task) => {
                let group = self.group_of(task);
                if let Phase::Active { running, .. } = &mut self.slot_mut(group).phase {
                    running.remove(&task);
                }
                self.advance(group);
            }
            Event::Failed(task) => {
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
            Event::Acked { service, machine } => {
                self.acked.insert((service, machine));
            }
        }
    }

    /// Starts and publishes that may begin now, in operation order.
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
                let Task { machine, starts } = self.task(task);
                let window = windows.entry(*machine).or_insert(0);
                let room = !*starts || *window < MACHINE_WINDOW;
                if room && self.missing_ack(&self.slot(group).group, task).is_none() {
                    if *starts {
                        *window += 1;
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

    fn windows(&self) -> BTreeMap<MachineId, usize> {
        let mut windows = BTreeMap::new();
        for slot in &self.slots {
            if let Phase::Active { running, .. } = &slot.phase {
                for task in running.iter().map(|task| self.task(*task)) {
                    if task.starts {
                        *windows.entry(task.machine).or_insert(0) += 1;
                    }
                }
            }
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
            None if group.publish => Phase::Publishing,
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

    /// The published dependency `task`'s Machine has not acked yet, if any.
    /// Only a new Container needs its dependencies routable where it runs.
    fn missing_ack(&self, group: &ServiceGroup, task: usize) -> Option<ServiceName> {
        let Task { machine, starts } = self.task(task);
        if !starts {
            return None;
        }
        group
            .dependencies
            .iter()
            .find(|dependency| {
                self.published.contains(*dependency)
                    && !self.acked.contains(&((*dependency).clone(), *machine))
            })
            .cloned()
    }
}

/// A new Container of the Service starts here, so the task takes a window
/// slot and the Deploy removes the Container if the Service never switches.
pub(crate) fn starts_container(operation: &DeployOperation) -> bool {
    match operation {
        DeployOperation::RunContainer { .. } => true,
        DeployOperation::ReplaceContainer(replacement) => {
            replacement.spec.update.order != UpdateOrder::StopFirst
        }
        DeployOperation::PrepareVolumes { .. }
        | DeployOperation::WaitHealthy { .. }
        | DeployOperation::StopContainer { .. }
        | DeployOperation::RemoveContainer { .. }
        | DeployOperation::StopHook { .. }
        | DeployOperation::RunHook { .. }
        | DeployOperation::RemoveVolume { .. } => false,
    }
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
            previous: None,
        }
    }

    fn start(machine: char) -> Task {
        Task {
            machine: self::machine(machine),
            starts: true,
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
        let tasks = (0..12)
            .map(|index| {
                if index % 2 == 0 {
                    start('1')
                } else {
                    start('2')
                }
            })
            .collect();
        let mut schedule = Schedule::new(vec![group("api", &[], steps)], tasks);
        let first = starts(&mut schedule);
        assert_eq!(first, vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        let draining = Task {
            machine: machine('1'),
            starts: false,
        };
        assert!(
            !draining.starts,
            "a draining copy is never a task of this Deploy"
        );
    }

    #[test]
    fn serial_steps_run_one_at_a_time_and_a_non_start_takes_no_window_slot() {
        let steps = vec![Step::Serial(0), Step::Serial(1), Step::Parallel(vec![2, 3])];
        let tasks = vec![
            Task {
                machine: machine('1'),
                starts: false,
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
                    previous: None,
                },
                group("a", &[], vec![Step::Serial(1)]),
                group("b", &[], vec![Step::Serial(2)]),
            ],
            vec![
                Task {
                    machine: machine('1'),
                    starts: false,
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
                    previous: None,
                },
            ],
            vec![
                start('1'),
                Task {
                    machine: machine('1'),
                    starts: false,
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
}
