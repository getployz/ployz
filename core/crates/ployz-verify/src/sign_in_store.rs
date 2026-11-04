//! This device's Cloud sign-in file, `cloud.json`, and the processes that read and write it
//! while a sign-in waits for the browser. A bare `:n` is a line of the file named just before
//! it in the same parentheses, and otherwise of `core/crates/ployz/src/cloud_login.rs`.
//!
//! `ployz login` without `--json` waits (core/crates/ployz/src/handlers/login.rs:83, :99). Its
//! `start` reads the file with `replaceable` (:307). It refreshes a SignedIn Cloud still honours
//! (:314-323), resumes a Pending that has not expired with one poll (:325-337), and otherwise asks
//! Cloud for a new code and saves it as a Pending (`request_code`, :340, :489-518). `wait` then
//! polls until Cloud answers (:348-364). An approved code's token goes to `finish`, which saves a
//! SignedIn (:568-585). Denied or Expired goes to `forget`, whose `clear` removes the file,
//! whatever it holds (:617-620, :224-229). `poll` reads invalid_grant as Expired (:562). Other
//! Cloud commands read the file through `signed_in` (:276-287), which polls a Pending once and can
//! finish or forget it as well. `ployz logout` reads the file, signs a SignedIn out at Cloud and
//! clears it (:387-446).
//!
//! Each of these runs in its own `ployz` process, so one can step between any two of
//! another's file operations. No lock covers them, and no write compares the file with what
//! its process read. `save` writes a temp file and renames it over the file (:211-222), so a
//! reader sees the old file or the new one, never a partial one.
//!
//! Cloud answers the token poll from better-auth's device authorization plugin, which the
//! dashboard wires at dashboard/src/server/auth.server.ts:268-277. The first token request
//! for an approved code consumes its row. A later request finds no row and gets
//! invalid_grant, as dashboard/src/server/auth.server.postgres.test.ts:259-260 pins.
//!
//! The finding: two `ployz login` processes wait on one code. The first to poll after the
//! browser approves gets the token and saves the SignedIn. The other's next poll gets
//! invalid_grant, reads it as Expired, and its `forget` removes the SignedIn the first just
//! saved.
//!
//! The fix variant takes a lock around each of its writes. Its `forget` removes the file
//! only while it holds the Pending of the code its process polled. Its `request_code` POSTs
//! without the lock, then under it saves only over the file as its process last knew it; if
//! the file changed, it throws the new code away and `start` reads the file again.

use std::fmt::Write as _;

use stateright::{Model, Property};

/// The most `ployz login` processes a model runs. The default model runs two.
const MAX_LOGINS: usize = 3;
/// The slot of the other Cloud command, whose `signed_in` reads the file.
const COMMAND: usize = MAX_LOGINS;
/// The slot of `ployz logout`.
const LOGOUT: usize = MAX_LOGINS + 1;
const PROCESSES: usize = MAX_LOGINS + 2;
/// Only `request_code` mints a code, and each login saves at most one.
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
    /// reads it as none (:204-209). Another release or an edit writes it, and the model
    /// starts from it. Under Broken, a save half-written reads this way too.
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
    /// `logout` returned, after it read this.
    LoggedOut(File),
}

/// Where one process is: its next step.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Pc {
    /// Not started, or, under the fix, about to read the file again.
    Idle,
    /// `start` read a SignedIn whose session Cloud honours (:307, :315); its save is next
    /// (:321).
    Refreshing(Code),
    /// `start` read a Pending that has not expired (:307, :325); `poll_once`'s poll is next
    /// (:326).
    Resuming(Code),
    /// `request_code` is next (:340, or :333 in the resume branch). `expect` is the file the
    /// fix's save must still find, and `None` when the save writes over anything, as the
    /// code's does.
    Requesting {
        expect: Option<File>,
    },
    /// `start` returned this code's Pending; the wait loop's poll is next (:355-356).
    Waiting(Code),
    /// `signed_in` loaded a Pending (:277, :280); `poll_once`'s poll is next (:282).
    Polling(Code),
    /// A poll returned the token; `finish`'s get-session and save are next (:574-583).
    Finishing(Code),
    /// A poll of `code` answered Denied or Expired; `forget` is next (:617-620).
    Forgetting {
        code: Code,
        answer: Answer,
        then: Then,
    },
    /// Under Broken, the fix's `forget` read its own Pending without the lock; its
    /// `remove_file` is next.
    Removing {
        code: Code,
        answer: Answer,
        then: Then,
    },
    /// Under Broken, the fix's `request_code` compared the file under the lock before its
    /// POST, found what `start` read, and released the lock; the POST and a save with no
    /// compare are next.
    Posting,
    /// Under Broken, a save wrote half of this, and the file reads unreadable until the rest
    /// is written.
    Torn(File),
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

/// Which kind of process took a step.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Role {
    Login,
    Command,
}

/// Which poll answered.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Poller {
    /// `start`'s `poll_once` (:326).
    Resume,
    /// The wait loop's (:356).
    Wait,
    /// `signed_in`'s `poll_once` (:282).
    Command,
}

/// What a forget found in the file.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Found {
    Absent,
    Unreadable,
    /// The Pending of the code its process polled.
    Own,
    /// The Pending of another code.
    Other,
    SignedIn,
}

impl Found {
    fn of(file: File, polled: Code) -> Self {
        match file {
            File::Absent => Self::Absent,
            File::Unreadable => Self::Unreadable,
            File::Pending(code) if code == polled => Self::Own,
            File::Pending(_) => Self::Other,
            File::SignedIn(_) => Self::SignedIn,
        }
    }
}

