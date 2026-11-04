//! This device's Cloud sign-in file, `cloud.json`, and the processes that read and write it
//! while a sign-in waits for the browser. Paths are under `core/crates/ployz/src/`, and a
//! bare `:n` is in `cloud_login.rs`.
//!
//! `ployz login` without `--json` waits (handlers/login.rs:83, :99). Its `start` reads the
//! file with `replaceable` (:307). It refreshes a SignedIn Cloud still honours (:314-323),
//! resumes a Pending that has not expired with one poll (:325-337), and otherwise asks Cloud
//! for a new code and saves it as a Pending (`request_code`, :340, :489-518). `wait` then
//! polls until Cloud answers (:348-364). An approved code's token goes to `finish`, which
//! saves a SignedIn (:568-585). Denied or Expired goes to `forget`, whose `clear` removes the
//! file, whatever it holds (:617-620, :224-229). `poll` reads invalid_grant as Expired
//! (:562). Other Cloud commands read the file through `signed_in` (:276-287), which polls a
//! Pending once and can finish or forget it as well. `ployz logout` reads the file, signs a
//! SignedIn out at Cloud and clears it (:387-446).
//!
//! Each of these runs in its own `ployz` process, so one can step between any two of
//! another's file operations. No lock covers them, and no write compares the file with what
//! its process read. `save` writes a temp file and renames it over the file (:211-222), so a
//! reader sees the old file or the new one, never a partial one.
//!
//! Cloud answers the token poll from better-auth's device authorization plugin, which the
//! dashboard wires at dashboard/src/server/auth.server.ts:268-276. The first token request
//! for an approved code consumes its row. A later request finds no row and gets
//! invalid_grant, as dashboard/src/server/auth.server.postgres.test.ts:258-259 pins.
//!
//! The finding: two `ployz login` processes wait on one code. The first to poll after the
//! browser approves gets the token and saves the SignedIn. The other's next poll gets
//! invalid_grant, reads it as Expired, and its `forget` removes the SignedIn the first just
//! saved.

use std::fmt::Write as _;

use stateright::{Model, Property};

/// The most `ployz login` processes a model runs. The default model runs two.
const MAX_LOGINS: usize = 3;
/// The slot of the other Cloud command, whose `signed_in` reads the file.
const COMMAND: usize = MAX_LOGINS;
/// The slot of `ployz logout`.
const LOGOUT: usize = MAX_LOGINS + 1;
const PROCESSES: usize = MAX_LOGINS + 2;
/// Only `request_code` mints a code, and each login runs it at most once.
const MAX_CODES: usize = MAX_LOGINS;

/// A device code Cloud minted, in the order it was minted. A SignedIn holds the token that
/// code was exchanged for, so the code names the token too.
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct Code(u8);

impl Code {
    fn index(self) -> usize {
        usize::from(self.0)
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "c{}", self.0)
    }
}

impl std::fmt::Debug for Code {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self}")
    }
}

/// What `cloud.json` holds (`enum Stored`, :168-173).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum File {
    /// No file: `load` returns `Ok(None)` (:198).
    Absent,
    /// A file that is not a `Stored`: `load` returns `Corrupt` (:191-196), and `replaceable`
    /// reads it as none (:204-209). Another release or an edit writes it; the model starts
    /// from it.
    Unreadable,
    /// `Stored::Pending` holding this device code.
    Pending(Code),
    /// `Stored::SignedIn` holding the token this code was exchanged for.
    SignedIn(Code),
}

impl std::fmt::Display for File {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absent => formatter.write_str("none"),
            Self::Unreadable => formatter.write_str("unreadable"),
            Self::Pending(code) => write!(formatter, "Pending({code})"),
            Self::SignedIn(code) => write!(formatter, "SignedIn({code})"),
        }
    }
}

/// Cloud's row for one device code, as better-auth's `redeemDeviceCode` reads it, and the
/// session its token opened.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Grant {
    /// Not minted yet.
    Unminted,
    /// Status pending: a poll gets authorization_pending.
    Waiting,
    /// The browser approved it: the next poll gets the token.
    Approved,
    /// The browser denied it: the next poll gets access_denied, which deletes the row.
    Denied,
    /// Past its expiry. The client's own clock says so first, and `poll` answers Expired
    /// without asking Cloud (:535-536).
    Lapsed,
    /// A poll got the token, which consumed the row; every later poll gets invalid_grant.
    /// `ended` says a logout signed the token's session out at Cloud.
    Spent { ended: bool },
    /// access_denied deleted the row; every later poll gets invalid_grant.
    Swept,
}

/// What `poll` (:534-564) returned, read from Cloud's answer.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Answer {
    /// authorization_pending (:558).
    Pending,
    /// The token (:548-550).
    Token,
    /// access_denied (:560).
    Denied,
    /// The code's expiry passed; `poll` answers Expired without asking (:535-536).
    Lapsed,
    /// invalid_grant for a code another poll spent; `poll` reads it as Expired (:562).
    Spent,
    /// invalid_grant for a code access_denied deleted; `poll` reads it as Expired (:562).
    Swept,
}

