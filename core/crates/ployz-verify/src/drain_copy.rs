//! A drain's start of its new copy on the destination Server, from the request to the
//! cleanup: the handler that serves it, other callers of ensure.v1 on the same Server, and
//! the drain that sends it, resends it, cancels, reads the answer and cleans up.
//!
//! The properties:
//!
//! - **The reply is ours.** Every `Present` answer names the Container the handler made or
//!   adopted for this request, with the drain's spec and the pinned image.
//! - **No wrong start.** The handler never starts a Container that runs another image.
//! - **No foreign removal.** Every Container a removal the drain caused took was made by one
//!   of the move's own sends: the cleanup's Gone by key, and the handler's own recovery of a
//!   half-made Container while it reads the slot.
//! - **Nothing of ours is left.** A drain that ends without moving leaves no Container any
//!   of its sends made, in any slot, unless that Container was made after its Gone.
//! - **The report is true.** A drain that reports its replacement removed leaves no
//!   Container any of its sends made, then or later.
//!
//! The actors, each step its own transition:
//!
//! - the handler, `MachineService::ensure` (ployzd/src/machine_api/local.rs), then
//!   `converge_now` and `create_and_start` (ployzd/src/machine/local_machine/container.rs):
//!   it reads the slot its request names and plans, recovers a half-made Container of that
//!   key (`matching_creation`, ployzd/src/docker/lifecycle.rs), creates or adopts the
//!   Container a name conflict finds, checks the image by ID (`image_differs`) before it
//!   starts or answers, starts, then reads the Container it acted on and answers. Any of
//!   those steps may end in an error instead. One request, and one resend after a lost
//!   answer or a send the entry couldn't dial (`RestartTolerant`,
//!   ployz/src/deploy/exec/transport.rs), which the entry may refuse to dial again for;
//! - the planter, an honest other caller (an older CLI's move of the same copy, a
//!   hand-written ensure.v1, a second drain), acting at most twice in the slot `move:{old}`
//!   or in a second drain's minted slot: it creates a Container of either spec and either
//!   image, half-made or started, stops one, or removes one by key. It never acts in this
//!   move's minted slot: only a forger can name it, which is an accepted residual;
//! - a Machine restore, once: it gives the move's own Container a new ID under the same key
//!   and spec, and the move remains its creator;
//! - the drain, `move_container` (ployz/src/deploy/exec/start.rs): it mints its slot once,
//!   sends, may cancel at any point the resend wait included, waits out health, and after
//!   any end but a healthy Container sends one Gone by its slot's key. It reports the
//!   replacement removed only when that Gone left the slot empty and no send of it went out
//!   without an answer (`Outstanding`).
//!
//! The model's Gone is one atomic step. The code resends it while the Machine restarts, and
//! a resend by the same key removes nothing the first did not, so the resends come to this
//! one removal. The atomic step always reads its slot back empty; the code reports a Gone
//! that reads back a Container as not removed, which can only make the report more careful.
//!
//! The drain's cleanup has no branch, so the model calls no production decision. The key
//! choice is a transition, and `Key` switches it back to the designs it replaced.
//!
//! Bounds: four slots, Container IDs up to 5, two specs, two images, one resend.
//!
//! Out of scope: a second destination, rotation and the steps after a healthy answer
//! (`relocate`, `bring_in`, `wait_listed`, `retire_old`), and who signed the spec.

use std::fmt::Write as _;

use stateright::{Model, Property};

/// A slot on the destination, named by its creation key. Docker names a Container by its
/// key, so a slot holds at most one.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Slot {
    /// `move:{old}:{nonce}`, which this move minted.
    Minted,
    /// A key the resend minted afresh, under `Key::PerSend` only.
    Resent,
    /// `move:{old}`, which an older CLI or anyone reading the old copy's ID can name.
    Shared,
    /// A second drain's minted slot.
    Second,
}

const SLOTS: [Slot; 4] = [Slot::Minted, Slot::Resent, Slot::Shared, Slot::Second];

/// The spec a Container was made from: the drain's own, or another.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Spec {
    Drained,
    Other,
}