/// The branch the last step took.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Branch {
    /// `start` read this and goes on to `request_code` (:338-340).
    StartRequests(File),
    /// `request_code` saved a new Pending (:513).
    Requested,
    /// The fix's `request_code` found the file changed since `start` read it, and `start`
    /// reads it again.
    RequestRereads,
    /// A poll answered.
    Polled { by: Poller, answer: Answer },
    /// `finish` saved a SignedIn (:583).
    Finished(Role),
    /// `start` saved the refreshed SignedIn (:321).
    Refreshed,
    /// `forget` ran: what it found, whether it removed the file, the answer that sent it
    /// there, and where its process goes next.
    Forgot {
        role: Role,
        found: Found,
        removed: bool,
        answer: Answer,
        then: Then,
    },
    /// `signed_in` read a SignedIn and returned it (:279).
    CommandReturnsStored,
    /// `signed_in` found no file (:278).
    CommandSignedOut,
    /// `signed_in`'s `load` returned Corrupt (:277).
    CommandUnreadable,
    /// `logout` read a SignedIn, and Cloud signed its session out (:408-414).
    LogoutSignsOut,
    /// `logout`'s `clear` removed the file: what logout read, and what the file held when
    /// `clear` removed it.
    LogoutCleared { read: File, found: File },
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    file: File,
    grants: [Grant; MAX_CODES],
    pcs: [Pc; PROCESSES],
    /// Each code whose token a process returned signed in since the last logout step.
    kept: [bool; MAX_CODES],
    /// The last step read the file while another process's save was half-written.
    torn_read: bool,
    took: Option<Branch>,
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

    /// The file a reader sees, noting a half-written save under it.
    fn read(&mut self) -> File {
        self.torn_read =
            self.file == File::Unreadable && self.pcs.iter().any(|pc| matches!(pc, Pc::Torn(_)));
        self.file
    }
}

/// A guard taken out, to show the check catches what it prevents. The first is a guard the
/// code has; the other four are parts of the fix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag removes one independent guard"
)]
struct Broken {
    /// `save` writes the file in place, in two steps, instead of a temp file renamed over
    /// it (:215-220).
    save_tears: bool,
    /// The fix's `forget` removes the file whatever it holds, as the code's does.
    fix_forgets_any_file: bool,
    /// The fix's `forget` compares and removes in two steps, with no lock between them.
    fix_forget_unlocked: bool,
    /// The fix's `request_code` compares before its POST, and saves after it with no compare.
    fix_compares_before_post: bool,
    /// The fix's `request_code` saves over whatever the file holds, as the code's does.
    fix_request_overwrites: bool,
}

/// Which model a `COVERAGE` row is reached on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    Both,
    /// Only on the code as it is: the row is a removal the fix takes away.
    AsIs,
    /// Only with the fix.
    Fix,
}

/// One `sometimes`: a branch the code handles, read from the state that took it.
struct Coverage {
    name: &'static str,
    reach: Reach,
    reached: fn(&SignInStore, &State) -> bool,
}

fn took(state: &State, branch: Branch) -> bool {
    state.took == Some(branch)
}

fn polled(state: &State, by: Poller, answer: Answer) -> bool {
    took(state, Branch::Polled { by, answer })
}

fn forgot(state: &State, role: Role, found: Found, removed: bool) -> bool {
    matches!(
        state.took,
        Some(Branch::Forgot { role: r, found: f, removed: d, .. })
            if r == role && f == found && d == removed
    )
}

fn logins_waiting_on(state: &State, code: Code) -> usize {
    (0..MAX_LOGINS)
        .filter(|who| state.pc(*who) == Pc::Waiting(code))
        .count()
}