impl Answer {
    /// The error `forget` returns for it (:360-361, :529-530).
    fn outcome(self) -> Outcome {
        match self {
            Self::Denied => Outcome::Denied,
            Self::Lapsed | Self::Spent | Self::Swept => Outcome::Expired,
            Self::Pending | Self::Token => unreachable!("{self:?} forgets nothing"),
        }
    }
}

/// Where `forget` goes once it has cleared the file.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Then {
    /// The process returns the error.
    Return,
    /// `start`'s resume branch requests a new code (:332-334).
    Request,
}

/// What a process returned.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Outcome {
    SignedIn,
    /// `LoginError::Expired`, "the sign-in code expired before it was approved" (:67).
    Expired,
    /// `LoginError::Denied`, "the sign-in was denied in the browser" (:69).
    Denied,
    /// `signed_in`'s `AwaitingApproval` (:284).
    AwaitingApproval,
    /// `signed_in`'s `SignedOut` (:278).
    SignedOut,
    /// `signed_in`'s `Corrupt` from `load` (:277).
    Unreadable,
    /// `logout` returned.
    LoggedOut,
}

/// Where one process is: its next step.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Pc {
    /// Not started.
    Idle,
    /// `start` read a SignedIn whose session Cloud honours (:307, :315); its save is next
    /// (:321).
    Refreshing(Code),
    /// `start` read a Pending that has not expired (:307, :325); `poll_once`'s poll is next
    /// (:326).
    Resuming(Code),
    /// `request_code` is next (:340, or :333 in the resume branch).
    Requesting,
    /// `start` returned this code's Pending; the wait loop's poll is next (:355-356).
    Waiting(Code),
    /// `signed_in` loaded a Pending (:277, :280); `poll_once`'s poll is next (:282).
    Polling(Code),
    /// A poll returned the token; `finish`'s get-session and save are next (:574-583).
    Finishing(Code),
    /// A poll answered Denied or Expired; `forget`'s `clear` is next (:617-620).
    Forgetting {
        answer: Answer,
        then: Then,
    },
    /// `logout` read this (:397) and, for a SignedIn, Cloud signed it out (:409-414);
    /// `clear` is next (:444, or :399 for none).
    Clearing(File),
    Done(Outcome),
}

/// One process's next step, or the browser's.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// The process in this slot takes its next step.
    Step(usize),
    /// A signed-in member approves the code at `/device`.
    Approve(Code),
    /// A signed-in member denies the code at `/device`.
    Deny(Code),
    /// The code passes its expiry.
    Lapse(Code),
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    file: File,
    grants: [Grant; MAX_CODES],
    pcs: [Pc; PROCESSES],
    /// Each code whose token a process returned signed in since the last logout step.
    kept: [bool; MAX_CODES],
}

impl State {
    fn grant(&self, code: Code) -> Grant {
        *self
            .grants
            .get(code.index())
            .expect("a code the model can mint")
    }

    fn set_grant(&mut self, code: Code, grant: Grant) {
        *self
            .grants
            .get_mut(code.index())
            .expect("a code the model can mint") = grant;
    }

    fn pc(&self, who: usize) -> Pc {
        *self.pcs.get(who).expect("a process slot")
    }

    fn set_pc(&mut self, who: usize, pc: Pc) {
        *self.pcs.get_mut(who).expect("a process slot") = pc;
    }

    /// Cloud honours the session this code's token opened.
    fn honoured(&self, code: Code) -> bool {
        self.grant(code) == Grant::Spent { ended: false }
    }

    fn minted(&self) -> impl Iterator<Item = Code> + '_ {
        self.grants
            .iter()
            .take_while(|grant| **grant != Grant::Unminted)
            .enumerate()
            .map(|(index, _)| Code(u8::try_from(index).expect("a small index")))
    }

    /// Since the last logout step, no process returned a sign-in Cloud still honours, or
    /// `cloud.json` holds a SignedIn.
    fn approved_kept(&self) -> bool {
        !self
            .minted()
            .any(|code| self.kept(code) && self.honoured(code))
            || matches!(self.file, File::SignedIn(_))
    }

    /// A process returned `code`'s token signed in since the last logout step.
    fn kept(&self, code: Code) -> bool {
        *self
            .kept
            .get(code.index())
            .expect("a code the model can mint")
    }
}

#[derive(Clone, Copy, Debug)]
struct SignInStore {
    /// How many `ployz login` processes run, at most `MAX_LOGINS`.
    logins: usize,
}

impl SignInStore {
    /// `request_code` (:489-518): Cloud mints the next code and `save` writes its Pending.
    fn mint(state: &mut State) -> Code {
        let code = Code(u8::try_from(state.minted().count()).expect("a small count"));
        state.set_grant(code, Grant::Waiting);
        code
    }