/// The image a Container runs: the pinned image ID, or another the tag names now.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Image {
    Pinned,
    Other,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Maker {
    /// One of this move's sends, or a restore of what one made.
    Move,
    Planter,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Container {
    id: u8,
    spec: Spec,
    image: Image,
    running: bool,
    /// Created by Docker, its spec never written: the next create of its key recovers it.
    half: bool,
    made_by: Maker,
}

const MAX_ID: u8 = 5;

/// Why the handler refused.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Refusal {
    /// `ContainerRefusalReason::SlotOccupied`: the slot holds another spec's Container.
    SlotOccupied,
    /// `ContainerRefusalReason::ImageDiffers`: the Container runs another image.
    ImageDiffers,
}

/// What came back to the drain, as `EnsuredContainer` reads.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Answer {
    /// `Ensured { now: Present }`, with what the final read saw.
    Present {
        id: u8,
        spec: Spec,
        image: Image,
    },
    /// `Ensured { now: Absent }`.
    Absent,
    Refused(Refusal),
    /// An error in place of an answer, whatever it says the Machine did.
    Error,
}

/// What the handler does next, as `converge_now` and `create_and_start` order it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Step {
    /// Waits for the drain's request.
    Idle,
    /// The request is on its way and not yet admitted: a Gone by key doesn't wait for it.
    Plan,
    /// `create_container_admitted`: creates the Container, or adopts the one a name
    /// conflict finds when it was made from the same spec.
    Create,
    /// `image_differs`, before it starts or the handler answers.
    Check { id: u8, then: Then },
    /// `start_checked`. `checked` says whether the image was checked already.
    Start { id: u8, checked: bool },
    /// The final read and the answer.
    Answer,
    /// The answer was lost, or the entry couldn't dial: `RestartTolerant` waits to resend.
    Backoff,
    /// Answered, or nothing left to send.
    Done,
}

/// The step after a passed image check.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Then {
    Answer,
    Start,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Handler {
    step: Step,
    /// 1 for the request, 2 for the resend.
    run: u8,
    /// The slot this run's request names.
    slot: Slot,
    /// The Container this run made or adopted, or `Start` planned (`acted`).
    acted: Option<u8>,
    refusal: Option<Refusal>,
    erred: bool,
    /// The Container the first run made, so a resend that adopts it shows.
    first_made: Option<u8>,
    /// What the plan read when it found the request done, which `Done` answers.
    planned: Option<Container>,
}

/// Where the drain is.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Phase {
    Ready,
    Waiting,
    /// No answer: cancelled, or every send lost.
    Unanswered,
    Answered,
    /// Healthy: the move is done.
    Moved,
    /// Cleaned up.
    Finished,
}