/// Each branch the bounds reach, on the code as it is, on the fix, or on both.
/// `current_code_breaks_only_the_finding` and `the_fix_keeps_an_approved_sign_in` expect
/// every row their model reaches.
const COVERAGE: &[Coverage] = &[
    Coverage {
        name: "start finds no cloud.json and requests a code",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::StartRequests(File::Absent)),
    },
    Coverage {
        name: "start reads an unreadable cloud.json as none and requests a code",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::StartRequests(File::Unreadable)),
    },
    Coverage {
        name: "start skips an expired Pending and requests a code",
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::StartRequests(File::Pending(code))) if state.grant(code) == Grant::Lapsed),
    },
    Coverage {
        name: "start finds a SignedIn whose session Cloud ended and requests a code",
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::StartRequests(File::SignedIn(code))) if !state.honoured(code)),
    },
    Coverage {
        name: "request_code saves a new Pending",
        reach: Reach::Both,
        reached: |_, state| {
            took(state, Branch::Requested) && matches!(state.file, File::Pending(_))
        },
    },
    Coverage {
        name: "start refreshes a stored SignedIn and returns it",
        reach: Reach::Both,
        reached: |_, state| {
            took(state, Branch::Refreshed)
                && matches!(state.file, File::SignedIn(code) if state.honoured(code))
        },
    },
    Coverage {
        name: "start's refresh saves a SignedIn whose session a logout ended meanwhile",
        reach: Reach::Both,
        reached: |_, state| {
            took(state, Branch::Refreshed)
                && matches!(state.file, File::SignedIn(code) if !state.honoured(code))
        },
    },
    Coverage {
        name: "a second waiter resumes the code",
        reach: Reach::Both,
        reached: |_, state| {
            polled(state, Poller::Resume, Answer::Pending)
                && state
                    .minted()
                    .any(|code| logins_waiting_on(state, code) >= 2)
        },
    },
    Coverage {
        name: "start's poll_once gets the token of a stored Pending",
        reach: Reach::Both,
        reached: |_, state| polled(state, Poller::Resume, Answer::Token),
    },
    Coverage {
        name: "start's poll_once gets access_denied",
        reach: Reach::Both,
        reached: |_, state| polled(state, Poller::Resume, Answer::Denied),
    },
    Coverage {
        name: "start's poll_once gets invalid_grant for a spent code",
        reach: Reach::Both,
        reached: |_, state| polled(state, Poller::Resume, Answer::Spent),
    },
    Coverage {
        name: "a waiter signs in",
        reach: Reach::Both,
        reached: |_, state| {
            took(state, Branch::Finished(Role::Login)) && matches!(state.file, File::SignedIn(_))
        },
    },
    Coverage {
        name: "the wait loop gets access_denied",
        reach: Reach::Both,
        reached: |_, state| polled(state, Poller::Wait, Answer::Denied),
    },
    Coverage {
        name: "the wait loop finds its code expired",
        reach: Reach::Both,
        reached: |_, state| polled(state, Poller::Wait, Answer::Lapsed),
    },
    Coverage {
        name: "the wait loop gets invalid_grant for a spent code",
        reach: Reach::Both,
        reached: |_, state| polled(state, Poller::Wait, Answer::Spent),
    },
    Coverage {
        name: "the wait loop gets invalid_grant for a code access_denied deleted",
        reach: Reach::Both,
        reached: |_, state| polled(state, Poller::Wait, Answer::Swept),
    },
    Coverage {
        name: "signed_in answers SignedOut with no cloud.json",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::CommandSignedOut),
    },
    Coverage {
        name: "signed_in fails on an unreadable cloud.json",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::CommandUnreadable),
    },
    Coverage {
        name: "signed_in returns a stored SignedIn",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::CommandReturnsStored),
    },
    Coverage {
        name: "signed_in's poll_once answers AwaitingApproval",
        reach: Reach::Both,
        reached: |_, state| {
            polled(state, Poller::Command, Answer::Pending)
                && state.pc(COMMAND) == Pc::Done(Outcome::AwaitingApproval)
        },
    },
    Coverage {
        name: "signed_in's poll_once gets the token and finish saves SignedIn",
        reach: Reach::Both,
        reached: |_, state| {
            took(state, Branch::Finished(Role::Command)) && matches!(state.file, File::SignedIn(_))
        },
    },
    Coverage {
        name: "signed_in's poll_once gets invalid_grant for a spent code",
        reach: Reach::Both,
        reached: |_, state| polled(state, Poller::Command, Answer::Spent),
    },
    Coverage {
        name: "logout signs a SignedIn out at Cloud",
        reach: Reach::Both,
        reached: |_, state| {
            took(state, Branch::LogoutSignsOut)
                && matches!(state.pc(LOGOUT), Pc::Clearing(File::SignedIn(code)) if !state.honoured(code))
        },
    },
    Coverage {
        name: "logout clears a Pending it did not send to Cloud",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::LogoutCleared {
                    read: File::Pending(_),
                    ..
                })
            ) && state.file == File::Absent
        },
    },
    Coverage {
        name: "logout clears an unreadable cloud.json",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::LogoutCleared {
                    read: File::Unreadable,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "logout finds no cloud.json",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::LogoutCleared {
                    read: File::Absent,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "a logout's clear removes a SignedIn saved after its read",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::LogoutCleared { read, found: found @ File::SignedIn(_) }) if read != found
            )
        },
    },
    Coverage {
        name: "a waiter signs in after a logout read its Pending and cleared cloud.json",
        reach: Reach::Both,
        reached: |_, state| {
            took(state, Branch::Finished(Role::Login))
                && matches!(
                    (state.file, state.pc(LOGOUT)),
                    (File::SignedIn(signed), Pc::Done(Outcome::LoggedOut(File::Pending(read))))
                        if signed == read
                )
        },
    },
    Coverage {
        name: "a forget removes a SignedIn",
        reach: Reach::AsIs,
        reached: |_, state| {
            forgot(state, Role::Login, Found::SignedIn, true)
                || forgot(state, Role::Command, Found::SignedIn, true)
        },
    },
    Coverage {
        name: "the fix's forget removes its own Pending",
        reach: Reach::Fix,
        reached: |_, state| {
            forgot(state, Role::Login, Found::Own, true) && state.file == File::Absent
        },
    },
    Coverage {
        name: "the fix's forget leaves a SignedIn",
        reach: Reach::Fix,
        reached: |_, state| {
            forgot(state, Role::Login, Found::SignedIn, false)
                && matches!(state.file, File::SignedIn(_))
        },
    },
    Coverage {
        name: "the fix's forget leaves another code's Pending",
        reach: Reach::Fix,
        reached: |_, state| {
            forgot(state, Role::Login, Found::Other, false)
                && matches!(state.file, File::Pending(_))
        },
    },
    Coverage {
        name: "the fix's forget finds no cloud.json",
        reach: Reach::Fix,
        reached: |_, state| forgot(state, Role::Login, Found::Absent, false),
    },
    Coverage {
        name: "signed_in's forget leaves a SignedIn under the fix",
        reach: Reach::Fix,
        reached: |_, state| {
            forgot(state, Role::Command, Found::SignedIn, false)
                && matches!(state.file, File::SignedIn(_))
        },
    },
    Coverage {
        name: "the fix's request_code finds cloud.json changed and start reads it again",
        reach: Reach::Fix,
        reached: |_, state| took(state, Branch::RequestRereads),
    },
    Coverage {
        name: "under the fix, a waiter whose poll got invalid_grant still fails Expired beside the SignedIn",
        reach: Reach::Fix,
        reached: |_, state| {
            state.took
                == Some(Branch::Forgot {
                    role: Role::Login,
                    found: Found::SignedIn,
                    removed: false,
                    answer: Answer::Spent,
                    then: Then::Return,
                })
        },
    },
];

#[derive(Clone, Copy, Debug)]
struct SignInStore {
    /// How many `ployz login` processes run, at most `MAX_LOGINS`.
    logins: usize,
    /// The fix: a locked `forget` that removes only its own Pending, and a locked
    /// `request_code` save that writes only over the file `start` read.
    fix: bool,
    broken: Broken,
}

/// The fix's `forget` removes the file only while it holds the Pending of the code its
/// process polled.
fn forgets(file: File, polled: Code) -> bool {
    file == File::Pending(polled)
}

impl SignInStore {
    fn role(who: usize) -> Role {
        if who == COMMAND {
            Role::Command
        } else {
            Role::Login
        }
    }

    /// What `request_code`'s save must still find: the file `start` read under the fix, and
    /// nothing, so it writes over anything, as the code does.
    fn expect(&self, read: File) -> Option<File> {
        (self.fix && !self.broken.fix_request_overwrites).then_some(read)
    }

    /// The fix's `forget` reads and compares the file before it removes it.
    fn compares(&self) -> bool {
        self.fix && !self.broken.fix_forgets_any_file
    }

    /// `request_code` (:489-518): Cloud mints the next code.
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

    /// Where a save of `file` leaves its process: a Pending's login waits, and a
    /// SignedIn's process returns it.
    fn saved(state: &mut State, file: File) -> Pc {
        match file {
            File::Pending(code) => Pc::Waiting(code),
            File::SignedIn(code) => {
                *state
                    .kept
                    .get_mut(code.index())
                    .expect("a code the model can mint") = true;
                Pc::Done(Outcome::SignedIn)
            }
            File::Absent | File::Unreadable => unreachable!("save writes a Stored"),
        }
    }

    /// `save` (:211-222): a temp file renamed over `cloud.json`, or, under Broken, the first
    /// half of a write in place.
    fn save(&self, state: &mut State, file: File) -> Pc {
        if self.broken.save_tears {
            state.file = File::Unreadable;
            Pc::Torn(file)
        } else {
            state.file = file;
            Self::saved(state, file)
        }
    }