    /// `poll` (:534-564) for `code`, and what the token request does to Cloud's row.
    fn poll(state: &mut State, code: Code) -> Answer {
        match state.grant(code) {
            Grant::Lapsed => Answer::Lapsed,
            Grant::Waiting => Answer::Pending,
            Grant::Approved => {
                state.set_grant(code, Grant::Spent { ended: false });
                Answer::Token
            }
            Grant::Denied => {
                state.set_grant(code, Grant::Swept);
                Answer::Denied
            }
            Grant::Spent { .. } => Answer::Spent,
            Grant::Swept => Answer::Swept,
            Grant::Unminted => unreachable!("{code} was polled before it was minted"),
        }
    }

    fn signs_in(state: &mut State, code: Code) -> Pc {
        *state
            .kept
            .get_mut(code.index())
            .expect("a code the model can mint") = true;
        Pc::Done(Outcome::SignedIn)
    }

    fn step(state: &mut State, who: usize) {
        let next = match state.pc(who) {
            Pc::Idle if who == LOGOUT => {
                if let File::SignedIn(code) = state.file {
                    state.set_grant(code, Grant::Spent { ended: true });
                }
                state.kept = [false; MAX_CODES];
                Pc::Clearing(state.file)
            }
            Pc::Idle if who == COMMAND => match state.file {
                File::Absent => Pc::Done(Outcome::SignedOut),
                File::Unreadable => Pc::Done(Outcome::Unreadable),
                File::SignedIn(code) => Self::signs_in(state, code),
                File::Pending(code) => Pc::Polling(code),
            },
            Pc::Idle => match state.file {
                File::SignedIn(code) if state.honoured(code) => Pc::Refreshing(code),
                File::Pending(code) if state.grant(code) != Grant::Lapsed => Pc::Resuming(code),
                File::Absent | File::Unreadable | File::Pending(_) | File::SignedIn(_) => {
                    Pc::Requesting
                }
            },
            Pc::Refreshing(code) | Pc::Finishing(code) => {
                state.file = File::SignedIn(code);
                Self::signs_in(state, code)
            }
            Pc::Requesting => {
                let code = Self::mint(state);
                state.file = File::Pending(code);
                Pc::Waiting(code)
            }
            Pc::Resuming(code) => match Self::poll(state, code) {
                Answer::Pending => Pc::Waiting(code),
                Answer::Token => Pc::Finishing(code),
                answer @ (Answer::Denied | Answer::Lapsed | Answer::Spent | Answer::Swept) => {
                    Pc::Forgetting {
                        answer,
                        then: Then::Request,
                    }
                }
            },
            pc @ (Pc::Waiting(code) | Pc::Polling(code)) => match Self::poll(state, code) {
                Answer::Pending if matches!(pc, Pc::Polling(_)) => {
                    Pc::Done(Outcome::AwaitingApproval)
                }
                Answer::Pending => unreachable!("a waiter polls only once Cloud would answer"),
                Answer::Token => Pc::Finishing(code),
                answer @ (Answer::Denied | Answer::Lapsed | Answer::Spent | Answer::Swept) => {
                    Pc::Forgetting {
                        answer,
                        then: Then::Return,
                    }
                }
            },
            Pc::Forgetting { answer, then } => {
                state.file = File::Absent;
                match then {
                    Then::Return => Pc::Done(answer.outcome()),
                    Then::Request => Pc::Requesting,
                }
            }
            Pc::Clearing(_) => {
                state.file = File::Absent;
                state.kept = [false; MAX_CODES];
                Pc::Done(Outcome::LoggedOut)
            }
            Pc::Done(_) => unreachable!("a returned process takes no step"),
        };
        state.set_pc(who, next);
    }

    /// Whether the process in slot `who` runs in this model and has a step to take.
    fn offers(&self, state: &State, who: usize) -> bool {
        match state.pc(who) {
            Pc::Idle => who < self.logins || who == COMMAND || who == LOGOUT,
            // A poll that gets authorization_pending changes nothing, so the wait loop
            // polls only once Cloud would answer otherwise.
            Pc::Waiting(code) => state.grant(code) != Grant::Waiting,
            Pc::Done(_) => false,
            Pc::Refreshing(_)
            | Pc::Resuming(_)
            | Pc::Requesting
            | Pc::Polling(_)
            | Pc::Finishing(_)
            | Pc::Forgetting { .. }
            | Pc::Clearing(_) => true,
        }
    }
}

impl Model for SignInStore {
    type State = State;
    type Action = Action;

    /// An unreadable `cloud.json`, and none, with every process yet to start. Stateright
    /// explores the last start state first, so a story that either can start shows none.
    fn init_states(&self) -> Vec<State> {
        let start = State {
            file: File::Absent,
            grants: [Grant::Unminted; MAX_CODES],
            pcs: [Pc::Idle; PROCESSES],
            kept: [false; MAX_CODES],
        };
        vec![
            State {
                file: File::Unreadable,
                ..start
            },
            start,
        ]
    }

