//! A drain's start of its new copy on the destination Server, from the request to the
//! cleanup: the handler that serves it, another caller of ensure.v1 on the same slot,
//! and the drain that sends it, cancels, reads the answer and cleans up.
//!
//! The properties:
//!
//! - **The reply is ours.** Every `Present` answer names the Container the handler made or
//!   adopted for this request, with the drain's spec and the pinned image.
//! - **No wrong start.** The handler never starts a Container that runs another image.
//! - **Cleanup by the named ID.** After a definitive answer, the drain removes only the
//!   Container a `Present` answer named. After `Absent` or a refusal it removes nothing, so a
//!   refused occupant another caller made always survives. An answer is definitive when the
//!   Machine gave it, as a typed answer or an error it raised before Docker's create, and no
//!   earlier send of the start reached the handler and lost its answer.
//! - **Nothing of ours is left.** A drain that ends without moving leaves no Container the
//!   handler made in the slot, unless the handler made it after the drain's removal by key.
//!
//! The actors, each step its own transition:
//!
//! - the handler, `MachineService::ensure` (ployzd/src/machine_api/local.rs), then
//!   `converge_now` and `create_and_start` (ployzd/src/machine/local_machine/container.rs):
//!   admission may refuse the request before anything else, as an error it marks as
//!   refused before acting (`LocalMachineError::Unacted`); otherwise it reads
//!   the slot and plans (`ContainerStep::{Done, Start, CreateAndStart}`), creates or adopts
//!   the Container a name conflict finds, checks the image by ID (`image_differs`) before it
//!   starts or answers, starts, then reads the Container it acted on (`container_now` of
//!   `ContainerLocator::Id(acted)`) and answers. `Done` answers what its plan read. A
//!   signer removed while the image pulled refuses the create right before Docker makes it
//!   (`still_holds` in `create_admitted`, ployzd/src/docker/lifecycle.rs), marked as
//!   refused before acting. Its start may be refused after the create (`start_checked`'s
//!   signer check), as an error with no mark. One request, and one resend after a lost
//!   answer or a send the entry couldn't dial (`RestartTolerant`,
//!   ployz/src/deploy/exec/transport.rs), which the entry may refuse to redial;
//! - the planter, an ensure.v1 caller on the drain's key `move:{old}`, acting at most twice:
//!   it creates and starts a Container of either spec and either image while the slot is
//!   empty, stops the slot's Container by ID, or removes it by key;
//! - the drain, `move_container` (ployz/src/deploy/exec/start.rs): it sends, may cancel
//!   before the answer, reads what each failed send did from its error, waits out health,
//!   and cleans up by the `Outcome` that `ployz_decide::drain_start::Outcome::of` builds
//!   from its last send and the sends before it. The model calls both: `Failed::of` on the
//!   error each failed send carries, as the entry or the handler words it, and
//!   `Outcome::of`.
//!
//! Bounds: one destination slot, Container IDs up to 4, two specs, two images.
//!
//! Out of scope: a second destination (two Servers can't show a copy landing on the wrong
//! one), two drains at once, rotation and the steps after a healthy answer (`relocate`,
//! `bring_in`, `wait_listed`, `retire_old`, all unchanged), and who signed the spec. A
//! discard by key with no answer can still remove another caller's Container that took the
//! slot meanwhile; the model reaches it and accepts it, as main does.

use std::fmt::Write as _;

use ployz_core::{RpcError, RpcErrorCode};
use ployz_decide::drain_start::{Earlier, Failed, Heard, Outcome};
use stateright::{Model, Property};

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
    Handler,
    Planter,
}

/// The Container named by the slot's key. Docker names a Container by its key, so the
/// slot holds at most one.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Container {
    id: u8,
    spec: Spec,
    image: Image,
    running: bool,
    made_by: Maker,
}

const MAX_ID: u8 = 4;

/// Why the handler refused.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Refusal {
    /// `ContainerRefusalReason::SlotOccupied`: the slot holds another spec's Container.
    SlotOccupied,
    /// `ContainerRefusalReason::ImageDiffers`: the Container runs another image.
    ImageDiffers,
}

/// An error in place of an answer.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Erred {
    /// Admission refused the request before the slot was read, and marked the error so.
    Admission,
    /// The signer was removed while the image pulled, so the create was refused right
    /// before Docker made it, by an error marked so.
    BeforeCreate,
    /// The start was refused after the create, by an error that names its gate but carries
    /// no mark.
    AfterCreate,
    /// The entry refused to dial again for the resend, which never left the drain.
    Redial,
}