    /// `forget` after a poll of `code` answered: `clear` (:617-620, :224-229), or the fix's
    /// locked compare and removal.
    /// The code's `clear` reads nothing, so only the fix's compare reads the file.
    fn forget(&self, state: &mut State, who: usize, code: Code, answer: Answer, then: Then) -> Pc {
        let found = Found::of(
            if self.compares() {
                state.read()
            } else {
                state.file
            },
            code,
        );
        let removes = !self.compares() || forgets(state.file, code);
        if removes && self.compares() && self.broken.fix_forget_unlocked {
            return Pc::Removing { code, answer, then };
        }
        if removes {
            state.file = File::Absent;
        }
        state.took = Some(Branch::Forgot {
            role: Self::role(who),
            found,
            removed: removes,
            answer,
            then,
        });
        self.after_forget(code, answer, then, removes)
    }

    /// Where a process goes once its `forget` ran. The resume branch's `request_code` must
    /// still find the file as this process last knew it: none after it removed its own
    /// Pending, and otherwise the Pending its `start` read, which the file no longer holds.
    fn after_forget(&self, code: Code, answer: Answer, then: Then, removed: bool) -> Pc {
        match then {
            Then::Return => Pc::Done(answer.outcome()),
            Then::Request => Pc::Requesting {
                expect: self.expect(if removed {
                    File::Absent
                } else {
                    File::Pending(code)
                }),
            },
        }
    }

    /// `request_code`'s POST and save (:494-513). Under the fix the POST runs without the
    /// lock, and the save then compares the file with `expect` under it: on a match it
    /// saves, and otherwise it throws the new code away and `start` reads the file again.
    /// The POST changes only Cloud's new row, which no other process reads, so the model
    /// takes the POST, the compare and the save as one step, and leaves a thrown-away code
    /// unminted: no process ever knows it.
    fn request(&self, state: &mut State, expect: Option<File>) -> Pc {
        if let Some(expected) = expect
            && state.read() != expected
        {
            state.took = Some(Branch::RequestRereads);
            return Pc::Idle;
        }
        if expect.is_some() && self.broken.fix_compares_before_post {
            return Pc::Posting;
        }
        self.post_and_save(state)
    }

    /// `request_code`'s POST mints a code, and its save writes the Pending (:494-513).
    fn post_and_save(&self, state: &mut State) -> Pc {
        let code = Self::mint(state);
        state.took = Some(Branch::Requested);
        self.save(state, File::Pending(code))
    }

    /// A poll by `poller` of `code`, and where its answer sends the process.
    fn polls(&self, state: &mut State, poller: Poller, code: Code) -> Pc {
        let answer = Self::poll(state, code);
        state.took = Some(Branch::Polled { by: poller, answer });
        let then = if poller == Poller::Resume {
            Then::Request
        } else {
            Then::Return
        };
        match answer {
            Answer::Pending => match poller {
                Poller::Resume => Pc::Waiting(code),
                Poller::Command => Pc::Done(Outcome::AwaitingApproval),
                Poller::Wait => unreachable!("a waiter polls only once Cloud would answer"),
            },
            Answer::Token => Pc::Finishing(code),
            Answer::Denied | Answer::Lapsed | Answer::Spent | Answer::Swept => {
                Pc::Forgetting { code, answer, then }
            }
        }
    }

    fn step(&self, state: &mut State, who: usize) {
        let next = match state.pc(who) {
            Pc::Idle if who == LOGOUT => {
                let read = state.read();
                if let File::SignedIn(code) = read {
                    state.set_grant(code, Grant::Spent { ended: true });
                    state.took = Some(Branch::LogoutSignsOut);
                }
                state.kept = [false; MAX_CODES];
                Pc::Clearing(read)
            }
            Pc::Idle if who == COMMAND => match state.read() {
                File::Absent => {
                    state.took = Some(Branch::CommandSignedOut);
                    Pc::Done(Outcome::SignedOut)
                }
                File::Unreadable => {
                    state.took = Some(Branch::CommandUnreadable);
                    Pc::Done(Outcome::Unreadable)
                }
                File::SignedIn(code) => {
                    state.took = Some(Branch::CommandReturnsStored);
                    Self::saved(state, File::SignedIn(code))
                }
                File::Pending(code) => Pc::Polling(code),
            },
            Pc::Idle => match state.read() {
                File::SignedIn(code) if state.honoured(code) => Pc::Refreshing(code),
                File::Pending(code) if state.grant(code) != Grant::Lapsed => Pc::Resuming(code),
                read @ (File::Absent | File::Unreadable | File::Pending(_) | File::SignedIn(_)) => {
                    state.took = Some(Branch::StartRequests(read));
                    Pc::Requesting {
                        expect: self.expect(read),
                    }
                }
            },
            Pc::Refreshing(code) => {
                state.took = Some(Branch::Refreshed);
                self.save(state, File::SignedIn(code))
            }
            Pc::Finishing(code) => {
                state.took = Some(Branch::Finished(Self::role(who)));
                self.save(state, File::SignedIn(code))
            }
            Pc::Requesting { expect } => self.request(state, expect),
            Pc::Resuming(code) => self.polls(state, Poller::Resume, code),
            Pc::Waiting(code) => self.polls(state, Poller::Wait, code),
            Pc::Polling(code) => self.polls(state, Poller::Command, code),
            Pc::Forgetting { code, answer, then } => self.forget(state, who, code, answer, then),
            Pc::Removing { code, answer, then } => {
                let found = Found::of(state.file, code);
                state.file = File::Absent;
                state.took = Some(Branch::Forgot {
                    role: Self::role(who),
                    found,
                    removed: true,
                    answer,
                    then,
                });
                self.after_forget(code, answer, then, true)
            }
            Pc::Posting => self.post_and_save(state),
            Pc::Torn(file) => {
                state.file = file;
                Self::saved(state, file)
            }
            Pc::Clearing(read) => {
                state.took = Some(Branch::LogoutCleared {
                    read,
                    found: state.file,
                });
                state.file = File::Absent;
                state.kept = [false; MAX_CODES];
                Pc::Done(Outcome::LoggedOut(read))
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
            | Pc::Requesting { .. }
            | Pc::Polling(_)
            | Pc::Finishing(_)
            | Pc::Forgetting { .. }
            | Pc::Removing { .. }
            | Pc::Posting
            | Pc::Torn(_)
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
            torn_read: false,
            took: None,
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
        state.took = None;
        state.torn_read = false;
        match action {
            Action::Step(who) => self.step(&mut state, who),
            Action::Approve(code) => state.set_grant(code, Grant::Approved),
            Action::Deny(code) => state.set_grant(code, Grant::Denied),
            Action::Lapse(code) => state.set_grant(code, Grant::Lapsed),
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always(APPROVED_SIGN_IN_KEPT, |_, state: &State| {
                state.approved_kept()
            }),
            Property::always(NO_TORN_READ, |_, state: &State| !state.torn_read),
        ];
        properties.extend(
            COVERAGE
                .iter()
                .filter(|row| match row.reach {
                    Reach::Both => true,
                    Reach::AsIs => !self.fix,
                    Reach::Fix => self.fix,
                })
                .map(|row| Property::sometimes(row.name, row.reached)),
        );
        properties
    }
}

const APPROVED_SIGN_IN_KEPT: &str = "once a process returned a sign-in Cloud still honours, cloud.json holds a SignedIn until a logout steps";
const NO_TORN_READ: &str = "no process reads cloud.json while another's save is half-written";

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
                if state.pcs.iter().all(|pc| *pc == Pc::Idle) {
                    format!(
                        "start: cloud.json is {}; no process has started",
                        state.file
                    )
                } else {
                    format!("from the state above: cloud.json is {}", state.file)
                }
            },
            |(before, action)| reads_as(model, &before, action, state),
        );
        // A row that skips a guard is reached by a flag or by a direct edit to the model;
        // it names Broken only when a flag is set.
        let what = if model.broken == Broken::default() {
            what.replace(" (under Broken)", "")
        } else {
            what
        };
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