    /// Each process's next step in slot order, then the browser's, code by code. The order
    /// decides only which of the shortest paths a story shows.
    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        actions.extend(
            (0..PROCESSES)
                .filter(|who| self.offers(state, *who))
                .map(Action::Step),
        );
        for code in state.minted() {
            match state.grant(code) {
                Grant::Waiting => actions.extend([
                    Action::Approve(code),
                    Action::Deny(code),
                    Action::Lapse(code),
                ]),
                Grant::Approved | Grant::Denied => actions.push(Action::Lapse(code)),
                Grant::Unminted | Grant::Lapsed | Grant::Spent { .. } | Grant::Swept => {}
            }
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = *last;
        match action {
            Action::Step(who) => Self::step(&mut state, who),
            Action::Approve(code) => state.set_grant(code, Grant::Approved),
            Action::Deny(code) => state.set_grant(code, Grant::Denied),
            Action::Lapse(code) => state.set_grant(code, Grant::Lapsed),
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![Property::always(
            APPROVED_SIGN_IN_KEPT,
            |_, state: &State| state.approved_kept(),
        )]
    }
}

const APPROVED_SIGN_IN_KEPT: &str = "once a process returned a sign-in Cloud still honours, cloud.json holds a SignedIn until a logout steps";

/// The name a story gives the process in slot `who`.
fn name(who: usize) -> &'static str {
    match who {
        0 => "Login A",
        1 => "Login B",
        2 => "Login C",
        COMMAND => "Command",
        LOGOUT => "Logout",
        _ => unreachable!("slot {who} holds no process"),
    }
}

/// `path` in the code's words: each step, then the file, Cloud's codes and each process.
fn story(model: &SignInStore, path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    // Stateright pairs each state with the action taken from it; the row lists the action
    // that led to the state instead, so the last row shows the broken state.
    let mut led_by: Option<(State, Action)> = None;
    for (n, (state, action)) in path.iter().enumerate() {
        let what = led_by.map_or_else(
            || {
                format!(
                    "start: cloud.json is {}; no process has started",
                    state.file
                )
            },
            |(before, action)| reads_as(&before, action, state),
        );
        led_by = action.map(|action| (*state, action));
        let codes: Vec<String> = state
            .minted()
            .map(|code| format!("{code} {:?}", state.grant(code)))
            .collect();
        let processes: Vec<String> = (0..PROCESSES)
            .filter(|who| *who < model.logins || *who >= MAX_LOGINS)
            .map(|who| format!("{}: {:?}", name(who), state.pc(who)))
            .collect();
        let _ = writeln!(
            story,
            "{n:2}. {what}\n    cloud.json: {} | Cloud: {} | {}",
            state.file,
            if codes.is_empty() {
                "no code".to_owned()
            } else {
                codes.join(", ")
            },
            processes.join(" | "),
        );
    }
    story
}

/// What a poll's answer did, in the code's words.
fn answered(answer: Answer, code: Code) -> String {
    match answer {
        Answer::Pending => format!("Cloud answers authorization_pending for {code} (:558)"),
        Answer::Token => format!(
            "Cloud grants {code}'s token and consumes the code; finish's get-session and save are next (:548-550, :574-583)"
        ),
        Answer::Denied => format!("Cloud answers access_denied for {code} and deletes it (:560)"),
        Answer::Lapsed => {
            format!("{code} has expired, so poll answers Expired without asking Cloud (:535-536)")
        }
        Answer::Spent => format!(
            "Cloud answers invalid_grant, because {code} is spent, and poll reads it as Expired (:562)"
        ),
        Answer::Swept => format!(
            "Cloud answers invalid_grant, because access_denied deleted {code}, and poll reads it as Expired (:562)"
        ),
    }
}

/// The answer a poll step got, read from where it left the process.
fn answer_of(after: Pc) -> Answer {
    match after {
        Pc::Forgetting { answer, .. } => answer,
        Pc::Finishing(_) => Answer::Token,
        Pc::Idle
        | Pc::Refreshing(_)
        | Pc::Resuming(_)
        | Pc::Requesting
        | Pc::Waiting(_)
        | Pc::Polling(_)
        | Pc::Clearing(_)
        | Pc::Done(_) => Answer::Pending,
    }
}