/// What came back to the drain, as `EnsuredContainer` reads.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Answer {
    /// `Ensured { now: Present }`, with what the final read saw.
    Present { id: u8, spec: Spec, image: Image },
    /// `Ensured { now: Absent }`.
    Absent,
    /// `Refused`, naming the occupant the read found.
    Refused {
        refusal: Refusal,
        occupant: Option<u8>,
    },
    /// An error that says why.
    Error(Erred),
}

/// What the handler does next, as `converge_now` and `create_and_start` order it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Step {
    /// Waits for the drain's request.
    Idle,
    /// Reads the slot and plans the step (`plan` in `converge_now`).
    Plan,
    /// `create_container_admitted`: creates the Container, or adopts the one a name
    /// conflict finds when it was made from the same spec.
    Create,
    /// `image_differs`: the Container must run the pinned image, before it starts or the
    /// handler answers.
    Check { id: u8, then: Then },
    /// `start_checked`. `checked` says whether the image was checked already.
    Start { id: u8, checked: bool },
    /// The final read and the answer.
    Answer,
    /// Answered, or the request was lost with nothing left to resend.
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
    /// The Container this run made or adopted, or `Start` planned (`acted`).
    acted: Option<u8>,
    refusal: Option<Refusal>,
    erred: Option<Erred>,
    /// A Docker failure: the Container it acted on was gone. The drain gets an error.
    failed: bool,
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
    /// No answer: cancelled before one, lost twice, or a daemon error.
    Unanswered,
    Answered,
    /// Healthy: the move is done.
    Moved,
    /// Cleaned up, or nothing to clean.
    Finished,
}