/// The drain's Gone by key: the slot it named, and the Container it took.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Gone {
    slot: Slot,
    took: Option<u8>,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Drain {
    phase: Phase,
    answer: Option<Answer>,
    /// The answered Container failed health, or the drain was cancelled after the answer.
    failed: bool,
    /// A send went out and lost its answer, or was cut off while out (`Outstanding`).
    outstanding: bool,
    /// Cancelled while it waited to resend.
    cancelled_in_wait: bool,
    gone: Option<Gone>,
    /// What the report says of the replacement.
    reported_removed: bool,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    slots: [Option<Container>; 4],
    next_id: u8,
    handler: Handler,
    planter_acts: u8,
    /// The ID a restore gave the move's Container.
    restored: Option<u8>,
    drain: Drain,
    /// The handler started a Container that runs another image.
    wrong_start: bool,
    /// A removal the drain caused took a Container none of its sends made.
    foreign_removed: bool,
    /// Bit `id` is set for a Container the handler made after the drain's Gone.
    late: u8,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum PlanterAct {
    Create {
        spec: Spec,
        image: Image,
        half: bool,
    },
    Stop,
    Gone,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    Send,
    /// The drain is cancelled before any answer, the resend wait included.
    Cancel,
    Handler,
    /// The request fails with an error at this step, before or after anything was made.
    Error,
    /// The entry can't dial the destination, so the request never reaches the handler.
    Undial,
    /// `RestartTolerant` resends.
    Resend,
    /// The entry refuses to dial again, so the resend never leaves.
    RefuseRedial,
    /// The answer reaches the drain.
    Deliver,
    /// The answer is lost on the way.
    Lose,
    /// The answered Container serves healthy.
    Healthy,
    /// The answered Container fails health, or the drain is cancelled after the answer.
    Fails,
    /// The drain's one Gone by key.
    Cleanup,
    /// A Machine restore gives the move's Container a new ID under its key.
    Restore,
    Planter(Slot, PlanterAct),
}

/// The code as it ships, or one part of it switched back to an earlier design.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
struct Switches {
    key: Key,
    reply: ReplyBy,
    check: Check,
}

/// Which slot each send of the move names.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
enum Key {
    /// Minted once per move, so every send and the Gone name the one slot.
    #[default]
    Minted,
    /// `move:{old}`, as before this change: another caller can name it.
    Shared,
    /// Minted afresh for each send, so the Gone names only the last send's slot.
    PerSend,
}

/// What the final read names.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
enum ReplyBy {
    /// `container_now(&ContainerLocator::Id(acted))`.
    #[default]
    Acted,
    /// The slot, as the read before the pinned route did.
    Slot,
}

/// When the image is checked by ID.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
enum Check {
    /// Before any start or answer.
    #[default]
    BeforeStart,
    /// Never.
    None,
    /// After the start.
    AfterStart,
}

#[derive(Clone, Debug)]
struct DrainCopy {
    switches: Switches,
}

const REPLY_IS_OURS: &str =
    "every Present answer names our Container, from our spec, on the pinned image";
const NO_WRONG_START: &str = "the handler never starts a Container that runs another image";
const NO_FOREIGN_REMOVAL: &str =
    "every Container a removal the drain caused took was made by one of the move's sends";
const NOTHING_LEFT: &str = "a drain that ends without moving leaves no Container its sends made, unless made after its Gone";
const REPORT_IS_TRUE: &str =
    "a drain that reports its replacement removed leaves no Container its sends made";

impl State {
    fn slot(&self, slot: Slot) -> Option<Container> {
        let [minted, resent, shared, second] = self.slots;
        match slot {
            Slot::Minted => minted,
            Slot::Resent => resent,
            Slot::Shared => shared,
            Slot::Second => second,
        }
    }

    fn slot_mut(&mut self, slot: Slot) -> &mut Option<Container> {
        let [minted, resent, shared, second] = &mut self.slots;
        match slot {
            Slot::Minted => minted,
            Slot::Resent => resent,
            Slot::Shared => shared,
            Slot::Second => second,
        }
    }

    /// The Container `id` in `slot`, if it is still there.
    fn container(&self, slot: Slot, id: u8) -> Option<Container> {
        self.slot(slot).filter(|container| container.id == id)
    }

    fn make(&mut self, slot: Slot, container: Container) -> Option<u8> {
        if self.next_id > MAX_ID {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        *self.slot_mut(slot) = Some(Container { id, ..container });
        Some(id)
    }

    /// Remove what `slot` holds for a removal the drain caused.
    fn remove(&mut self, slot: Slot) -> Option<u8> {
        let taken = self.slot_mut(slot).take()?;
        if taken.made_by != Maker::Move {
            self.foreign_removed = true;
        }
        Some(taken.id)
    }

    /// The Containers the move's sends made that are still on the destination.
    fn ours(&self) -> impl Iterator<Item = Container> + '_ {
        self.slots
            .iter()
            .flatten()
            .copied()
            .filter(|container| container.made_by == Maker::Move)
    }
}

impl DrainCopy {
    /// The slot run `run` of the move names.
    fn slot_of(&self, run: u8) -> Slot {
        match (self.switches.key, run) {
            (Key::Minted, _) | (Key::PerSend, 1) => Slot::Minted,
            (Key::PerSend, _) => Slot::Resent,
            (Key::Shared, _) => Slot::Shared,
        }
    }

    /// One handler step from `state`.
    fn handle(&self, state: &mut State) {
        let target = state.handler.slot;
        match state.handler.step {
            Step::Idle | Step::Answer | Step::Backoff | Step::Done => {
                unreachable!("not a handler step")
            }
            Step::Plan => {
                // `matching_creation` recovers a half-made Container of this key: it removes it
                // and creates afresh.
                if state.slot(target).is_some_and(|container| container.half) {
                    state.remove(target);
                }
                let occupant = state.slot(target);
                let handler = &mut state.handler;
                handler.step = match occupant {
                    None => Step::Create,
                    Some(occupant) if occupant.spec == Spec::Other => {
                        handler.refusal = Some(Refusal::SlotOccupied);
                        Step::Answer
                    }
                    Some(occupant) => {
                        handler.acted = Some(occupant.id);
                        let then = if occupant.running {
                            handler.planned = Some(occupant);
                            Then::Answer
                        } else {
                            Then::Start
                        };
                        self.after_plan(occupant.id, then)
                    }
                };
            }
            Step::Create => match state.slot(target) {
                None => {
                    let made = state.make(
                        target,
                        Container {
                            id: 0,
                            spec: Spec::Drained,
                            image: Image::Pinned,
                            running: false,
                            half: false,
                            made_by: Maker::Move,
                        },
                    );
                    if let Some(id) = made
                        && state.drain.gone.is_some()
                    {
                        state.late |= 1 << id;
                    }
                    let handler = &mut state.handler;
                    if handler.run == 1 {
                        handler.first_made = made;
                    }
                    match made {
                        Some(id) => {
                            handler.acted = Some(id);
                            handler.step = self.after_plan(id, Then::Start);
                        }
                        None => {
                            handler.erred = true;
                            handler.step = Step::Answer;
                        }
                    }
                }
                // `create_refusal`: nothing was made, so the answer reads the slot.
                Some(occupant) if occupant.spec == Spec::Other => {
                    state.handler.refusal = Some(Refusal::SlotOccupied);
                    state.handler.step = Step::Answer;
                }
                Some(occupant) => {
                    state.handler.acted = Some(occupant.id);
                    state.handler.step = self.after_plan(occupant.id, Then::Start);
                }
            },
            Step::Check { id, then } => {
                let checked = state.container(target, id);
                let handler = &mut state.handler;
                handler.step = match checked {
                    None => {
                        handler.erred = true;
                        Step::Answer
                    }
                    Some(container) if container.image != Image::Pinned => {
                        handler.refusal = Some(Refusal::ImageDiffers);
                        Step::Answer
                    }
                    Some(_) => match then {
                        Then::Answer => Step::Answer,
                        Then::Start => Step::Start { id, checked: true },
                    },
                };
            }
            Step::Start { id, checked } => match state.container(target, id) {
                None => {
                    state.handler.erred = true;
                    state.handler.step = Step::Answer;
                }
                Some(container) => {
                    // Starting a running Container changes nothing.
                    if container.image != Image::Pinned && !container.running {
                        state.wrong_start = true;
                    }
                    if let Some(slot) = state.slot_mut(target) {
                        slot.running = true;
                    }
                    state.handler.step = if !checked && self.switches.check == Check::AfterStart {
                        Step::Check {
                            id,
                            then: Then::Answer,
                        }
                    } else {
                        Step::Answer
                    };
                }
            },
        }
    }

    /// The step after the plan picks Container `id`: its image check, unless the switch
    /// moves or drops it.
    fn after_plan(&self, id: u8, then: Then) -> Step {
        match (self.switches.check, then) {
            (Check::BeforeStart, _) | (Check::AfterStart, Then::Answer) => Step::Check { id, then },
            (Check::None, Then::Answer) => Step::Answer,
            (Check::None | Check::AfterStart, Then::Start) => Step::Start { id, checked: false },
        }
    }

    /// The final read and its answer.
    fn answer(&self, state: &State) -> Answer {
        let handler = state.handler;
        if handler.erred {
            return Answer::Error;
        }
        if let Some(refusal) = handler.refusal {
            return Answer::Refused(refusal);
        }
        let read = match (handler.planned, self.switches.reply) {
            (Some(planned), _) => Some(planned),
            (None, ReplyBy::Acted) => handler
                .acted
                .and_then(|id| state.container(handler.slot, id)),
            (None, ReplyBy::Slot) => state.slot(handler.slot),
        };
        read.map_or(Answer::Absent, |container| Answer::Present {
            id: container.id,
            spec: container.spec,
            image: container.image,
        })
    }

    /// The drain hears `answer`, or nothing.
    fn hear(state: &mut State, answer: Option<Answer>) {
        if state.drain.phase != Phase::Waiting {
            return;
        }
        state.drain.answer = answer;
        state.drain.phase = match answer {
            None => Phase::Unanswered,
            Some(_) => Phase::Answered,
        };
    }

    /// Whether the drain's next step is its Gone: after any end but a healthy Container.
    /// The daemon's Gone by key waits for admitted work on that key, so it runs only while
    /// no request of the move is past its admission.
    fn cleans_up(&self, state: &State) -> bool {
        let drain = state.drain;
        let due = match drain.phase {
            Phase::Unanswered => true,
            Phase::Answered => match drain.answer {
                Some(Answer::Present { .. }) => drain.failed,
                _ => true,
            },
            Phase::Ready | Phase::Waiting | Phase::Moved | Phase::Finished => false,
        };
        let admitted = matches!(
            state.handler.step,
            Step::Create | Step::Check { .. } | Step::Start { .. } | Step::Answer
        );
        due && !admitted
    }
}

impl Model for DrainCopy {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            slots: [None; 4],
            next_id: 1,
            handler: Handler {
                step: Step::Idle,
                run: 1,
                slot: self.slot_of(1),
                acted: None,
                refusal: None,
                erred: false,
                first_made: None,
                planned: None,
            },
            planter_acts: 0,
            restored: None,
            drain: Drain {
                phase: Phase::Ready,
                answer: None,
                failed: false,
                outstanding: false,
                cancelled_in_wait: false,
                gone: None,
                reported_removed: false,
            },
            wrong_start: false,
            foreign_removed: false,
            late: 0,
        }]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        match state.drain.phase {
            Phase::Ready => actions.push(Action::Send),
            Phase::Waiting => actions.push(Action::Cancel),
            Phase::Answered => {
                if matches!(state.drain.answer, Some(Answer::Present { .. })) && !state.drain.failed
                {
                    actions.extend([Action::Healthy, Action::Fails]);
                }
            }
            Phase::Unanswered | Phase::Moved | Phase::Finished => {}
        }
        if self.cleans_up(state) {
            actions.push(Action::Cleanup);
        }
        match state.handler.step {
            Step::Idle | Step::Done => {}
            Step::Answer => actions.extend([Action::Deliver, Action::Lose]),
            Step::Backoff => {
                if state.drain.phase == Phase::Waiting {
                    actions.extend([Action::Resend, Action::RefuseRedial]);
                }
            }
            Step::Plan => {
                actions.extend([Action::Handler, Action::Error]);
                if state.handler.run == 1 {
                    actions.push(Action::Undial);
                }
            }
            Step::Create | Step::Check { .. } | Step::Start { .. } => {
                actions.extend([Action::Handler, Action::Error]);
            }
        }
        if state.restored.is_none()
            && state.next_id <= MAX_ID
            && [Slot::Minted, Slot::Resent]
                .iter()
                .any(|slot| state.slot(*slot).is_some_and(|c| c.made_by == Maker::Move))
        {
            actions.push(Action::Restore);
        }
        if state.planter_acts < 2 {
            for slot in [Slot::Shared, Slot::Second] {
                match state.slot(slot) {
                    None if state.next_id <= MAX_ID => {
                        for spec in [Spec::Drained, Spec::Other] {
                            for image in [Image::Pinned, Image::Other] {
                                for half in [false, true] {
                                    actions.push(Action::Planter(
                                        slot,
                                        PlanterAct::Create { spec, image, half },
                                    ));
                                }
                            }
                        }
                    }
                    None => {}
                    Some(container) => {
                        if container.running {
                            actions.push(Action::Planter(slot, PlanterAct::Stop));
                        }
                        actions.push(Action::Planter(slot, PlanterAct::Gone));
                    }
                }
            }
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = *last;
        match action {
            Action::Send => {
                state.drain.phase = Phase::Waiting;
                state.handler.step = Step::Plan;
            }
            Action::Cancel => {
                let handler = &mut state.handler;
                if handler.step == Step::Backoff {
                    state.drain.cancelled_in_wait = true;
                    handler.step = Step::Done;
                } else if handler.step != Step::Done {
                    // Cut off while out: the request goes on without the drain.
                    state.drain.outstanding = true;
                }
                state.drain.phase = Phase::Unanswered;
            }
            Action::Handler => self.handle(&mut state),
            Action::Error => {
                state.handler.erred = true;
                state.handler.step = Step::Answer;
            }
            // The request never reached the handler, but the client can't tell it from a
            // lost one (`target_unanswered`).
            Action::Undial => {
                state.handler.step = Step::Backoff;
                state.drain.outstanding = true;
            }
            Action::Resend => {
                state.handler = Handler {
                    step: Step::Plan,
                    run: 2,
                    slot: self.slot_of(2),
                    acted: None,
                    refusal: None,
                    erred: false,
                    first_made: state.handler.first_made,
                    planned: None,
                };
            }
            Action::RefuseRedial => {
                state.handler.step = Step::Done;
                Self::hear(&mut state, Some(Answer::Error));
            }
            Action::Deliver | Action::Lose => {
                let answer = self.answer(&state);
                if action == Action::Lose && state.handler.run == 1 {
                    state.handler.step = Step::Backoff;
                    state.drain.outstanding = true;
                } else {
                    state.handler.step = Step::Done;
                    if action == Action::Lose {
                        state.drain.outstanding = true;
                    }
                    Self::hear(&mut state, (action == Action::Deliver).then_some(answer));
                }
            }
            Action::Healthy => state.drain.phase = Phase::Moved,
            Action::Fails => state.drain.failed = true,
            Action::Cleanup => {
                let slot = self.slot_of(state.handler.run);
                let took = state.remove(slot);
                state.drain.gone = Some(Gone { slot, took });
                state.drain.reported_removed = !state.drain.outstanding;
                state.drain.phase = Phase::Finished;
            }
            Action::Restore => {
                let slot = [Slot::Minted, Slot::Resent]
                    .into_iter()
                    .find(|slot| state.slot(*slot).is_some_and(|c| c.made_by == Maker::Move))
                    .expect("a Container of the move to restore");
                let restored = state.slot(slot).expect("checked above");
                state.restored = state.make(slot, restored);
                // The restored Container is the same creation: one made after the Gone still is.
                if let Some(id) = state.restored
                    && state.late & (1 << restored.id) != 0
                {
                    state.late |= 1 << id;
                }
            }
            Action::Planter(slot, act) => {
                state.planter_acts += 1;
                match act {
                    PlanterAct::Create { spec, image, half } => {
                        state.make(
                            slot,
                            Container {
                                id: 0,
                                spec,
                                image,
                                running: !half,
                                half,
                                made_by: Maker::Planter,
                            },
                        )?;
                    }
                    PlanterAct::Stop => {
                        if let Some(container) = state.slot_mut(slot) {
                            container.running = false;
                        }
                    }
                    PlanterAct::Gone => *state.slot_mut(slot) = None,
                }
            }
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(REPLY_IS_OURS, |_, state: &State| match state.drain.answer {
                Some(Answer::Present { id, spec, image }) => {
                    spec == Spec::Drained
                        && image == Image::Pinned
                        // The answer of the run that served it: the last one.
                        && state.handler.acted == Some(id)
                }
                _ => true,
            }),
            Property::always(NO_WRONG_START, |_, state: &State| !state.wrong_start),
            Property::always(NO_FOREIGN_REMOVAL, |_, state: &State| {
                !state.foreign_removed
            }),
            Property::always(NOTHING_LEFT, |_, state: &State| {
                state.drain.phase != Phase::Finished
                    || state
                        .ours()
                        .all(|container| state.late & (1 << container.id) != 0)
            }),
            Property::always(REPORT_IS_TRUE, |_, state: &State| {
                !state.drain.reported_removed || state.ours().next().is_none()
            }),
            Property::sometimes(
                "a fresh answer names the Container just made",
                |_, state: &State| {
                    matches!(state.drain.answer, Some(Answer::Present { id, .. })
                        if state.handler.run == 1 && state.handler.first_made == Some(id))
                },
            ),
            Property::sometimes(
                "a resend adopts what its lost send made, and the Gone removes it",
                |_, state: &State| {
                    state.handler.run == 2
                        && state.handler.first_made.is_some()
                        && state.drain.gone.map(|gone| gone.took) == Some(state.handler.first_made)
                },
            ),
            Property::sometimes(
                "a cancel in the resend wait leaves another caller's Container in move:{old}",
                |_, state: &State| {
                    state.drain.cancelled_in_wait
                        && state.drain.phase == Phase::Finished
                        && state
                            .slot(Slot::Shared)
                            .is_some_and(|y| y.made_by == Maker::Planter)
                },
            ),
            Property::sometimes(
                "the Gone removes the move's Container a restore gave a new ID",
                |_, state: &State| {
                    state.restored.is_some()
                        && state.drain.gone.and_then(|gone| gone.took) == state.restored
                },
            ),
            Property::sometimes(
                "a send still out makes its Container after the Gone",
                |_, state: &State| state.late != 0,
            ),
            Property::sometimes("a drain reports its replacement removed", |_, s: &State| {
                s.drain.reported_removed && s.drain.gone.is_some_and(|gone| gone.took.is_some())
            }),
        ]
    }
}