/// What a save wrote at `cite`, and what its process does next.
fn saves(who: usize, before: &State, after: &State, file: File, cite: &str) -> String {
    if matches!(after.pc(who), Pc::Torn(_)) {
        format!(
            "save starts writing {file} in place, and cloud.json reads unreadable until it ends (under Broken) ({cite})"
        )
    } else {
        format!(
            "save writes {file} over {} ({cite}){}",
            before.file,
            saved_then(who, file)
        )
    }
}

/// What a process does once its save of `file` is written.
fn saved_then(who: usize, file: File) -> &'static str {
    match file {
        File::Pending(_) => {
            "; ployz login prints \"Open <url> and confirm the code <code>.\" and waits (core/crates/ployz/src/handlers/login.rs:98-99, :104-114)"
        }
        File::SignedIn(_) if who == COMMAND => {
            "; signed_in returns it, and the command acts with it (:282-283)"
        }
        File::SignedIn(_) => {
            "; ployz login prints \"Signed in to <cloud> as <email> in Organization <slug>.\" (core/crates/ployz/src/handlers/login.rs:86, :100, :116-123)"
        }
        File::Absent | File::Unreadable => "",
    }
}

/// `action` from `before` under `model`, read with `after` for the branch it took.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per statement of the code the model copies"
)]
fn reads_as(model: &SignInStore, before: &State, action: Action, after: &State) -> String {
    let who = match action {
        Action::Approve(code) => {
            return format!(
                "Browser: a signed-in member approves {code} at /device (dashboard/src/server/auth.server.ts:269-272)"
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
    let torn = if after.torn_read {
        " while another process's save is half-written (under Broken)"
    } else {
        ""
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
                "logout: replaceable reads {read} as none{torn} (:204-209, :397); clear is next (:398-399)"
            ),
        },
        (Pc::Idle, Pc::Done(Outcome::SignedOut)) => {
            "signed_in: load finds no cloud.json and returns SignedOut, \"Not signed in to Cloud.\" (:277-278, :65)".to_owned()
        }
        (Pc::Idle, Pc::Done(Outcome::Unreadable)) => format!(
            "signed_in: load returns Corrupt{torn}, \"The sign-in stored at ... is unreadable.\" (:188-196, :277, :50)"
        ),
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
        (Pc::Idle, Pc::Requesting { .. }) => format!(
            "start: replaceable reads {}; request_code is next (:340)",
            match before.file {
                File::Absent => "no cloud.json (:198, :338)".to_owned(),
                File::Unreadable =>
                    format!("an unreadable cloud.json as none{torn} (:204-209, :338)"),
                File::Pending(code) =>
                    format!("Pending({code}), which has expired, so start skips it (:325, :338)"),
                File::SignedIn(code) => format!(
                    "SignedIn({code}), and get-session answers 401 for its ended session, so start falls through (:315, :599-600)"
                ),
            }
        ),
        (Pc::Refreshing(code), _) => format!(
            "start: {}",
            saves(who, before, after, File::SignedIn(code), ":321-322")
        ),
        (Pc::Finishing(code), _) => format!(
            "finish: get-session honours {code}'s token, and {}",
            saves(who, before, after, File::SignedIn(code), ":574-583")
        ),
        (Pc::Requesting { expect }, next) => match (after.took, next, expect) {
            (Some(Branch::RequestRereads), _, Some(expected)) => format!(
                "request_code: POST /api/auth/device/code mints a code; under the lock its save finds {} where {expected} was expected, so the fix throws the code away (no process knows it, so the model leaves it unminted) and start reads cloud.json again",
                before.file
            ),
            (_, Pc::Posting, _) => format!(
                "request_code: under the lock, its compare finds cloud.json still {}, as expected, and it releases the lock for the POST (under Broken)",
                before.file
            ),
            (_, _, guard) => format!(
                "request_code: POST /api/auth/device/code mints a code, and {}",
                saves(
                    who,
                    before,
                    after,
                    File::Pending(after.minted().last().expect("request_code minted a code")),
                    if guard.is_some() {
                        ":494-513; under the lock the fix's save found the file it expected"
                    } else {
                        ":494-513"
                    }
                ),
            ),
        },
        (Pc::Resuming(code), next) => format!(
            "start: poll_once's poll: {}{}",
            answered(polled_answer(after), code),
            match next {
                Pc::Waiting(_) =>
                    "; start returns the Pending as resumed (:328-331), and ployz login prints the code and waits (core/crates/ployz/src/handlers/login.rs:98-99)",
                Pc::Forgetting { .. } =>
                    "; forget is next (:529-530), then request_code (:332-333)",
                Pc::Idle
                | Pc::Refreshing(_)
                | Pc::Resuming(_)
                | Pc::Requesting { .. }
                | Pc::Polling(_)
                | Pc::Finishing(_)
                | Pc::Removing { .. }
                | Pc::Posting
                | Pc::Torn(_)
                | Pc::Clearing(_)
                | Pc::Done(_) => "",
            }
        ),
        (Pc::Waiting(code), _) => format!(
            "wait: after its sleep, poll: {}{}",
            answered(polled_answer(after), code),
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
            answered(polled_answer(after), code),
            if matches!(after.pc(who), Pc::Forgetting { .. }) {
                "; forget is next (:529-530)"
            } else {
                ""
            }
        ),
        (Pc::Forgetting { code, answer, then }, next) => {
            let (removed, read) = match after.took {
                Some(Branch::Forgot { removed, .. }) => (removed, false),
                _ => (false, matches!(next, Pc::Removing { .. })),
            };
            let did = if read {
                format!(
                    "forget reads {} and finds its own Pending without the lock (under Broken); remove_file is next",
                    before.file
                )
            } else if !model.compares() {
                format!(
                    "forget: clear removes cloud.json, which held {}{} (:617-618, :224-229)",
                    before.file,
                    if model.fix { " (under Broken)" } else { "" }
                )
            } else if removed {
                format!(
                    "forget removes its own Pending({code}) under the lock (the fix)"
                )
            } else {
                format!(
                    "forget finds {}, not Pending({code}), and leaves it (the fix)",
                    before.file
                )
            };
            if read {
                did
            } else {
                format!("{did}; {}", forget_then(who, answer, then))
            }
        }
        (Pc::Removing { answer, then, .. }, _) => format!(
            "forget: remove_file removes cloud.json, which now holds {} (under Broken); {}",
            before.file,
            forget_then(who, answer, then)
        ),
        (Pc::Posting, _) => format!(
            "request_code: POST /api/auth/device/code mints a code, and {}",
            saves(
                who,
                before,
                after,
                File::Pending(after.minted().last().expect("request_code minted a code")),
                "no compare since the lock was released, under Broken"
            )
        ),
        (Pc::Torn(file), _) => format!(
            "save finishes writing {file} (under Broken){}",
            saved_then(who, file)
        ),
        (Pc::Clearing(read), _) => format!(
            "logout: clear removes cloud.json, which holds {} (:{}); logout prints \"{}\" (core/crates/ployz/src/handlers/login.rs:151-157)",
            before.file,
            if matches!(read, File::Absent | File::Unreadable) {
                "399"
            } else {
                "444"
            },
            if matches!(read, File::Absent | File::Unreadable) {
                "Not signed in."
            } else {
                "Signed out of <cloud>."
            }
        ),
        (
            Pc::Idle | Pc::Done(_),
            Pc::Idle
            | Pc::Refreshing(_)
            | Pc::Resuming(_)
            | Pc::Requesting { .. }
            | Pc::Polling(_)
            | Pc::Finishing(_)
            | Pc::Forgetting { .. }
            | Pc::Removing { .. }
            | Pc::Posting
            | Pc::Torn(_)
            | Pc::Clearing(_)
            | Pc::Waiting(_)
            | Pc::Done(_),
        ) => unreachable!("{:?} cannot lead to {:?}", before.pc(who), after.pc(who)),
    };
    format!("{}: {what}", name(who))
}

/// The answer a poll step got.
fn polled_answer(after: &State) -> Answer {
    match after.took {
        Some(Branch::Polled { answer, .. }) => answer,
        other => unreachable!("a poll step took {other:?}"),
    }
}

/// What a process does once its `forget` is done.
fn forget_then(who: usize, answer: Answer, then: Then) -> String {
    match (then, answer.outcome()) {
        (Then::Request, _) => {
            "start's resume branch goes on to request_code (:332-333)".to_owned()
        }
        (Then::Return, Outcome::Expired) if who == COMMAND => {
            "signed_in returns Expired, \"the sign-in code expired before it was approved\" (:619, :67)".to_owned()
        }
        (Then::Return, Outcome::Expired) => {
            "ployz login fails, \"the sign-in code expired before it was approved\", next: ployz login (:619, :67, core/crates/ployz/src/failure.rs:1030-1033)".to_owned()
        }
        (Then::Return, outcome) => format!("it returns {outcome:?} (:619)"),
    }
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
        SignInStore {
            logins: 2,
            fix: false,
            broken: Broken::default(),
        }
    }

    /// The fix: a locked `forget` of its own Pending, and a locked `request_code` save over
    /// the file `start` read.
    fn fixed() -> SignInStore {
        SignInStore {
            fix: true,
            ..as_is()
        }
    }

    fn as_is_without(broken: Broken) -> SignInStore {
        SignInStore { broken, ..as_is() }
    }

    fn fixed_without(broken: Broken) -> SignInStore {
        SignInStore { broken, ..fixed() }
    }

    fn check(model: SignInStore) -> impl Checker<SignInStore> {
        model.checker().threads(4).spawn_bfs().join()
    }

    /// The first path in BFS order whose last state breaks `property` under `model`, so no
    /// shorter path breaks it, printed as a story under the test's name. For a
    /// `sometimes`, such as a `COVERAGE` row's name, it is the first path that reaches it.
    /// It panics when no path does, as for an `always` that holds. The checker's own
    /// discovery is not that path: `check_block` stores every violation it visits, each
    /// overwriting the last, so a visitor keeps the first. One thread keeps the path the
    /// same from run to run.
    fn breaks(model: SignInStore, property: &'static str) -> Path {
        first_break(model, property, |_| true, true).expect("a path breaks the property")
    }

    /// As `breaks`, the first such path that also satisfies `and`. It explores the whole
    /// model, since the first discovery may not pass `and`. The checker keeps one path to
    /// each state, so `and` sees only the first path to the state it ends in.
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
        // The default, `HasDiscoveries::All`, stops once every property has a discovery.
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

    /// `who` took a step from a place that satisfies `from` to one that satisfies `to`.
    fn moved(path: &Path, who: usize, from: fn(Pc) -> bool, to: fn(Pc) -> bool) -> bool {
        moves(path, who)
            .into_iter()
            .any(|(before, after)| from(before) && to(after))
    }

    /// The branch the last step took.
    fn last_took(path: &Path) -> Option<Branch> {
        last(path).took
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

    /// The finding's schedule: A requests c0 and waits, B resumes c0 and waits, the browser
    /// approves, A's poll gets the token and A saves, and B's poll gets invalid_grant and B
    /// forgets.
    const THE_FINDING: [Action; 9] = [
        Action::Step(A),
        Action::Step(A),
        Action::Step(B),
        Action::Step(B),
        Action::Approve(Code(0)),
        Action::Step(A),
        Action::Step(A),
        Action::Step(B),
        Action::Step(B),
    ];

    /// The resume branch's schedule: A requests c0 and waits, B's `start` reads Pending(c0),
    /// the browser approves, A's poll gets the token and A saves, B's one poll gets
    /// invalid_grant, B forgets, and B's `request_code` runs.
    const THE_LATE_RESUME: [Action; 9] = [
        Action::Step(A),
        Action::Step(A),
        Action::Step(B),
        Action::Approve(Code(0)),
        Action::Step(A),
        Action::Step(A),
        Action::Step(B),
        Action::Step(B),
        Action::Step(B),
    ];

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
        let path = follow(as_is(), &THE_FINDING);
        assert!(moved(
            &path,
            A,
            |pc| matches!(pc, Pc::Requesting { .. }),
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
            |pc| matches!(
                pc,
                Pc::Forgetting {
                    answer: Answer::Spent,
                    then: Then::Return,
                    ..
                }
            )
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
            matches!(before.pc(B), Pc::Requesting { .. })
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
            |pc| matches!(pc, Pc::Requesting { .. })
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
            matches!(
                before.pc(B),
                Pc::Forgetting {
                    answer: Answer::Spent,
                    then: Then::Request,
                    ..
                }
            ) && before.approved_kept()
                && matches!(before.file, File::SignedIn(_))
        });
        let broken = last(&path);
        assert_eq!(broken.file, File::Absent);
        assert_eq!(broken.pc(A), Pc::Done(Outcome::SignedIn));
        assert!(matches!(broken.pc(B), Pc::Requesting { .. }));
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

    /// Another Cloud command's `signed_in` loads Pending(c0) before a login saves, and its
    /// one poll gets invalid_grant afterwards. Its `forget` removes the SignedIn (:282,
    /// :530).
    #[test]
    fn a_cloud_command_forgets_an_approved_sign_in() {
        let path = breaks_where(as_is(), APPROVED_SIGN_IN_KEPT, |path| {
            last_took(path)
                == Some(Branch::Forgot {
                    role: Role::Command,
                    found: Found::SignedIn,
                    removed: true,
                    answer: Answer::Spent,
                    then: Then::Return,
                })
        });
        let broken = last(&path);
        assert_eq!(broken.pc(COMMAND), Pc::Done(Outcome::Expired));
        assert_eq!(broken.file, File::Absent);
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
        let model = SignInStore {
            logins: 3,
            ..as_is()
        };
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

    /// The code as it is breaks only `APPROVED_SIGN_IN_KEPT`, and reaches every row the fix
    /// does not add.
    #[test]
    fn current_code_breaks_only_the_finding() {
        let checker = check(as_is());
        println!("as is: {} states", checker.unique_state_count());
        assert!(checker.discovery(APPROVED_SIGN_IN_KEPT).is_some());
        assert!(
            checker.discovery(NO_TORN_READ).is_none(),
            "{NO_TORN_READ} breaks"
        );
        for row in COVERAGE.iter().filter(|row| row.reach != Reach::Fix) {
            assert!(
                checker.discovery(row.name).is_some(),
                "{} is never reached",
                row.name
            );
        }
    }

    /// If `save` wrote the file in place, a reader would see it half-written: `start` reads
    /// it as none and requests a code (:204-209, :338-340).
    #[test]
    fn save_writes_a_temp_file_and_renames_it() {
        let path = breaks(
            as_is_without(Broken {
                save_tears: true,
                ..Broken::default()
            }),
            NO_TORN_READ,
        );
        let broken = last(&path);
        assert!(broken.torn_read);
        assert_eq!(broken.file, File::Unreadable);
        assert!(broken.pcs.iter().any(|pc| matches!(pc, Pc::Torn(_))));
    }

    /// The code's `forget` only calls `clear`, a bare `remove_file` (:617-618, :224-229),
    /// so it reads nothing, even while another process's save is half-written. A requests
    /// c0, B's `start` finds it, and the browser denies it. A's poll gets access_denied, B's
    /// resume branch forgets c0 and starts saving Pending(c1), and A's `forget` removes the
    /// file while that save is half-written.
    #[test]
    fn forget_reads_nothing() {
        let path = follow(
            as_is_without(Broken {
                save_tears: true,
                ..Broken::default()
            }),
            &[
                Action::Step(A),
                Action::Step(A),
                Action::Step(A),
                Action::Step(B),
                Action::Deny(Code(0)),
                Action::Step(A),
                Action::Step(B),
                Action::Step(B),
                Action::Step(B),
                Action::Step(A),
            ],
        );
        let after = last(&path);
        assert!(forgot(&after, Role::Login, Found::Unreadable, true));
        assert!(!after.torn_read);
        assert_eq!(after.pc(A), Pc::Done(Outcome::Denied));
    }

    /// The fix holds, and every `sometimes` it reaches is reached.
    #[test]
    fn the_fix_keeps_an_approved_sign_in() {
        let checker = check(fixed());
        println!("fixed: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// The finding's schedule under the fix: B's `forget` finds A's SignedIn, not its own
    /// Pending, and leaves it. B still fails Expired.
    #[test]
    fn the_fix_keeps_the_finding_s_sign_in() {
        let path = follow(fixed(), &THE_FINDING);
        let kept = last(&path);
        assert_eq!(kept.file, File::SignedIn(Code(0)));
        assert_eq!(kept.pc(A), Pc::Done(Outcome::SignedIn));
        assert_eq!(kept.pc(B), Pc::Done(Outcome::Expired));
        assert!(kept.approved_kept());
    }

    /// Without the device-code comparison, the fix's `forget` removes whatever the file
    /// holds, and the finding comes back.
    #[test]
    fn the_fix_needs_the_comparison() {
        let path = breaks(
            fixed_without(Broken {
                fix_forgets_any_file: true,
                ..Broken::default()
            }),
            APPROVED_SIGN_IN_KEPT,
        );
        assert!(matches!(
            last_took(&path),
            Some(Branch::Forgot {
                found: Found::SignedIn,
                removed: true,
                ..
            })
        ));
    }

    /// Without the lock, `forget`'s compare still loses: it reads its own Pending, a
    /// `finish` save lands, and `remove_file` removes the SignedIn.
    #[test]
    fn the_fix_needs_the_forget_lock() {
        let unlocked = fixed_without(Broken {
            fix_forget_unlocked: true,
            ..Broken::default()
        });
        let path = breaks_where(unlocked, APPROVED_SIGN_IN_KEPT, |path| {
            forgot(&last(path), Role::Login, Found::SignedIn, true)
        });
        assert!(path.windows(2).any(|pair| matches!(
            pair,
            [(before, _), (after, _)]
                if before.file != after.file && matches!(after.file, File::SignedIn(_))
                    && after.pcs.iter().any(|pc| matches!(pc, Pc::Removing { .. }))
        )));
    }

    /// The POST runs without the lock, so `request_code` must compare at its save. A
    /// compare made before the POST finds the file `start` read, a `finish` save lands
    /// while the POST runs, and the Pending is written over the SignedIn: the slow code
    /// request route, back.
    #[test]
    fn the_fix_compares_at_the_save() {
        let early = fixed_without(Broken {
            fix_compares_before_post: true,
            ..Broken::default()
        });
        let path = breaks_where(early, APPROVED_SIGN_IN_KEPT, |path| {
            let [.., (before, Some(Action::Step(who))), (after, None)] = path.as_slice() else {
                return false;
            };
            before.pc(*who) == Pc::Posting
                && before.approved_kept()
                && matches!(before.file, File::SignedIn(_))
                && matches!(after.file, File::Pending(_))
        });
        assert!(matches!(last(&path).file, File::Pending(_)));
    }

    /// Without the guard on `request_code`'s save, a `start` that read the file before a
    /// login saved its SignedIn writes a Pending over it. The resume branch is one such
    /// `start`: its `forget` leaves A's SignedIn, and its `request_code` replaces it.
    #[test]
    fn the_fix_needs_the_request_guard() {
        let overwrites = fixed_without(Broken {
            fix_request_overwrites: true,
            ..Broken::default()
        });
        let path = breaks(overwrites, APPROVED_SIGN_IN_KEPT);
        assert_eq!(last_took(&path), Some(Branch::Requested));
        assert!(matches!(last(&path).file, File::Pending(_)));
        let path = follow(overwrites, &THE_LATE_RESUME);
        let left = path
            .iter()
            .find(|(state, _)| {
                state.took
                    == Some(Branch::Forgot {
                        role: Role::Login,
                        found: Found::SignedIn,
                        removed: false,
                        answer: Answer::Spent,
                        then: Then::Request,
                    })
            })
            .expect("B's forget left A's SignedIn")
            .0;
        assert_eq!(left.file, File::SignedIn(Code(0)));
        let replaced = last(&path);
        assert_eq!(replaced.file, File::Pending(Code(1)));
        assert_eq!(replaced.pc(B), Pc::Waiting(Code(1)));
        assert!(!replaced.approved_kept());
    }

    /// The same schedule under the fix: B's `request_code` finds A's SignedIn where its
    /// `start` read Pending(c0), so `start` reads the file again, refreshes the SignedIn
    /// and returns it.
    #[test]
    fn the_fix_rereads_after_the_resume_branch() {
        let mut schedule = THE_LATE_RESUME.to_vec();
        schedule.extend([Action::Step(B), Action::Step(B)]);
        let path = follow(fixed(), &schedule);
        assert!(
            path.iter()
                .any(|(state, _)| state.took == Some(Branch::RequestRereads))
        );
        let kept = last(&path);
        assert_eq!(kept.file, File::SignedIn(Code(0)));
        assert_eq!(kept.pc(B), Pc::Done(Outcome::SignedIn));
        assert!(kept.approved_kept());
    }

    /// A resume branch whose `forget` removed its own Pending expects no file, so its
    /// `request_code` saves at once. A requests c0, B's `start` reads Pending(c0), the browser
    /// denies c0, B's one poll gets access_denied (:529, :560), and B's `forget` removes
    /// Pending(c0).
    #[test]
    fn the_fix_requests_after_forgetting_its_own_pending() {
        let path = follow(
            fixed(),
            &[
                Action::Step(A),
                Action::Step(A),
                Action::Step(B),
                Action::Deny(Code(0)),
                Action::Step(B),
                Action::Step(B),
                Action::Step(B),
            ],
        );
        assert!(
            path.iter()
                .any(|(state, _)| forgot(state, Role::Login, Found::Own, true))
        );
        assert!(
            path.iter()
                .all(|(state, _)| state.took != Some(Branch::RequestRereads))
        );
        let after = last(&path);
        assert_eq!(after.took, Some(Branch::Requested));
        assert_eq!(after.file, File::Pending(Code(1)));
    }

    /// The fix's `forget` removes the file only while it holds the polled code's Pending:
    /// not when there is none, not when it is unreadable, not another code's Pending, and
    /// not a SignedIn.
    #[test]
    fn the_fix_forgets_only_the_polled_pending() {
        let polled = Code(0);
        assert!(forgets(File::Pending(polled), polled));
        for file in [
            File::Absent,
            File::Unreadable,
            File::Pending(Code(1)),
            File::SignedIn(polled),
        ] {
            assert!(!forgets(file, polled), "the fix forgets {file}");
        }
        // No step leaves cloud.json unreadable once a process saved, so the model never
        // reaches a forget beside one. Step one from a state built for it.
        let start = fixed()
            .init_states()
            .into_iter()
            .find(|start| start.file == File::Unreadable)
            .expect("an unreadable start");
        let mut beside = start;
        beside.set_grant(polled, Grant::Swept);
        beside.set_pc(
            B,
            Pc::Forgetting {
                code: polled,
                answer: Answer::Swept,
                then: Then::Return,
            },
        );
        let fixed_step = fixed()
            .next_state(&beside, Action::Step(B))
            .expect("B forgets");
        assert_eq!(fixed_step.file, File::Unreadable);
        assert_eq!(
            fixed_step.took,
            Some(Branch::Forgot {
                role: Role::Login,
                found: Found::Unreadable,
                removed: false,
                answer: Answer::Swept,
                then: Then::Return,
            })
        );
        let as_is_step = as_is()
            .next_state(&beside, Action::Step(B))
            .expect("B forgets");
        assert_eq!(as_is_step.file, File::Absent);
    }

    /// Under the fix, the finding's loser keeps the sign-in but still reports that its code
    /// expired.
    #[test]
    fn the_fix_still_reports_expired() {
        let path = breaks(
            fixed(),
            "under the fix, a waiter whose poll got invalid_grant still fails Expired beside the SignedIn",
        );
        assert!(matches!(last(&path).file, File::SignedIn(_)));
    }
}