/// A removal the drain sent: by the ID an answer named, or by the slot's key.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Removal {
    Id(u8),
    Key,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Drain {
    phase: Phase,
    /// What the sends before the last may have done.
    earlier: Earlier,
    answer: Option<Answer>,
    /// The answered Container failed health, or the drain was cancelled after the answer.
    failed: bool,
    /// The removal it sent, and the Container it took, if any.
    removal: Option<(Removal, Option<u8>)>,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    slot: Option<Container>,
    next_id: u8,
    handler: Handler,
    planter_acts: u8,
    drain: Drain,
    /// The handler started a Container that runs another image.
    wrong_start: bool,
    /// The handler made a Container after the drain removed by key.
    made_after_discard: bool,
    /// A send reached the handler and its answer was lost, so it may have acted: what the
    /// drain's `earlier` must say, whatever its errors told it.
    lost_after_reaching: bool,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum PlanterAct {
    Create(Spec, Image),
    Stop,
    Gone,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    Send,
    /// The drain is cancelled before any answer.
    Cancel,
    Handler,
    /// Admission refuses the request.
    Refuse,
    /// The signer is removed while the image pulls: the create is refused before Docker
    /// makes anything.
    RefuseBeforeCreate,
    /// The entry can't dial the destination, so the request never reaches the handler;
    /// `RestartTolerant` resends once.
    Undial,
    /// `start_checked` refuses the start after the create.
    RefuseStart,
    /// The entry refuses to dial again, so the resend never leaves.
    RefuseRedial,
    /// The answer reaches the drain.
    Deliver,
    /// The answer is lost on the way; `RestartTolerant` resends once.
    Lose,
    /// The answered Container serves healthy.
    Healthy,
    /// The answered Container fails health, or the drain is cancelled after the answer.
    Fails,
    /// The drain cleans up what its answer, or the lack of one, left.
    Cleanup,
    Planter(PlanterAct),
}

/// The code as it ships, or one part of it switched back to an earlier design.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
struct Switches {
    reply: ReplyBy,
    check: Check,
    cleanup: Cleanup,
    marks: Marks,
    dial: Dial,
}

/// Which errors the handler marks as raised before it acted.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
enum Marks {
    /// Admission's, and the signer check right before Docker's create
    /// (`LocalMachineError::Unacted` through `CreateFailure::unacted`).
    #[default]
    BeforeCreate,
    /// Admission's only: the signer check before the create answers unmarked.
    AdmissionOnly,
    /// None.
    Nothing,
}

/// What the client keeps of an entry that couldn't dial the destination.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
enum Dial {
    /// The router's mark, kept as `undialed` by `answered_by`.
    #[default]
    Kept,
    /// Dropped: the error says only that the Machine didn't answer.
    Dropped,
}

/// What the final read names.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
enum ReplyBy {
    /// `container_now(&ContainerLocator::Id(acted))`.
    #[default]
    Acted,
    /// The slot, as the read before this change did.
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

/// How the drain cleans up.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
enum Cleanup {
    /// By what it heard: the named ID, nothing, or the key with no answer.
    #[default]
    ByAnswer,
    /// By the slot's key after any failure, as main's `discard_attempt` did.
    BySlot,
    /// As `ByAnswer`, and also removes the occupant a refusal names.
    RefusedOccupant,
    /// As `ByAnswer`, but any error that names its gate is read as a refusal, marked or not.
    GateAsRefusal,
    /// As `ByAnswer`, but only the last send counts, as if no earlier one had been lost.
    LastReply,
}

#[derive(Clone, Debug)]
struct DrainCopy {
    switches: Switches,
}

const REPLY_IS_OURS: &str =
    "every Present answer names our Container, from our spec, on the pinned image";
const NO_WRONG_START: &str = "the handler never starts a Container that runs another image";
const CLEANUP_BY_ID: &str =
    "after a definitive answer the drain removes only the Container a Present answer named";

/// The error a gate refusing a deploy's signer words, as `local_failure` sends it for
/// `DeployRefused`: it names its capability, and is marked only when `marked`.
fn gated(marked: bool) -> RpcError {
    let error = RpcError::new(
        RpcErrorCode::from(ployz_core::FORBIDDEN_CODE),
        "the spec's signer is no longer trusted here",
    )
    .with_details(serde_json::json!({ "capability": ployz_core::DEPLOY_APPROVAL_CAPABILITY }));
    if marked { error.unacted() } else { error }
}
const NOTHING_LEFT: &str =
    "a drain that ends without moving leaves no Container the handler made in the slot";

impl State {
    fn container(&self, id: u8) -> Option<Container> {
        self.slot.filter(|container| container.id == id)
    }

    fn make(&mut self, spec: Spec, image: Image, made_by: Maker) -> Option<u8> {
        if self.next_id > MAX_ID {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.slot = Some(Container {
            id,
            spec,
            image,
            running: made_by == Maker::Planter,
            made_by,
        });
        Some(id)
    }
}

impl DrainCopy {
    /// One handler step from `state`.
    fn handle(&self, state: &mut State) {
        let handler = &mut state.handler;
        match handler.step {
            Step::Idle | Step::Answer | Step::Done => unreachable!("not a handler step"),
            Step::Plan => {
                handler.step = match state.slot {
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
            Step::Create => match state.slot {
                None => {
                    let made = state.make(Spec::Drained, Image::Pinned, Maker::Handler);
                    let handler = &mut state.handler;
                    if handler.run == 1 {
                        handler.first_made = made;
                    }
                    if state
                        .drain
                        .removal
                        .is_some_and(|(removal, _)| removal == Removal::Key)
                    {
                        state.made_after_discard = true;
                    }
                    let handler = &mut state.handler;
                    match made {
                        Some(id) => {
                            handler.acted = Some(id);
                            handler.step = self.after_plan(id, Then::Start);
                        }
                        None => {
                            handler.failed = true;
                            handler.step = Step::Answer;
                        }
                    }
                }
                // `create_refusal`: nothing was made, so the answer reads the slot.
                Some(occupant) if occupant.spec == Spec::Other => {
                    handler.refusal = Some(Refusal::SlotOccupied);
                    handler.step = Step::Answer;
                }
                Some(occupant) => {
                    handler.acted = Some(occupant.id);
                    handler.step = self.after_plan(occupant.id, Then::Start);
                }
            },
            Step::Check { id, then } => {
                handler.step = match state.slot.filter(|container| container.id == id) {
                    None => {
                        handler.failed = true;
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
            Step::Start { id, checked } => {
                match state.slot.filter(|container| container.id == id) {
                    None => {
                        handler.failed = true;
                        handler.step = Step::Answer;
                    }
                    Some(container) => {
                        // Starting a running Container changes nothing.
                        if container.image != Image::Pinned && !container.running {
                            state.wrong_start = true;
                        }
                        if let Some(slot) = &mut state.slot {
                            slot.running = true;
                        }
                        let handler = &mut state.handler;
                        handler.step = if !checked && self.switches.check == Check::AfterStart {
                            Step::Check {
                                id,
                                then: Then::Answer,
                            }
                        } else {
                            Step::Answer
                        };
                    }
                }
            }
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

    /// The final read and its answer; `None` for a Docker failure.
    fn answer(&self, state: &State) -> Option<Answer> {
        let handler = state.handler;
        if handler.failed {
            return None;
        }
        let read = match (handler.planned, self.switches.reply) {
            (Some(planned), _) => Some(planned),
            (None, ReplyBy::Acted) => handler.acted.map_or(state.slot, |id| state.container(id)),
            (None, ReplyBy::Slot) => state.slot,
        };
        if let Some(erred) = handler.erred {
            // An error carries no read of the slot.
            return Some(Answer::Error(erred));
        }
        Some(match handler.refusal {
            Some(refusal) => Answer::Refused {
                refusal,
                occupant: read.map(|container| container.id),
            },
            None => read.map_or(Answer::Absent, |container| Answer::Present {
                id: container.id,
                spec: container.spec,
                image: container.image,
            }),
        })
    }

    /// What the drain's last send heard, as `move_container` reads it.
    fn heard(&self, answer: Option<Answer>) -> Heard<u8> {
        let marks = self.switches.marks;
        let error = match answer {
            None => return Heard::Unanswered,
            Some(Answer::Present { id, .. }) => return Heard::Present(id),
            Some(Answer::Absent) => return Heard::Absent,
            Some(Answer::Refused { .. }) => return Heard::Refused,
            // The resend never left: the start's last send is the lost one before it,
            // which `earlier` holds already.
            Some(Answer::Error(Erred::Redial)) => return Heard::Unsent,
            Some(Answer::Error(Erred::Admission)) => gated(marks != Marks::Nothing),
            Some(Answer::Error(Erred::BeforeCreate)) => gated(marks == Marks::BeforeCreate),
            Some(Answer::Error(Erred::AfterCreate)) => gated(false),
        };
        if self.switches.cleanup == Cleanup::GateAsRefusal
            && error.details.get("capability").is_some()
        {
            return Heard::Refused;
        }
        Failed::of(&error).into()
    }

    /// The error the drain gets for a send the entry couldn't dial, as `answered_by` words
    /// the router's status.
    fn undialed(&self) -> RpcError {
        let silent = RpcError::unanswered_target(
            RpcErrorCode::Unavailable,
            "Machine 2 did not answer: connection refused".into(),
        );
        match self.switches.dial {
            Dial::Kept => silent.undialed_target(),
            Dial::Dropped => silent,
        }
    }

    /// The removal the drain sends for what it heard, if any.
    fn removal(&self, drain: Drain) -> Option<Removal> {
        let cleanup = self.switches.cleanup;
        match drain.answer {
            Some(_) if cleanup == Cleanup::BySlot => return Some(Removal::Key),
            Some(Answer::Refused {
                occupant: Some(id), ..
            }) if cleanup == Cleanup::RefusedOccupant => return Some(Removal::Id(id)),
            _ => {}
        }
        let earlier = if cleanup == Cleanup::LastReply {
            Earlier::Nothing
        } else {
            drain.earlier
        };
        match Outcome::of(earlier, self.heard(drain.answer)) {
            Outcome::Unknown => Some(Removal::Key),
            Outcome::Absent | Outcome::Refused => None,
            Outcome::Present(id) => Some(Removal::Id(id)),
        }
    }

    /// The drain hears `answer`, or nothing, and goes on to what it calls for.
    fn hear(&self, state: &mut State, answer: Option<Answer>) {
        if state.drain.phase != Phase::Waiting {
            return;
        }
        let Some(answer) = answer else {
            state.drain.phase = Phase::Unanswered;
            return;
        };
        state.drain.answer = Some(answer);
        state.drain.phase = Phase::Answered;
        // Nothing waits on an answer that is not Present and that leaves nothing to remove.
        if !matches!(answer, Answer::Present { .. }) && self.removal(state.drain).is_none() {
            state.drain.phase = Phase::Finished;
        }
    }
}

impl Model for DrainCopy {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            slot: None,
            next_id: 1,
            handler: Handler {
                step: Step::Idle,
                run: 1,
                acted: None,
                refusal: None,
                erred: None,
                failed: false,
                first_made: None,
                planned: None,
            },
            planter_acts: 0,
            drain: Drain {
                phase: Phase::Ready,
                earlier: Earlier::Nothing,
                answer: None,
                failed: false,
                removal: None,
            },
            wrong_start: false,
            made_after_discard: false,
            lost_after_reaching: false,
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
            Step::Plan => {
                actions.extend([Action::Handler, Action::Refuse]);
                match state.handler.run {
                    1 => actions.push(Action::Undial),
                    _ => actions.push(Action::RefuseRedial),
                }
            }
            Step::Start { .. } => actions.extend([Action::Handler, Action::RefuseStart]),
            // `still_holds` runs only when no Container of the key was found to adopt.
            Step::Create if state.slot.is_none() => {
                actions.extend([Action::Handler, Action::RefuseBeforeCreate]);
            }
            Step::Create | Step::Check { .. } => actions.push(Action::Handler),
        }
        if state.planter_acts < 2 {
            match state.slot {
                None if state.next_id <= MAX_ID => {
                    for spec in [Spec::Drained, Spec::Other] {
                        for image in [Image::Pinned, Image::Other] {
                            actions.push(Action::Planter(PlanterAct::Create(spec, image)));
                        }
                    }
                }
                None => {}
                Some(container) => {
                    if container.running {
                        actions.push(Action::Planter(PlanterAct::Stop));
                    }
                    actions.push(Action::Planter(PlanterAct::Gone));
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
            Action::Cancel => state.drain.phase = Phase::Unanswered,
            Action::Handler => self.handle(&mut state),
            Action::Refuse => {
                state.handler.erred = Some(Erred::Admission);
                state.handler.step = Step::Answer;
            }
            Action::RefuseBeforeCreate => {
                state.handler.erred = Some(Erred::BeforeCreate);
                state.handler.step = Step::Answer;
            }
            // The handler never got the request; the resend is its first.
            Action::Undial => {
                state.handler.run = 2;
                state.drain.earlier = state.drain.earlier.then(Failed::of(&self.undialed()));
            }
            // The created Container stays, not started.
            Action::RefuseStart => {
                state.handler.erred = Some(Erred::AfterCreate);
                state.handler.step = Step::Answer;
            }
            Action::RefuseRedial => {
                state.handler.step = Step::Done;
                self.hear(&mut state, Some(Answer::Error(Erred::Redial)));
            }
            Action::Deliver | Action::Lose => {
                let answer = self.answer(&state);
                let handler = &mut state.handler;
                let resend = action == Action::Lose && handler.run == 1;
                if resend {
                    *handler = Handler {
                        step: Step::Plan,
                        run: 2,
                        acted: None,
                        refusal: None,
                        erred: None,
                        failed: false,
                        first_made: handler.first_made,
                        planned: None,
                    };
                    // The lost send may have acted; the resend's answer speaks only for itself.
                    state.drain.earlier = state.drain.earlier.then(Failed::Unanswered);
                    state.lost_after_reaching = true;
                } else {
                    handler.step = Step::Done;
                    let heard = if action == Action::Deliver {
                        answer
                    } else {
                        None
                    };
                    self.hear(&mut state, heard);
                }
            }
            Action::Healthy => state.drain.phase = Phase::Moved,
            Action::Fails => state.drain.failed = true,
            Action::Cleanup => {
                let removal = self.removal(state.drain).expect("a removal to send");
                let taken = match removal {
                    Removal::Id(id) => state.container(id).map(|container| container.id),
                    Removal::Key => state.slot.map(|container| container.id),
                };
                if taken.is_some() {
                    state.slot = None;
                }
                state.drain.removal = Some((removal, taken));
                state.drain.phase = Phase::Finished;
            }
            Action::Planter(act) => {
                state.planter_acts += 1;
                match act {
                    PlanterAct::Create(spec, image) => {
                        state.make(spec, image, Maker::Planter)?;
                    }
                    PlanterAct::Stop => {
                        if let Some(slot) = &mut state.slot {
                            slot.running = false;
                        }
                    }
                    PlanterAct::Gone => state.slot = None,
                }
            }
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(REPLY_IS_OURS, |_, state: &State| {
                match state.drain.answer {
                    Some(Answer::Present { id, spec, image }) => {
                        spec == Spec::Drained && image == Image::Pinned
                            // The answer of the run that served it: the last one.
                            && state.handler.acted == Some(id)
                    }
                    _ => true,
                }
            }),
            Property::always(NO_WRONG_START, |_, state: &State| !state.wrong_start),
            Property::always(CLEANUP_BY_ID, |_, state: &State| {
                let drain = state.drain;
                let definitive = !state.lost_after_reaching
                    && matches!(
                        drain.answer,
                        Some(
                            Answer::Present { .. }
                                | Answer::Absent
                                | Answer::Refused { .. }
                                | Answer::Error(Erred::Admission | Erred::BeforeCreate)
                        )
                    );
                match (drain.answer, drain.removal) {
                    (_, None) => true,
                    (Some(Answer::Present { id, .. }), Some((Removal::Id(removed), _))) => {
                        id == removed
                    }
                    _ => !definitive,
                }
            }),
            Property::always(NOTHING_LEFT, |_, state: &State| {
                state.drain.phase != Phase::Finished
                    || state.made_after_discard
                    || state.slot.is_none_or(|c| c.made_by != Maker::Handler)
            }),
            Property::sometimes(
                "a fresh answer names the Container just made",
                |_, state: &State| {
                    matches!(state.drain.answer, Some(Answer::Present { id, .. })
                    if state.handler.run == 1 && state.handler.first_made == Some(id))
                },
            ),
            Property::sometimes(
                "a resend adopts the Container its lost answer made",
                |_, state: &State| {
                    matches!(state.drain.answer, Some(Answer::Present { id, .. })
                    if state.handler.run == 2 && state.handler.first_made == Some(id))
                },
            ),
            Property::sometimes(
                "the planter removed ours and filled the slot: Absent, and its Container is left",
                |_, state: &State| {
                    state.drain.answer == Some(Answer::Absent)
                        && state.drain.phase == Phase::Finished
                        && state.slot.is_some_and(|y| y.made_by == Maker::Planter)
                },
            ),
            Property::sometimes(
                "a refusal leaves the planter's occupant in place",
                |_, state: &State| {
                    matches!(state.drain.answer, Some(Answer::Refused { occupant: Some(id), .. })
                        if state.drain.phase == Phase::Finished
                            && state.container(id).is_some_and(|o| o.made_by == Maker::Planter))
                },
            ),
            Property::sometimes(
                "an admission refusal leaves the planter's Container in place",
                |_, state: &State| {
                    state.drain.answer == Some(Answer::Error(Erred::Admission))
                        && state.drain.earlier == Earlier::Nothing
                        && state.drain.phase == Phase::Finished
                        && state.slot.is_some_and(|o| o.made_by == Maker::Planter)
                },
            ),
            Property::sometimes(
                "a signer removed before the create leaves the planter's Container in place",
                |_, state: &State| {
                    state.drain.answer == Some(Answer::Error(Erred::BeforeCreate))
                        && state.drain.phase == Phase::Finished
                        && state.slot.is_some_and(|o| o.made_by == Maker::Planter)
                },
            ),
            Property::sometimes(
                "a refusal after a send the entry couldn't dial leaves the occupant in place",
                |_, state: &State| {
                    matches!(state.drain.answer, Some(Answer::Refused { occupant: Some(id), .. })
                        if state.handler.run == 2
                            && !state.lost_after_reaching
                            && state.drain.phase == Phase::Finished
                            && state.container(id).is_some_and(|o| o.made_by == Maker::Planter))
                },
            ),
            Property::sometimes(
                "a refused redial after a lost answer removes the lost send's Container by key",
                |_, state: &State| {
                    state.drain.answer == Some(Answer::Error(Erred::Redial))
                        && state.handler.first_made.is_some()
                        && state.drain.removal.map(|(_, taken)| taken)
                            == Some(state.handler.first_made)
                },
            ),
            Property::sometimes(
                "a start refused after its create removes that Container by key",
                |_, state: &State| {
                    state.drain.answer == Some(Answer::Error(Erred::AfterCreate))
                        && matches!(state.drain.removal, Some((Removal::Key, Some(_))))
                },
            ),
            Property::sometimes(
                "the handler makes a Container after the drain removed by key",
                |_, state: &State| state.made_after_discard,
            ),
        ]
    }
}

impl DrainCopy {
    /// Whether the drain's next step is its cleanup.
    fn cleans_up(&self, state: &State) -> bool {
        let drain = state.drain;
        let due = match drain.phase {
            Phase::Unanswered => true,
            Phase::Answered => match drain.answer {
                // A Container answered healthy is the move's; it waits for `Fails`.
                Some(Answer::Present { .. }) => drain.failed,
                _ => true,
            },
            Phase::Ready | Phase::Waiting | Phase::Moved | Phase::Finished => false,
        };
        due && self.removal(drain).is_some()
    }
}

/// `path` in the code's words: each step, then the slot and what the drain holds.
fn story(path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    let mut led_by: Option<(Action, State)> = None;
    for (n, (state, action)) in path.iter().enumerate() {
        let what = match led_by {
            None => "start".to_owned(),
            Some((action, before)) => reads_as(action, &before),
        };
        led_by = action.map(|action| (action, *state));
        let slot = state.slot.map_or_else(
            || "empty".to_owned(),
            |c| {
                format!(
                    "#{} {:?} spec on {:?} image, {}, made by {:?}",
                    c.id,
                    c.spec,
                    c.image,
                    if c.running { "running" } else { "not running" },
                    c.made_by
                )
            },
        );
        let _ = writeln!(
            story,
            "{n:2}. {what}\n    slot: {slot}; handler run {} at {:?}, acted {:?}; drain {:?}, earlier {:?}, answer {:?}, removal {:?}",
            state.handler.run,
            state.handler.step,
            state.handler.acted,
            state.drain.phase,
            state.drain.earlier,
            state.drain.answer,
            state.drain.removal,
        );
    }
    story
}

fn reads_as(action: Action, before: &State) -> String {
    match action {
        Action::Send => "drain: move_container sends the start (ensure_new)".to_owned(),
        Action::Cancel => "drain: cancelled before any answer".to_owned(),
        // Whether the refusal carries its mark depends on the switches.
        Action::Refuse => "handler: admit_spec refuses the request before acting".to_owned(),
        Action::RefuseBeforeCreate => {
            "handler: still_holds refuses the create right before Docker".to_owned()
        }
        Action::Undial => "entry: can't dial the destination; the drain resends".to_owned(),
        Action::RefuseStart => {
            "handler: start_checked refuses the start after the create, unmarked".to_owned()
        }
        Action::RefuseRedial => "drain: the entry refuses to dial again for the resend".to_owned(),
        Action::Handler => match before.handler.step {
            Step::Plan => "handler: converge_now reads the slot and plans".to_owned(),
            Step::Create => {
                "handler: create_and_start creates, or adopts the Container a name conflict finds"
                    .to_owned()
            }
            Step::Check { id, .. } => format!("handler: image_differs reads #{id}'s image"),
            Step::Start { id, .. } => format!("handler: start_checked starts #{id}"),
            Step::Idle | Step::Answer | Step::Done => unreachable!("not a handler step"),
        },
        Action::Deliver => "handler: the final read, and its answer reaches the drain".to_owned(),
        Action::Lose => "handler: the final read, and its answer is lost".to_owned(),
        Action::Healthy => "drain: the answered Container serves healthy".to_owned(),
        Action::Fails => {
            "drain: the answered Container fails health, or a cancel comes after the answer"
                .to_owned()
        }
        Action::Cleanup => "drain: cleans up by its Outcome".to_owned(),
        Action::Planter(act) => format!("planter: {act:?} on key move:{{old}}"),
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

    /// As shipped, every property holds and every `sometimes` is reached: a fresh answer,
    /// a resend that adopts its own Container, an Absent answer beside the planter's
    /// Container, a refusal that leaves the planter's occupant, an admission refusal and a
    /// signer removed before the create that each leave the planter's Container, a refusal
    /// after a send the entry couldn't dial that leaves the occupant, a refused redial and
    /// a start refused after its create that each remove the start's Container by key, and
    /// a late create after a removal by key.
    #[test]
    fn the_drain_s_start_answers_and_cleans_up_only_its_own_container() {
        let checker = check(switched(Switches::default()));
        println!("as shipped: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// Read by slot, the final read names whatever holds the slot: the planter removed
    /// ours after its start and made another spec's Container there.
    #[test]
    fn an_answer_read_by_slot_names_another_container() {
        let last = witness(
            Switches {
                reply: ReplyBy::Slot,
                ..Switches::default()
            },
            REPLY_IS_OURS,
        );
        assert!(
            matches!(last.drain.answer, Some(Answer::Present { id, .. }) if last.handler.acted != Some(id))
        );
    }

    /// With no image check, the handler answers a running same-spec Container on another
    /// image, and starts a stopped one.
    #[test]
    fn without_the_image_check_another_image_is_answered_and_started() {
        let unchecked = Switches {
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

    /// Checked only after the start, the planter's stopped Container on another image is
    /// started before the check refuses it.
    #[test]
    fn an_image_check_after_the_start_comes_too_late() {
        let last = witness(
            Switches {
                check: Check::AfterStart,
                ..Switches::default()
            },
            NO_WRONG_START,
        );
        assert!(last.wrong_start);
    }

    /// Main's cleanup removes by slot key after any failure. The planter removed our
    /// Container and made its own in the slot, the answer read ours by ID as Absent, and
    /// the cleanup by key takes the planter's.
    #[test]
    fn a_cleanup_by_slot_removes_another_caller_s_container() {
        let model = switched(Switches {
            cleanup: Cleanup::BySlot,
            ..Switches::default()
        });
        let path = breaks_where(model.clone(), CLEANUP_BY_ID, |path| {
            let last = last(path);
            last.drain.answer == Some(Answer::Absent)
                && matches!(last.drain.removal, Some((Removal::Key, Some(_))))
        });
        println!("{CLEANUP_BY_ID} fails by slot:\n{}", model.story(&path));
        let last = last(&path);
        assert!(matches!(last.drain.removal, Some((Removal::Key, Some(_)))));
    }

    /// Removing the occupant a refusal names takes another caller's Container, which may
    /// be a correct one made after the conflict.
    #[test]
    fn removing_a_refused_occupant_removes_another_caller_s_container() {
        let last = witness(
            Switches {
                cleanup: Cleanup::RefusedOccupant,
                ..Switches::default()
            },
            CLEANUP_BY_ID,
        );
        assert!(matches!(last.drain.answer, Some(Answer::Refused { .. })));
        assert!(matches!(
            last.drain.removal,
            Some((Removal::Id(_), Some(_)))
        ));
    }

    /// Read as no answer, admission's refusal sends the drain's removal by key, which takes
    /// the Container another caller left in the slot.
    #[test]
    fn a_refusal_read_as_no_answer_removes_another_caller_s_container() {
        let model = switched(Switches {
            marks: Marks::Nothing,
            ..Switches::default()
        });
        let path = breaks_where(model.clone(), CLEANUP_BY_ID, |path| {
            matches!(last(path).drain.removal, Some((Removal::Key, Some(_))))
        });
        println!(
            "{CLEANUP_BY_ID} fails on a refusal read as no answer:\n{}",
            model.story(&path)
        );
        let last = last(&path);
        assert_eq!(last.drain.answer, Some(Answer::Error(Erred::Admission)));
    }

    /// Read from its last send alone, a refusal of the resend speaks for the send whose
    /// answer was lost: the drain removes nothing and leaves the Container that send made.
    #[test]
    fn a_refused_resend_read_alone_leaves_the_lost_send_s_container() {
        let model = switched(Switches {
            cleanup: Cleanup::LastReply,
            ..Switches::default()
        });
        let path = breaks_where(model.clone(), NOTHING_LEFT, |path| {
            last(path).drain.answer == Some(Answer::Error(Erred::Redial))
        });
        println!(
            "{NOTHING_LEFT} fails on the last send alone:\n{}",
            model.story(&path)
        );
        let last = last(&path);
        assert_eq!(last.drain.earlier, Earlier::MaybeActed);
        assert_eq!(last.drain.removal, None);
        assert!(last.slot.is_some_and(|c| c.made_by == Maker::Handler));
    }

    /// Read as a refusal, a gated error after the create speaks for a Container the start
    /// made: the drain removes nothing and leaves it.
    #[test]
    fn a_gated_error_after_the_create_read_as_a_refusal_leaves_its_container() {
        let last = witness(
            Switches {
                cleanup: Cleanup::GateAsRefusal,
                ..Switches::default()
            },
            NOTHING_LEFT,
        );
        assert_eq!(last.drain.answer, Some(Answer::Error(Erred::AfterCreate)));
        assert_eq!(last.drain.removal, None);
    }

    /// Left unmarked, the signer check right before Docker's create reads as no answer:
    /// the drain removes by key, which takes the Container the planter made after it.
    #[test]
    fn a_refusal_before_the_create_left_unmarked_removes_another_caller_s_container() {
        let model = switched(Switches {
            marks: Marks::AdmissionOnly,
            ..Switches::default()
        });
        let path = breaks_where(model.clone(), CLEANUP_BY_ID, |path| {
            let last = last(path);
            last.drain.answer == Some(Answer::Error(Erred::BeforeCreate))
                && matches!(last.drain.removal, Some((Removal::Key, Some(_))))
        });
        println!(
            "{CLEANUP_BY_ID} fails on an unmarked refusal before the create:\n{}",
            model.story(&path)
        );
        let last = last(&path);
        assert!(matches!(last.drain.removal, Some((Removal::Key, Some(_)))));
        assert!(last.slot.is_none());
    }

    /// Without the entry's word that it couldn't dial, a send that never reached the
    /// handler reads as one that may have acted. The resend's refusal then removes by key
    /// and takes the occupant it named.
    #[test]
    fn a_send_the_entry_could_not_dial_read_as_unanswered_removes_the_occupant() {
        let model = switched(Switches {
            dial: Dial::Dropped,
            ..Switches::default()
        });
        let path = breaks_where(model.clone(), CLEANUP_BY_ID, |path| {
            let last = last(path);
            matches!(last.drain.answer, Some(Answer::Refused { .. }))
                && last.handler.first_made.is_none()
        });
        println!(
            "{CLEANUP_BY_ID} fails when the failed dial is forgotten:\n{}",
            model.story(&path)
        );
        let last = last(&path);
        assert_eq!(last.drain.earlier, Earlier::MaybeActed);
        assert!(!last.lost_after_reaching);
        assert!(matches!(last.drain.removal, Some((Removal::Key, Some(_)))));
    }
}