/// `path` in the code's words: each step, then the slots and what the drain holds.
fn story(path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    let mut led_by: Option<(Action, State)> = None;
    for (n, (state, action)) in path.iter().enumerate() {
        let what = match led_by {
            None => "start".to_owned(),
            Some((action, before)) => reads_as(action, &before),
        };
        led_by = action.map(|action| (action, *state));
        let slots = SLOTS
            .iter()
            .filter_map(|slot| {
                state.slot(*slot).map(|c| {
                    format!(
                        "{slot:?}: #{} {:?} spec on {:?} image, {}{}, made by {:?}",
                        c.id,
                        c.spec,
                        c.image,
                        if c.running { "running" } else { "not running" },
                        if c.half { ", half-made" } else { "" },
                        c.made_by
                    )
                })
            })
            .collect::<Vec<_>>();
        let slots = if slots.is_empty() {
            "all empty".to_owned()
        } else {
            slots.join("; ")
        };
        let _ = writeln!(
            story,
            "{n:2}. {what}\n    slots: {slots}\n    handler run {} on {:?} at {:?}, acted {:?}; drain {:?}, answer {:?}, outstanding {}, gone {:?}",
            state.handler.run,
            state.handler.slot,
            state.handler.step,
            state.handler.acted,
            state.drain.phase,
            state.drain.answer,
            state.drain.outstanding,
            state.drain.gone,
        );
    }
    story
}