/// `action` from `before`, read with `after` for the branch it took.
fn reads_as(before: &State, action: Action, after: &State) -> String {
    let who = match action {
        Action::Approve(code) => {
            return format!(
                "Browser: a signed-in member approves {code} at /device (dashboard/src/server/auth.server.ts:268-273)"
            );
        }
        Action::Deny(code) => {
            return format!("Browser: a signed-in member denies {code} at /device");
        }
        Action::Lapse(code) => {
            return format!(
                "Clock: {code} passes its expiry, better-auth's default 30 minutes, which dashboard/src/server/auth.server.ts:269-272 keeps"
            );
        }
        Action::Step(who) => who,
    };
    let what = match (before.pc(who), after.pc(who)) {
        (Pc::Idle, Pc::Clearing(read)) => match read {
            File::SignedIn(code) => format!(
                "logout: replaceable reads SignedIn({code}) (:397), and POST /api/cli/logout signs its session out at Cloud (:408-414, :431-433); clear is next (:444)"
            ),
            File::Pending(code) => format!(
                "logout: replaceable reads Pending({code}) (:397), which it does not send to Cloud (:402-407); clear is next (:444)"
            ),
            File::Absent | File::Unreadable => format!(
                "logout: replaceable reads {} as none (:204-209, :397); clear is next (:398-399)",
                read
            ),
        },
        (Pc::Idle, Pc::Done(Outcome::SignedOut)) => {
            "signed_in: load finds no cloud.json and returns SignedOut, \"Not signed in to Cloud.\" (:277-278, :65)".to_owned()
        }
        (Pc::Idle, Pc::Done(Outcome::Unreadable)) => {
            "signed_in: load returns Corrupt, \"The sign-in stored at ... is unreadable.\" (:188-196, :277, :50)".to_owned()
        }
        (Pc::Idle, Pc::Done(Outcome::SignedIn)) => format!(
            "signed_in: load reads {} and returns it (:277, :279)",
            before.file
        ),
        (Pc::Idle, Pc::Polling(code)) => format!(
            "signed_in: load reads Pending({code}) (:277, :280); poll_once's poll is next (:282)"
        ),
        (Pc::Idle, Pc::Refreshing(code)) => format!(
            "start: replaceable reads SignedIn({code}) (:307), and get-session honours its token (:315); the save is next (:321)"
        ),
        (Pc::Idle, Pc::Resuming(code)) => format!(
            "start: replaceable reads Pending({code}) (:307), for this Cloud and not expired, so poll_once is next (:325-326)"
        ),
        (Pc::Idle, Pc::Requesting) => format!(
            "start: replaceable reads {}; request_code is next (:340)",
            match before.file {
                File::Absent => "no cloud.json (:198, :338)".to_owned(),
                File::Unreadable => "an unreadable cloud.json as none (:204-209, :338)".to_owned(),
                File::Pending(code) =>
                    format!("Pending({code}), which has expired, so start skips it (:325, :338)"),
                File::SignedIn(code) => format!(
                    "SignedIn({code}), and get-session answers 401 for its ended session, so start falls through (:315, :599-600)"
                ),
            }
        ),
        (Pc::Refreshing(code), _) => format!(
            "start: save writes the refreshed SignedIn({code}) over {} and returns it; ployz login prints \"Signed in to ...\" (:321-322, handlers/login.rs:86, :116-123)",
            before.file
        ),
        (Pc::Requesting, Pc::Waiting(code)) => format!(
            "request_code: POST /api/auth/device/code mints {code}, and save writes Pending({code}) over {} (:494-513); ployz login prints \"Open <url> and confirm the code <code>.\" and waits (handlers/login.rs:98-99, :104-114)",
            before.file
        ),
        (Pc::Resuming(code), next) => {
            let answer = answer_of(after.pc(who));
            format!(
                "start: poll_once's poll: {}{}",
                answered(answer, code),
                match next {
                    Pc::Waiting(_) =>
                        "; start returns the Pending as resumed (:328-331), and ployz login prints the code and waits (handlers/login.rs:98-99)",
                    Pc::Forgetting { .. } => "; forget is next (:529-530), then request_code (:332-333)",
                    Pc::Idle
                    | Pc::Refreshing(_)
                    | Pc::Resuming(_)
                    | Pc::Requesting
                    | Pc::Polling(_)
                    | Pc::Finishing(_)
                    | Pc::Clearing(_)
                    | Pc::Done(_) => "",
                }
            )
        }
        (Pc::Waiting(code), _) => format!(
            "wait: after its sleep, poll: {}{}",
            answered(answer_of(after.pc(who)), code),
            if matches!(after.pc(who), Pc::Forgetting { .. }) {
                "; forget is next (:360-361)"
            } else {
                ""
            }
        ),
        (Pc::Polling(code), Pc::Done(Outcome::AwaitingApproval)) => format!(
            "signed_in: poll_once's poll: {}; signed_in returns AwaitingApproval, \"sign-in is waiting for approval at <url>\" (:528, :283-284, :73)",
            answered(Answer::Pending, code)
        ),
        (Pc::Polling(code), _) => format!(
            "signed_in: poll_once's poll: {}{}",
            answered(answer_of(after.pc(who)), code),
            if matches!(after.pc(who), Pc::Forgetting { .. }) {
                "; forget is next (:529-530)"
            } else {
                ""
            }
        ),
        (Pc::Finishing(code), _) => format!(
            "finish: get-session honours {code}'s token, and save writes SignedIn({code}) over {} (:574-583); {}",
            before.file,
            if who == COMMAND {
                "signed_in returns it, and the command acts with it (:282-283)"
            } else {
                "ployz login prints \"Signed in to <cloud> as <email> in Organization <slug>.\" (:327, :357, handlers/login.rs:86, :100, :116-123)"
            }
        ),
        (Pc::Forgetting { answer, then }, _) => format!(
            "forget: clear removes cloud.json, which held {} (:617-618, :224-229); {}",
            before.file,
            match (then, answer.outcome()) {
                (Then::Request, _) => "start's resume branch goes on to request_code (:332-333)".to_owned(),
                (Then::Return, Outcome::Expired) if who == COMMAND =>
                    "signed_in returns Expired, \"the sign-in code expired before it was approved\" (:619, :67)".to_owned(),
                (Then::Return, Outcome::Expired) =>
                    "ployz login fails, \"the sign-in code expired before it was approved\", next: ployz login (:619, :67, failure.rs:1030-1033)".to_owned(),
                (Then::Return, outcome) => format!("it returns {outcome:?} (:619)"),
            }
        ),
        (Pc::Clearing(read), _) => format!(
            "logout: clear removes cloud.json, which holds {} (:{}); logout prints \"{}\" (handlers/login.rs:151-157)",
            before.file,
            if matches!(read, File::Absent | File::Unreadable) { "399" } else { "444" },
            if matches!(read, File::Absent | File::Unreadable) {
                "Not signed in."
            } else {
                "Signed out of <cloud>."
            }
        ),
        (
            Pc::Idle | Pc::Requesting | Pc::Done(_),
            Pc::Idle
            | Pc::Refreshing(_)
            | Pc::Resuming(_)
            | Pc::Requesting
            | Pc::Polling(_)
            | Pc::Finishing(_)
            | Pc::Forgetting { .. }
            | Pc::Clearing(_)
            | Pc::Waiting(_)
            | Pc::Done(_),
        ) => unreachable!("{:?} cannot lead to {:?}", before.pc(who), after.pc(who)),
    };
    format!("{}: {what}", name(who))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::{Arc, Mutex},
    };

    use stateright::{Checker, Expectation, HasDiscoveries};

    use super::*;

    type Path = Vec<(State, Option<Action>)>;

    const A: usize = 0;
    const B: usize = 1;

    /// The code as it is, with two `ployz login` processes.
    fn as_is() -> SignInStore {
        SignInStore { logins: 2 }
    }

    /// As `breaks`, the first such path that also satisfies `and`. It explores the whole
    /// model, since the first discovery may not pass `and`. The path is the first in BFS
    /// order whose last state breaks `property` and passes `and`, printed as a story under
    /// the test's name. The checker's own discovery is not that path: `check_block` stores
    /// every violation it visits, each overwriting the last, so a visitor keeps the first.
    /// One thread keeps the path the same from run to run.
    fn breaks_where(
        model: SignInStore,
        property: &'static str,
        and: impl Fn(&Path) -> bool + Send + Sync + 'static,
    ) -> Path {
        first_break(model, property, and, false).expect("a path breaks the property")
    }

    /// As `breaks_where`, or `None` when no path breaks `property` and satisfies `and`.
    fn first_break(
        model: SignInStore,
        property: &'static str,
        and: impl Fn(&Path) -> bool + Send + Sync + 'static,
        stop_at_discovery: bool,
    ) -> Option<Path> {
        let named = model
            .properties()
            .into_iter()
            .find(|candidate| candidate.name == property)
            .expect("a model property");
        let reached = matches!(named.expectation, Expectation::Sometimes);
        let holds = named.condition;
        let first = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&first);
        let checker =
            model
                .checker()
                .threads(1)
                .visitor(move |path: stateright::Path<State, Action>| {
                    let mut first = seen.lock().expect("the first break");
                    if first.is_none() && holds(&model, path.last_state()) == reached {
                        let path = path.into_vec();
                        if and(&path) {
                            *first = Some(path);
                        }
                    }
                });
        // An empty `AnyOf` never matches, so the checker visits every state.
        let checker = checker.finish_when(if stop_at_discovery {
            HasDiscoveries::AllOf([property].into())
        } else {
            HasDiscoveries::AnyOf(BTreeSet::new())
        });
        checker.spawn_bfs().join();
        let path = first.lock().expect("the first break").take()?;
        let test = std::thread::current();
        println!(
            "[{}] {property} {} under {model:?}:\n{}",
            test.name().unwrap_or("?"),
            if reached { "is reached" } else { "fails" },
            story(&model, &path)
        );
        Some(path)
    }

    fn last(path: &Path) -> State {
        path.last().expect("a path has a state").0
    }

    /// Each step `who` took, as the process's place before and after it.
    fn moves(path: &Path, who: usize) -> Vec<(Pc, Pc)> {
        path.windows(2)
            .filter_map(|pair| match pair {
                [(before, Some(Action::Step(stepped))), (after, _)] if *stepped == who => {
                    Some((before.pc(who), after.pc(who)))
                }
                _ => None,
            })
            .collect()
    }

    /// `who` took a step from `from` to a place that satisfies `to`.
    fn moved(path: &Path, who: usize, from: fn(Pc) -> bool, to: fn(Pc) -> bool) -> bool {
        moves(path, who)
            .into_iter()
            .any(|(before, after)| from(before) && to(after))
    }

    /// Steps `actions` from the start with no `cloud.json` under `model`, each one offered
    /// where it is taken, and prints the path as a story under the test's name.
    fn follow(model: SignInStore, actions: &[Action]) -> Path {
        let mut state = model
            .init_states()
            .into_iter()
            .find(|start| start.file == File::Absent)
            .expect("a start with no cloud.json");
        let mut path = Vec::new();
        for &action in actions {
            let mut offered = Vec::new();
            model.actions(&state, &mut offered);
            assert!(offered.contains(&action), "{action:?} is offered");
            let next = model.next_state(&state, action).expect("a next state");
            path.push((state, Some(action)));
            state = next;
        }
        path.push((state, None));
        let test = std::thread::current();
        println!(
            "[{}] a schedule under {model:?}:\n{}",
            test.name().unwrap_or("?"),
            story(&model, &path)
        );
        path
    }

    /// The finding. Login A's `start` finds no file, and `request_code` saves Pending(c0).
    /// Login B's `start` reads it, its poll gets authorization_pending, and it waits too
    /// (:325-331). The browser approves; A's poll gets the token, and `finish` saves
    /// SignedIn(c0). B's next poll gets invalid_grant for the spent code, which `poll` reads
    /// as Expired (:562), and B's `forget` removes the file (:361, :617-620). A printed
    /// "Signed in", B printed "the sign-in code expired before it was approved", and
    /// `cloud.json` is gone.
    ///
    /// The checker keeps one path to each state, the first in BFS order. A shorter path ends
    /// in the same state: B requests c0, and A's `start` resumes it after the approval and
    /// gets the token with its one poll. So the test replays this schedule with `follow`,
    /// and then asks the checker for its first path to the same broken state.
    #[test]
    fn current_code_lets_a_losing_waiter_forget_an_approved_sign_in() {
        let c0 = Code(0);
        let path = follow(
            as_is(),
            &[
                Action::Step(A),
                Action::Step(A),
                Action::Step(B),
                Action::Step(B),
                Action::Approve(c0),
                Action::Step(A),
                Action::Step(A),
                Action::Step(B),
                Action::Step(B),
            ],
        );
        assert!(moved(
            &path,
            A,
            |pc| pc == Pc::Requesting,
            |pc| pc == Pc::Waiting(Code(0))
        ));
        assert!(moved(
            &path,
            B,
            |pc| pc == Pc::Resuming(Code(0)),
            |pc| pc == Pc::Waiting(Code(0))
        ));
        assert!(moved(
            &path,
            A,
            |pc| pc == Pc::Waiting(Code(0)),
            |pc| pc == Pc::Finishing(Code(0))
        ));
        assert!(moved(
            &path,
            B,
            |pc| pc == Pc::Waiting(Code(0)),
            |pc| pc
                == Pc::Forgetting {
                    answer: Answer::Spent,
                    then: Then::Return,
                }
        ));
        let broken = last(&path);
        assert_eq!(broken.file, File::Absent);
        assert_eq!(broken.pc(A), Pc::Done(Outcome::SignedIn));
        assert_eq!(broken.pc(B), Pc::Done(Outcome::Expired));
        assert!(broken.kept(c0));
        assert!(broken.honoured(c0), "Cloud still honours A's session");
        assert!(!broken.approved_kept());
        let found = breaks_where(as_is(), APPROVED_SIGN_IN_KEPT, move |path| {
            last(path) == broken
        });
        assert_eq!(last(&found), broken);
    }

    /// The same broken state by another route: B's `start` read the file before A saved,
    /// and B's `request_code` saves its new Pending over A's SignedIn. B's POST to
    /// /api/auth/device/code (:494-499) has to outlast A's approval, poll and save.
    #[test]
    fn a_slow_code_request_overwrites_an_approved_sign_in() {
        let path = breaks_where(as_is(), APPROVED_SIGN_IN_KEPT, |path| {
            let [.., (before, Some(Action::Step(B))), (after, None)] = path.as_slice() else {
                return false;
            };
            before.pc(B) == Pc::Requesting
                && before.approved_kept()
                && matches!(before.file, File::SignedIn(_))
                && matches!(after.file, File::Pending(_))
        });
        let broken = last(&path);
        assert_eq!(broken.file, File::Pending(Code(1)));
        assert_eq!(broken.pc(A), Pc::Done(Outcome::SignedIn));
        assert_eq!(broken.pc(B), Pc::Waiting(Code(1)));
        assert!(moved(
            &path,
            B,
            |pc| pc == Pc::Idle,
            |pc| pc == Pc::Requesting
        ));
    }

    /// `start`'s resume branch (:325-336). B reads Pending(c0) before A saves, and its one
    /// poll lands after A's poll spent c0. B's `forget` removes A's SignedIn (:530), and its
    /// `request_code` then saves a new Pending (:333), so `cloud.json` ends holding B's code
    /// and B waits on it.
    #[test]
    fn a_late_resume_replaces_an_approved_sign_in() {
        let path = breaks_where(as_is(), APPROVED_SIGN_IN_KEPT, |path| {
            let [.., (before, Some(Action::Step(B))), (_, None)] = path.as_slice() else {
                return false;
            };
            before.pc(B)
                == Pc::Forgetting {
                    answer: Answer::Spent,
                    then: Then::Request,
                }
                && before.approved_kept()
                && matches!(before.file, File::SignedIn(_))
        });
        let broken = last(&path);
        assert_eq!(broken.file, File::Absent);
        assert_eq!(broken.pc(A), Pc::Done(Outcome::SignedIn));
        assert_eq!(broken.pc(B), Pc::Requesting);
        let model = as_is();
        let then = model
            .next_state(&broken, Action::Step(B))
            .expect("B requests a code");
        println!(
            "[a_late_resume_replaces_an_approved_sign_in] then:\n{}",
            story(&model, &[(broken, Some(Action::Step(B))), (then, None)])
        );
        assert_eq!(then.file, File::Pending(Code(1)));
        assert_eq!(then.pc(B), Pc::Waiting(Code(1)));
        assert!(!then.approved_kept());
    }

    /// A Cloud command's `signed_in` loads Pending(c0) and its one poll gets the token, so
    /// its `finish` saves SignedIn(c0) (:282, :527, :568-585). Login A's next poll then gets
    /// invalid_grant for the code the command spent, and A's `forget` removes the command's
    /// sign-in (:361, :617-620). A prints "the sign-in code expired before it was approved".
    #[test]
    fn a_waiter_forgets_a_sign_in_a_cloud_command_finished() {
        let path = breaks_where(as_is(), APPROVED_SIGN_IN_KEPT, |path| {
            let [.., (before, Some(Action::Step(A))), (_, None)] = path.as_slice() else {
                return false;
            };
            matches!(
                before.pc(A),
                Pc::Forgetting {
                    then: Then::Return,
                    ..
                }
            ) && before.approved_kept()
                && matches!(before.file, File::SignedIn(_))
                && moved(
                    path,
                    COMMAND,
                    |pc| matches!(pc, Pc::Finishing(_)),
                    |pc| pc == Pc::Done(Outcome::SignedIn),
                )
        });
        let broken = last(&path);
        assert_eq!(broken.file, File::Absent);
        assert_eq!(broken.pc(COMMAND), Pc::Done(Outcome::SignedIn));
        assert_eq!(broken.pc(A), Pc::Done(Outcome::Expired));
    }

    /// The property watches every token returned since the last logout, not only the last.
    /// Three logins: B's `start` reads no file before A saves c0, and C reads SignedIn(c0) and
    /// waits to save its refresh. A logout signs c0 out and clears the file. B requests c1,
    /// the command loads Pending(c1), and B signs in with c1. C's stale refresh saves the
    /// ended c0 and returns it, and the command's poll then gets invalid_grant and removes the
    /// file. B's c1 is still honoured and no file holds a SignedIn. A predicate that read only
    /// the last returned token, c0, would hold here.
    #[test]
    fn a_stale_refresh_does_not_hide_a_live_sign_in() {
        const C: usize = 2;
        let model = SignInStore { logins: 3 };
        let path = follow(
            model,
            &[
                Action::Step(A),
                Action::Step(B),
                Action::Step(A),
                Action::Approve(Code(0)),
                Action::Step(A),
                Action::Step(A),
                Action::Step(C),
                Action::Step(LOGOUT),
                Action::Step(LOGOUT),
                Action::Step(B),
                Action::Step(COMMAND),
                Action::Approve(Code(1)),
                Action::Step(B),
                Action::Step(B),
                Action::Step(C),
                Action::Step(COMMAND),
                Action::Step(COMMAND),
            ],
        );
        let broken = last(&path);
        assert_eq!(broken.file, File::Absent);
        assert_eq!(broken.pc(B), Pc::Done(Outcome::SignedIn));
        assert_eq!(broken.pc(C), Pc::Done(Outcome::SignedIn));
        assert!(broken.kept(Code(0)) && !broken.honoured(Code(0)));
        assert!(broken.kept(Code(1)) && broken.honoured(Code(1)));
        assert!(!broken.approved_kept());
    }
}