fn reads_as(action: Action, before: &State) -> String {
    match action {
        Action::Send => "drain: move_container sends the start (ensure_new)".to_owned(),
        Action::Cancel => "drain: cancelled before any answer".to_owned(),
        Action::Error => "handler: the request fails with an error".to_owned(),
        Action::Undial => "entry: can't dial the destination".to_owned(),
        Action::Resend => "drain: RestartTolerant resends".to_owned(),
        Action::RefuseRedial => "drain: the entry refuses to dial again for the resend".to_owned(),
        Action::Handler => match before.handler.step {
            Step::Plan => "handler: converge_now reads the slot and plans".to_owned(),
            Step::Create => {
                "handler: create_and_start creates, or adopts the Container a name conflict finds"
                    .to_owned()
            }
            Step::Check { id, .. } => format!("handler: image_differs reads #{id}'s image"),
            Step::Start { id, .. } => format!("handler: start_checked starts #{id}"),
            Step::Idle | Step::Answer | Step::Backoff | Step::Done => {
                unreachable!("not a handler step")
            }
        },
        Action::Deliver => "handler: the final read, and its answer reaches the drain".to_owned(),
        Action::Lose => "handler: the final read, and its answer is lost".to_owned(),
        Action::Healthy => "drain: the answered Container serves healthy".to_owned(),
        Action::Fails => {
            "drain: the answered Container fails health, or a cancel comes after the answer"
                .to_owned()
        }
        Action::Cleanup => "drain: one Gone by its slot's key".to_owned(),
        Action::Restore => "machine: a restore gives the move's Container a new ID".to_owned(),
        Action::Planter(slot, act) => format!("planter: {act:?} in {slot:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stateright::Checker;

    use crate::harness::{Narrated, breaks, breaks_where, check, last};

    impl Narrated for DrainCopy {
        fn story(&self, path: &[(State, Option<Action>)]) -> String {
            story(path)
        }
    }

    fn switched(switches: Switches) -> DrainCopy {
        DrainCopy { switches }
    }

    /// The first path that breaks `property` with `switches` set, printed.
    fn witness(switches: Switches, property: &'static str) -> State {
        let model = switched(switches);
        let path = breaks(model.clone(), property);
        println!(
            "{property} fails under {switches:?}:\n{}",
            model.story(&path)
        );
        last(&path)
    }

    /// As shipped, every property holds and every `sometimes` is reached: a fresh answer, a
    /// resend that adopts what its lost send made and a Gone that removes it, a cancel in the
    /// resend wait beside another caller's Container, a restored Container the Gone removes,
    /// a late create after the Gone, and a report of the replacement removed.
    #[test]
    fn the_drain_s_cleanup_takes_only_what_its_own_sends_made() {
        let checker = check(switched(Switches::default()));
        println!("as shipped: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// With the shared key `move:{old}`, the drain's Gone takes what another caller made in
    /// it after the drain's own send failed.
    #[test]
    fn a_shared_key_removes_another_caller_s_container() {
        let model = switched(Switches {
            key: Key::Shared,
            ..Switches::default()
        });
        let path = breaks_where(model.clone(), NO_FOREIGN_REMOVAL, |path| {
            last(path)
                .drain
                .gone
                .is_some_and(|gone| gone.slot == Slot::Shared && gone.took.is_some())
        });
        println!(
            "{NO_FOREIGN_REMOVAL} fails with a shared key:\n{}",
            model.story(&path)
        );
        let last = last(&path);
        assert!(last.foreign_removed);
        assert!(
            last.ours().next().is_none(),
            "nothing of the move was there to take"
        );
    }

    /// With a key per send, the Gone names only the last send's slot: the first send made a
    /// Container and lost its answer, and that Container is left.
    #[test]
    fn a_key_per_send_leaves_the_lost_send_s_container() {
        let last = witness(
            Switches {
                key: Key::PerSend,
                ..Switches::default()
            },
            NOTHING_LEFT,
        );
        assert_eq!(last.drain.gone.map(|gone| gone.slot), Some(Slot::Resent));
        assert!(
            last.slot(Slot::Minted)
                .is_some_and(|left| left.made_by == Maker::Move)
        );
        assert!(!last.foreign_removed);
    }

    /// In a slot another caller can name, the final read by slot names whatever holds it.
    #[test]
    fn an_answer_read_by_slot_names_another_container() {
        let last = witness(
            Switches {
                key: Key::Shared,
                reply: ReplyBy::Slot,
                ..Switches::default()
            },
            REPLY_IS_OURS,
        );
        assert!(
            matches!(last.drain.answer, Some(Answer::Present { id, .. }) if last.handler.acted != Some(id))
        );
    }

    /// In a slot another caller can name, with no image check the handler answers a running
    /// same-spec Container on another image, and starts a stopped one.
    #[test]
    fn without_the_image_check_another_image_is_answered_and_started() {
        let unchecked = Switches {
            key: Key::Shared,
            check: Check::None,
            ..Switches::default()
        };
        let last = witness(unchecked, REPLY_IS_OURS);
        assert!(matches!(
            last.drain.answer,
            Some(Answer::Present {
                image: Image::Other,
                ..
            })
        ));
        assert!(witness(unchecked, NO_WRONG_START).wrong_start);
    }

    /// Checked only after the start, another caller's stopped Container on another image is
    /// started before the check refuses it.
    #[test]
    fn an_image_check_after_the_start_comes_too_late() {
        let last = witness(
            Switches {
                key: Key::Shared,
                check: Check::AfterStart,
                ..Switches::default()
            },
            NO_WRONG_START,
        );
        assert!(last.wrong_start);
    }
}
