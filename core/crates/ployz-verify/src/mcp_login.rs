//! `ployz mcp login` and the agent token it keeps in `mcp.json`, while two logins run at once.
//! A bare `:n` is a line of the file named just before it in the same parentheses, and
//! otherwise of `core/crates/ployz/src/handlers/mcp/login.rs`.
//!
//! `login` reads `mcp.json` once (`stored`, :92, :48-57). Inside `in_cloud` it asks Cloud for a
//! new agent token (`new_token`, :101-108), refuses a token of another kind (:109, :119-124),
//! and `save`s the new one: a temp file, flushed, renamed over `mcp.json`, then a flush of the
//! directory (:66-88). A failed save, or a refused kind, revokes the token just made (:125-138).
//! `fate` then decides what happens to the token it read (:140-141, :209-240), and `revoke`
//! lists the Organization's tokens and deletes that one while it is listed as an agent token
//! (:242-255). Last, `login` prints its warnings and its note (:160-178).
//!
//! Each login is its own `ployz mcp login` process, and its Cloud calls block its one thread
//! (`in_cloud`, core/crates/ployz/src/handlers/account.rs:133-142, on the current-thread
//! `runtime` of core/crates/ployz/src/handlers/mod.rs:480-484). So one login's steps run in
//! order, and another login can step between any two of them. No lock covers them, and the
//! rename (:79) does not read `mcp.json` again.
//!
//! The finding: two logins read the same token before, and both mint. The first rename's token
//! is replaced by the second's. It stays live in Cloud, and no login revokes it or names it.
//! Its login still prints that `ployz mcp` now acts with it (:171-174).
//!
//! The fix variant models E-142's lock: a CLI-local exclusive lock beside `mcp.json`, taken
//! before the read and held through the warnings and the note, in the shape of
//! `Config::update` (core/crates/ployz/src/context.rs:299-326). A second login then reads the
//! first one's token as its token before, and revokes it as `fate` and `revoke` do today.

use std::fmt::Write as _;

use stateright::{Model, Property};

/// The most `ployz mcp login` processes a model runs. The default model runs two.
const MAX_LOGINS: usize = 2;
/// The token `mcp.json` holds at the start, and one per login: each login mints at most one.
const TOKENS: usize = MAX_LOGINS + 1;
/// How many POSTs Cloud answers with a token of kind `cli`.
const CLI_ANSWERS: u8 = 1;
/// How many saves fail before their rename.
const SAVE_FAILURES: u8 = 1;
/// How many saves rename, then fail to flush their directory.
const FLUSH_FAILURES: u8 = 1;
/// How many of `revoke`'s lists fail.
const LIST_FAILURES: u8 = 1;
/// How many `DELETE`s fail, `revoke`'s (:253) or the one of a token just made (:128).
const DELETE_FAILURES: u8 = 1;

/// A token: `P`, the one `mcp.json` holds at the start, or the one a login minted.
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct Tok(u8);

impl Tok {
    const P: Self = Self(0);

    /// The token the login in slot `who` mints.
    fn of(who: usize) -> Self {
        Self(u8::try_from(who + 1).expect("a small slot"))
    }

    fn index(self) -> usize {
        usize::from(self.0)
    }

    /// `P`, then each login's token.
    fn all() -> impl Iterator<Item = Self> {
        (0..TOKENS).map(|index| Self(u8::try_from(index).expect("a small index")))
    }

    /// The login that minted it, or `None` for `P`.
    fn maker(self) -> Option<usize> {
        self.index().checked_sub(1)
    }
}

impl std::fmt::Display for Tok {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.maker() {
            None => formatter.write_str("P"),
            Some(who) => write!(formatter, "T{}", letter(who)),
        }
    }
}

impl std::fmt::Debug for Tok {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self}")
    }
}

/// Where a token's row is, from this sign-in's view.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Place {
    /// In the Cloud and Organization this sign-in acts in.
    Here,
    /// In another Organization of this Cloud.
    OtherOrganization,
    /// In another Cloud, under an Organization of the same slug.
    OtherCloud,
}

/// An Organization Token's kind (`TokenKind`).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Kind {
    Agent,
    Cli,
}

/// One Organization Token, and what the logins said of it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Token {
    /// Cloud has, or had, a row for it: `P` when the start file names one, and a login's own
    /// once its POST inserted it.
    exists: bool,
    place: Place,
    kind: Kind,
    /// Its row is still in Cloud: no `DELETE` removed it.
    live: bool,
    /// A `Left` warning named it: "The agent token … this replaced is still live" (:192-197).
    named_left: bool,
    /// The failed-save warning named it: "Could not revoke the token … just made" (:129-135).
    named_unrevoked: bool,
}

impl Token {
    const NONE: Self = Self {
        exists: false,
        place: Place::Here,
        kind: Kind::Agent,
        live: false,
        named_left: false,
        named_unrevoked: false,
    };

    /// A token whose row is in Cloud, named by no warning.
    fn live(place: Place, kind: Kind) -> Self {
        Self {
            exists: true,
            place,
            kind,
            live: true,
            ..Self::NONE
        }
    }
}

/// What `mcp.json` holds (`Stored`, :22-28).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum File {
    /// No file: `stored` returns `Ok(None)` (:54).
    Absent,
    /// A file that does not parse: `stored` returns `Corrupt` (:53). Its token, if any, is
    /// one nobody can know.
    Unreadable,
    /// A `Stored` naming this token.
    Holds(Tok),
}

impl std::fmt::Display for File {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absent => formatter.write_str("no file"),
            Self::Unreadable => formatter.write_str("an unreadable file"),
            Self::Holds(tok) => write!(formatter, "{tok}"),
        }
    }
}

/// The file a model starts from, with `P` as it names it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Start {
    Absent,
    /// `P`, an agent token in this Cloud and Organization.
    Here,
    /// `P`, an agent token in another Organization of this Cloud.
    OtherOrganization,
    /// `P`, an agent token in another Cloud, under the same Organization slug.
    OtherCloud,
    /// An edited file naming `P`, an Organization Token of kind `cli` in this Organization.
    Cli,
    Unreadable,
}

/// Every start file. Stateright explores the last start first, so a story that more than one
/// start can begin shows the one with no file.
const STARTS: [Start; 6] = [
    Start::Unreadable,
    Start::Cli,
    Start::OtherCloud,
    Start::OtherOrganization,
    Start::Here,
    Start::Absent,
];

/// Why a login leaves the token before live, and names it in a `Left` warning.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Why {
    /// `fate`'s Cloud check (:213-220).
    Cloud,
    /// `fate`'s Organization check (:221-228).
    Organization,
    /// `fate`'s Unflushed arm (:229-235).
    Unflushed,
    /// `revoke`'s list failed (:245, :148-154).
    ListFailed,
    /// `revoke`'s `DELETE` failed (:253, :148-154).
    DeleteFailed,
    /// `revoke`'s `DELETE` got a 404, which the CLI reads as `UnknownCredential`
    /// (core/crates/ployz/src/cloud_account.rs:535-537, :148-154).
    NotFound,
}

/// What `fate` decided for the token before (:140-157).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Fate {
    /// No token before (:143).
    NoPrevious,
    /// `fate` returned `Ok(id)`, so `revoke` runs (:145, :237).
    Revoke,
    /// `fate` returned `Err(Left)` (:144, :239).
    Left(Why),
}

/// Why a login revokes the token it just made (:125-138).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Unmake {
    /// The kind check refused it (:109, :119-124).
    Refused,
    /// `save` failed before its rename (:69-80).
    SaveFailed,
}

/// What a login returned.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Outcome {
    Ok,
    Err,
}

/// Where one login is: its next step.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Pc {
    /// Not started: `stored`'s read is next (:92).
    Idle,
    /// It read `mcp.json`; `in_cloud`'s credential load and `new_token`'s POST are next
    /// (core/crates/ployz/src/handlers/account.rs:139, :101-108).
    Minting,
    /// Cloud answered the POST; the kind check and `save` are next (:109-124, :66-88).
    Saving,
    /// The kind check or `save` failed; `remove_token` of the token just made is next (:128).
    Unmaking(Unmake),
    /// `fate` returned `Ok(id)`; `revoke`'s list is next (:245).
    Listing,
    /// `revoke` found the token before listed as an agent token; its `DELETE` is next (:253).
    Deleting,
    /// `in_cloud` returned; the warnings and the note are next (:160-178). `left` is why
    /// the token before stays live, if it does.
    Reporting {
        left: Option<Why>,
    },
    Done(Outcome),
}

/// What a login's save did before it returned: facts of its steps, not its `Saved` answer.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Flushes {
    /// `sync_all` flushed the temp file before the rename (:75-78).
    bytes: bool,
    /// The directory flush after the rename succeeded (:83).
    dir: bool,
}

/// One login's process.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Login {
    pc: Pc,
    /// The token its read found (`previous`, :92-99).
    previous: Option<Tok>,
    /// Bookkeeping, read by no step that copies code: how many renames had landed when it
    /// read `mcp.json`.
    saw: u8,
    /// What its save did, once it renamed.
    flushes: Option<Flushes>,
    /// Its `revoke` deleted the token before: `revoked` is `Some(id)` (:146).
    revoked: bool,
}

impl Login {
    const IDLE: Self = Self {
        pc: Pc::Idle,
        previous: None,
        saw: 0,
        flushes: None,
        revoked: false,
    };

    /// What `save` returned: `Unflushed` exactly when its directory flush failed (:82-86).
    fn durable(&self) -> bool {
        self.flushes.is_some_and(|flushes| flushes.dir)
    }
}

/// A guard taken out, to show the check catches what it prevents. The first seven are guards
/// the code has; the last two are parts of the fix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag removes one independent guard"
)]
struct Broken {
    /// `save` renames the temp file without its `sync_all` (:75-78).
    persist_unflushed_bytes: bool,
    /// `fate` has no Unflushed arm, so it revokes after a failed directory flush (:229-235).
    revoke_after_unflushed: bool,
    /// `fate` has no Cloud check (:213-220).
    fate_ignores_cloud: bool,
    /// `fate` has no Organization check (:221-228).
    fate_ignores_organization: bool,
    /// `revoke` deletes the token before without its listed-as-agent check (:246-252).
    revoke_unlisted: bool,
    /// `login` saves a token of any kind: no kind check (:109, :119-124).
    keep_non_agent: bool,
    /// A failed save, or a refused kind, returns its error without revoking the token just
    /// made or warning (:125-136).
    failed_save_keeps_new_token: bool,
    /// The fix takes no lock.
    fix_unlocked: bool,
    /// The fix releases its lock right after the rename, not after the note.
    fix_releases_before_report: bool,
}

/// How many of each failure the model has spent.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
struct Failures {
    cli: u8,
    save: u8,
    flush: u8,
    list: u8,
    delete: u8,
}

/// The branch the last step took.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Branch {
    /// `stored` read this (:92).
    Read(File),
    /// Cloud's POST answered a token of this kind (:101-108).
    Minted(Kind),
    /// The kind check or `save` failed, and the login goes on to revoke the token it made.
    Failed(Unmake),
    /// `remove_token` of the token just made: `revoked` false is the failed-save warning
    /// (:128-136).
    Unmade { why: Unmake, revoked: bool },
    /// `save` renamed this login's token over `over`, flushed its directory or not, and
    /// `fate` decided (:79-87, :140-141).
    Saved {
        who: usize,
        over: File,
        durable: bool,
        fate: Fate,
    },
    /// `revoke`'s list answered, and the token before was listed as an agent token or not
    /// (:245-252).
    Listed { who: usize, found: bool },
    /// `revoke`'s list failed (:245).
    ListFailed,
    /// `revoke`'s `DELETE` removed the row (:253).
    Deleted,
    /// `revoke`'s `DELETE` failed, Cloud unreachable (:253).
    DeleteFailed,
    /// `revoke`'s `DELETE` found no row, and Cloud answered 404 (:253).
    DeleteNotFound,
    /// The login printed its warnings and its note, and returned `Ok` (:160-178).
    Noted { who: usize, left: Option<Why> },
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    file: File,
    tokens: [Token; TOKENS],
    logins: [Login; MAX_LOGINS],
    failed: Failures,
    /// Bookkeeping, read by no step that copies code: how many renames have landed.
    renames: u8,
    /// Bookkeeping, read by no step that copies code: some login renamed after another
    /// login's rename that landed after its own read. Sticky.
    raced: bool,
    /// Under the fix, the login that holds the lock beside `mcp.json`.
    lock: Option<usize>,
    took: Option<Branch>,
}

impl State {
    fn token(&self, tok: Tok) -> Token {
        *self
            .tokens
            .get(tok.index())
            .expect("a token the model can mint")
    }

    fn token_mut(&mut self, tok: Tok) -> &mut Token {
        self.tokens
            .get_mut(tok.index())
            .expect("a token the model can mint")
    }

    fn login(&self, who: usize) -> Login {
        *self.logins.get(who).expect("a login slot")
    }

    fn login_mut(&mut self, who: usize) -> &mut Login {
        self.logins.get_mut(who).expect("a login slot")
    }

    fn pc(&self, who: usize) -> Pc {
        self.login(who).pc
    }

    /// The token before of the login in slot `who`.
    fn previous(&self, who: usize) -> Tok {
        self.login(who)
            .previous
            .expect("a login that revokes read a token")
    }
}

/// A token the property counts: an agent token, `P` or one a login made.
fn counts(token: Token) -> bool {
    token.exists && token.kind == Kind::Agent
}

/// `mcp.json` holds it, its row is gone, or a warning that names a token left unrevoked named
/// it by id.
fn accounted(state: &State, tok: Tok) -> bool {
    let token = state.token(tok);
    state.file == File::Holds(tok) || !token.live || token.named_left || token.named_unrevoked
}

/// `revoke`'s list holds the token as an agent token (:245-252). Cloud lists the rows of the
/// caller's Organization in this Cloud (`listCredentials`,
/// dashboard/src/modules/identity/organization-token.server.ts:119-168).
fn listed_as_agent(token: Token) -> bool {
    token.place == Place::Here && token.live && token.kind == Kind::Agent
}

/// Which model a `COVERAGE` row is reached on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    Both,
    /// Only on the code as it is: two logins decide one token's fate, or one renames while
    /// another is between its read and its note. The fix's lock takes these away.
    AsIs,
    /// Only with the fix.
    Fix,
}

/// One `sometimes`: a branch the code handles, read from the state that took it.
struct Coverage {
    name: &'static str,
    reach: Reach,
    reached: fn(&McpLogin, &State) -> bool,
}

fn took(state: &State, branch: Branch) -> bool {
    state.took == Some(branch)
}

/// The last step printed a note after leaving the token before live for `why`.
fn noted_left(state: &State, why: Why) -> bool {
    matches!(state.took, Some(Branch::Noted { left: Some(left), .. }) if left == why)
}

/// Each branch the bounds reach, on the code as it is, on the fix, or on both.
/// `current_code_breaks_only_the_finding` and `the_fix_holds` expect every row their model
/// reaches.
const COVERAGE: &[Coverage] = &[
    Coverage {
        name: "a login reads no mcp.json",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::Read(File::Absent)),
    },
    Coverage {
        name: "a login reads the agent token mcp.json holds",
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::Read(File::Holds(tok))) if state.token(tok).kind == Kind::Agent),
    },
    Coverage {
        name: "a login reads an unreadable mcp.json, warns \"Replacing it.\" and goes on with none",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::Read(File::Unreadable)),
    },
    Coverage {
        name: "Cloud mints an agent token",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::Minted(Kind::Agent)),
    },
    Coverage {
        name: "Cloud answers a cli token, and the login refuses it and revokes it",
        reach: Reach::Both,
        reached: |_, state| {
            took(
                state,
                Branch::Unmade {
                    why: Unmake::Refused,
                    revoked: true,
                },
            )
        },
    },
    Coverage {
        name: "a save fails before the rename, and the login revokes the token it just made",
        reach: Reach::Both,
        reached: |_, state| {
            took(
                state,
                Branch::Unmade {
                    why: Unmake::SaveFailed,
                    revoked: true,
                },
            )
        },
    },
    Coverage {
        name: "the revoke of the token just made fails, and the login warns \"Could not revoke the token … just made\"",
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::Unmade { revoked: false, .. })),
    },
    Coverage {
        name: "a save is Durable",
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::Saved { durable: true, .. })),
    },
    Coverage {
        name: "a save is Unflushed, and the login warns that it may not survive a crash and that the token before is still live",
        reach: Reach::Both,
        reached: |_, state| noted_left(state, Why::Unflushed),
    },
    Coverage {
        name: "the token before is in another Cloud, and the login warns that it is still live",
        reach: Reach::Both,
        reached: |_, state| noted_left(state, Why::Cloud),
    },
    Coverage {
        name: "the token before is in another Organization, and the login warns that it is still live",
        reach: Reach::Both,
        reached: |_, state| noted_left(state, Why::Organization),
    },
    Coverage {
        name: "the token before is listed as an agent token and revoked, and the login prints \"Revoked the agent token … it replaced.\"",
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::Noted { who, .. }) if state.login(who).revoked),
    },
    Coverage {
        name: "the token before is of kind cli, and the login leaves it and says nothing",
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::Listed { who, found: false }) if state.token(state.previous(who)).kind == Kind::Cli),
    },
    Coverage {
        name: "revoke's list fails, and the login warns that the token before is still live",
        reach: Reach::Both,
        reached: |_, state| noted_left(state, Why::ListFailed),
    },
    Coverage {
        name: "revoke's DELETE fails once, and the login warns that the token before is still live",
        reach: Reach::Both,
        reached: |_, state| noted_left(state, Why::DeleteFailed),
    },
    Coverage {
        name: "another login revoked the token before after this one read it, and this one finds it unlisted and says nothing",
        reach: Reach::AsIs,
        reached: |_, state| {
            matches!(state.took, Some(Branch::Listed { who, found: false }) if {
                let token = state.token(state.previous(who));
                token.kind == Kind::Agent && token.place == Place::Here && !token.live
            })
        },
    },
    Coverage {
        name: "another login's DELETE landed between this one's list and its DELETE, which gets a 404, and the login warns that the token before \"is still live\"",
        reach: Reach::AsIs,
        reached: |_, state| noted_left(state, Why::NotFound),
    },
    Coverage {
        name: "a login's rename replaces a token another login saved after this one read mcp.json",
        reach: Reach::AsIs,
        reached: |_, state| {
            matches!(state.took, Some(Branch::Saved { who, over: File::Holds(over), .. })
                if over.maker().is_some_and(|maker| maker != who)
                    && state.login(who).previous != Some(over))
        },
    },
    Coverage {
        name: "a login that read another login's token renames before that login's note, which is then printed stale",
        reach: Reach::AsIs,
        reached: |_, state| {
            matches!(state.took, Some(Branch::Noted { who, .. }) if matches!(state.file,
                File::Holds(held) if held.maker().is_some_and(|other| other != who
                    && state.login(other).previous == Some(Tok::of(who)))))
        },
    },
    Coverage {
        name: "under the fix, a login takes the lock once the other released it, and reads that login's token as its token before",
        reach: Reach::Fix,
        reached: |_, state| matches!(state.took, Some(Branch::Read(File::Holds(tok))) if tok.maker().is_some()),
    },
    Coverage {
        name: "every login returns Ok",
        reach: Reach::Both,
        reached: |model, state| {
            model
                .running()
                .all(|who| state.pc(who) == Pc::Done(Outcome::Ok))
        },
    },
];

/// The `ployz mcp login` processes, as the code is or with the fix.
#[derive(Clone, Copy, Debug)]
struct McpLogin {
    /// How many `ployz mcp login` processes run, at most `MAX_LOGINS`.
    logins: usize,
    /// The files the model starts from.
    starts: &'static [Start],
    /// The fix: E-142's lock beside `mcp.json`, from the read through the note.
    fix: bool,
    broken: Broken,
}

impl McpLogin {
    fn running(&self) -> std::ops::Range<usize> {
        0..self.logins
    }

    /// Every login has returned.
    fn all_returned(&self, state: &State) -> bool {
        self.running()
            .all(|who| matches!(state.pc(who), Pc::Done(_)))
    }

    fn start(start: Start) -> State {
        let state = State {
            file: File::Absent,
            tokens: [Token::NONE; TOKENS],
            logins: [Login::IDLE; MAX_LOGINS],
            failed: Failures::default(),
            renames: 0,
            raced: false,
            lock: None,
            took: None,
        };
        let held = |place, kind| {
            let mut state = state;
            state.file = File::Holds(Tok::P);
            *state.token_mut(Tok::P) = Token::live(place, kind);
            state
        };
        match start {
            Start::Absent => state,
            Start::Here => held(Place::Here, Kind::Agent),
            Start::OtherOrganization => held(Place::OtherOrganization, Kind::Agent),
            Start::OtherCloud => held(Place::OtherCloud, Kind::Agent),
            Start::Cli => held(Place::Here, Kind::Cli),
            Start::Unreadable => State {
                file: File::Unreadable,
                ..state
            },
        }
    }

    /// The fix takes its lock before the read and holds it.
    fn locks(&self) -> bool {
        self.fix && !self.broken.fix_unlocked
    }

    /// `fate` (:209-240) for the token before, with the save `durable` or not. A token in
    /// another Cloud carries the same Organization slug, so only the Cloud check leaves it.
    fn fate(&self, token: Token, durable: bool) -> Fate {
        if token.place == Place::OtherCloud && !self.broken.fate_ignores_cloud {
            Fate::Left(Why::Cloud)
        } else if token.place == Place::OtherOrganization && !self.broken.fate_ignores_organization
        {
            Fate::Left(Why::Organization)
        } else if !durable && !self.broken.revoke_after_unflushed {
            Fate::Left(Why::Unflushed)
        } else {
            Fate::Revoke
        }
    }

    /// Where a failed save or a refused kind goes: `remove_token` of the token just made
    /// (:125-137).
    fn unmaking(&self, why: Unmake) -> Pc {
        if self.broken.failed_save_keeps_new_token {
            Pc::Done(Outcome::Err)
        } else {
            Pc::Unmaking(why)
        }
    }

    /// `stored` reads `mcp.json` (:92-99). Under the fix the lock is taken first; no other
    /// login writes `mcp.json` without it, so taking it and reading are one step.
    fn read(&self, state: &mut State, who: usize) -> Pc {
        if self.locks() {
            state.lock = Some(who);
        }
        let read = state.file;
        let saw = state.renames;
        let login = state.login_mut(who);
        login.previous = match read {
            File::Holds(tok) => Some(tok),
            File::Absent | File::Unreadable => None,
        };
        login.saw = saw;
        state.took = Some(Branch::Read(read));
        Pc::Minting
    }

    /// `new_token`'s POST (:101-108): Cloud inserts the login's token, of kind agent unless it
    /// answers `cli`.
    fn mint(state: &mut State, who: usize, kind: Kind) -> Pc {
        *state.token_mut(Tok::of(who)) = Token::live(Place::Here, kind);
        if kind == Kind::Cli {
            state.failed.cli += 1;
        }
        state.took = Some(Branch::Minted(kind));
        Pc::Saving
    }

    /// The kind check and `save` (:109-124, :66-88), then `fate` (:140-141). Each part
    /// touches only this process's state except the rename (:79), so they are one step.
    fn save(&self, state: &mut State, who: usize, fails: bool, flushes_dir: bool) -> Pc {
        let made = Tok::of(who);
        if state.token(made).kind != Kind::Agent && !self.broken.keep_non_agent {
            state.took = Some(Branch::Failed(Unmake::Refused));
            return self.unmaking(Unmake::Refused);
        }
        if fails {
            state.failed.save += 1;
            state.took = Some(Branch::Failed(Unmake::SaveFailed));
            return self.unmaking(Unmake::SaveFailed);
        }
        if !flushes_dir {
            state.failed.flush += 1;
        }
        let over = state.file;
        state.file = File::Holds(made);
        if state.renames > state.login(who).saw {
            state.raced = true;
        }
        state.renames += 1;
        state.login_mut(who).flushes = Some(Flushes {
            bytes: !self.broken.persist_unflushed_bytes,
            dir: flushes_dir,
        });
        if self.locks() && self.broken.fix_releases_before_report {
            state.lock = None;
        }
        let fate = state
            .login(who)
            .previous
            .map_or(Fate::NoPrevious, |previous| {
                self.fate(state.token(previous), flushes_dir)
            });
        state.took = Some(Branch::Saved {
            who,
            over,
            durable: flushes_dir,
            fate,
        });
        match fate {
            Fate::NoPrevious => Pc::Reporting { left: None },
            Fate::Revoke => Pc::Listing,
            Fate::Left(why) => Pc::Reporting { left: Some(why) },
        }
    }

    /// `remove_token` of the token just made, its warning when that fails, and the error the
    /// login returns (:127-137).
    fn unmake(state: &mut State, who: usize, why: Unmake, fails: bool) -> Pc {
        if fails {
            state.failed.delete += 1;
            state.token_mut(Tok::of(who)).named_unrevoked = true;
        } else {
            state.token_mut(Tok::of(who)).live = false;
        }
        state.took = Some(Branch::Unmade {
            why,
            revoked: !fails,
        });
        Pc::Done(Outcome::Err)
    }

    /// `revoke`'s list and its listed-as-agent check (:245-252).
    fn list(&self, state: &mut State, who: usize, fails: bool) -> Pc {
        if fails {
            state.failed.list += 1;
            state.took = Some(Branch::ListFailed);
            return Pc::Reporting {
                left: Some(Why::ListFailed),
            };
        }
        let found =
            self.broken.revoke_unlisted || listed_as_agent(state.token(state.previous(who)));
        state.took = Some(Branch::Listed { who, found });
        if found {
            Pc::Deleting
        } else {
            Pc::Reporting { left: None }
        }
    }

    /// `revoke`'s `DELETE` (:253), one `DELETE … RETURNING` in Cloud
    /// (dashboard/src/modules/identity/organization-token.server.ts:174-177).
    fn delete(state: &mut State, who: usize, fails: bool) -> Pc {
        if fails {
            state.failed.delete += 1;
            state.took = Some(Branch::DeleteFailed);
            return Pc::Reporting {
                left: Some(Why::DeleteFailed),
            };
        }
        let previous = state.previous(who);
        let token = state.token(previous);
        if token.place == Place::Here && token.live {
            state.token_mut(previous).live = false;
            state.login_mut(who).revoked = true;
            state.took = Some(Branch::Deleted);
            Pc::Reporting { left: None }
        } else {
            state.took = Some(Branch::DeleteNotFound);
            Pc::Reporting {
                left: Some(Why::NotFound),
            }
        }
    }

    /// The warnings and the note (:160-178). Only the `Left` warning names a token (:192-197).
    fn report(state: &mut State, who: usize, left: Option<Why>) -> Pc {
        if left.is_some() {
            let previous = state.previous(who);
            state.token_mut(previous).named_left = true;
        }
        state.took = Some(Branch::Noted { who, left });
        Pc::Done(Outcome::Ok)
    }

    fn step(&self, state: &mut State, who: usize, how: How) {
        let fails = how == How::Fails;
        let next = match state.pc(who) {
            Pc::Idle => self.read(state, who),
            Pc::Minting => Self::mint(state, who, if fails { Kind::Cli } else { Kind::Agent }),
            Pc::Saving => self.save(state, who, fails, how != How::Unflushed),
            Pc::Unmaking(why) => Self::unmake(state, who, why, fails),
            Pc::Listing => self.list(state, who, fails),
            Pc::Deleting => Self::delete(state, who, fails),
            Pc::Reporting { left } => Self::report(state, who, left),
            Pc::Done(_) => unreachable!("a returned login takes no step"),
        };
        // The lock guard drops when its login returns, whichever way.
        if matches!(next, Pc::Done(_)) && state.lock == Some(who) {
            state.lock = None;
        }
        state.login_mut(who).pc = next;
    }

    /// Whether the login in slot `who` can take its next step as `how` says.
    fn offers(&self, state: &State, who: usize, how: How) -> bool {
        let pc = state.pc(who);
        if matches!(pc, Pc::Done(_)) {
            return false;
        }
        if pc == Pc::Idle && self.locks() && state.lock.is_some() {
            return false;
        }
        let saves = state.token(Tok::of(who)).kind == Kind::Agent || self.broken.keep_non_agent;
        let failed = state.failed;
        match how {
            How::Goes => true,
            How::Fails => match pc {
                Pc::Minting => failed.cli < CLI_ANSWERS,
                Pc::Saving => saves && failed.save < SAVE_FAILURES,
                Pc::Unmaking(_) | Pc::Deleting => failed.delete < DELETE_FAILURES,
                Pc::Listing => failed.list < LIST_FAILURES,
                Pc::Idle | Pc::Reporting { .. } | Pc::Done(_) => false,
            },
            How::Unflushed => pc == Pc::Saving && saves && failed.flush < FLUSH_FAILURES,
        }
    }
}

/// How a step goes.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum How {
    /// As the code goes when nothing fails, and Cloud answers kind agent.
    Goes,
    /// The step's one-off failure: Cloud answers the POST with kind `cli`, the save fails
    /// before its rename, or the list or the `DELETE` fails, Cloud unreachable.
    Fails,
    /// The save renames, then its directory flush fails.
    Unflushed,
}

/// One login's next step, and how it goes.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Action {
    who: usize,
    how: How,
}

impl Model for McpLogin {
    type State = State;
    type Action = Action;

    /// One start per file in `starts`, with every login yet to start.
    fn init_states(&self) -> Vec<State> {
        self.starts
            .iter()
            .map(|start| Self::start(*start))
            .collect()
    }

    /// Each login's next step in slot order, each as it goes, then as it fails, then with
    /// an unflushed directory. The order decides only which of the shortest paths a story
    /// shows.
    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        for how in [How::Goes, How::Fails, How::Unflushed] {
            actions.extend(
                self.running()
                    .filter(|who| self.offers(state, *who, how))
                    .map(|who| Action { who, how }),
            );
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = *last;
        state.took = None;
        self.step(&mut state, action.who, action.how);
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always(EVERY_TOKEN_ACCOUNTED, |model: &Self, state: &State| {
                every_token_accounted(model, state)
            }),
            Property::always(NOTE_HOLDS_WHEN_PRINTED, |_, state: &State| {
                note_holds(state)
            }),
            Property::always(REVOKED_ONLY_WHEN_DURABLE, |_, state: &State| {
                state.logins.iter().all(|login| {
                    !login.revoked
                        || login
                            .flushes
                            .is_some_and(|flushes| flushes.bytes && flushes.dir)
                })
            }),
            Property::always(ONLY_AGENT_TOKEN_HELD, |_, state: &State| {
                if let Some(Branch::Saved { who, .. }) = state.took {
                    state.token(Tok::of(who)).kind == Kind::Agent
                } else {
                    true
                }
            }),
            Property::always(NO_CLI_TOKEN_REVOKED, |_, state: &State| {
                state.logins.iter().all(|login| {
                    !login.revoked
                        || login
                            .previous
                            .is_some_and(|previous| state.token(previous).kind == Kind::Agent)
                })
            }),
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

/// Once every login has returned, each token the property counts is accounted for.
fn every_token_accounted(model: &McpLogin, state: &State) -> bool {
    !model.all_returned(state)
        || Tok::all().all(|tok| !counts(state.token(tok)) || accounted(state, tok))
}

/// A login that just printed its note finds `mcp.json` holding its own token.
fn note_holds(state: &State) -> bool {
    if let Some(Branch::Noted { who, .. }) = state.took {
        state.file == File::Holds(Tok::of(who))
    } else {
        true
    }
}

const EVERY_TOKEN_ACCOUNTED: &str = "once every login has returned, every agent token a login made, and the agent token mcp.json held at the start, is the one mcp.json holds, revoked, or named by id in a warning that names a token left unrevoked";
const NOTE_HOLDS_WHEN_PRINTED: &str =
    "when a login prints that `ployz mcp` now acts with its token, mcp.json holds that token";
const REVOKED_ONLY_WHEN_DURABLE: &str = "a login revokes the token it replaced only after its own save flushed the temp file before the rename, and the directory after it";
const ONLY_AGENT_TOKEN_HELD: &str =
    "no login's rename puts a token whose kind is not agent into mcp.json";
const NO_CLI_TOKEN_REVOKED: &str =
    "no login revokes an Organization Token of kind cli as the token it replaced";

/// The letter of the login in slot `who`.
fn letter(who: usize) -> char {
    match who {
        0 => 'A',
        1 => 'B',
        2 => 'C',
        _ => unreachable!("slot {who} holds no login"),
    }
}

fn name(who: usize) -> String {
    format!("Login {}", letter(who))
}

/// A token as Cloud holds it.
fn row(tok: Tok, token: Token) -> String {
    let kind = match token.kind {
        Kind::Agent => "agent",
        Kind::Cli => "cli",
    };
    let place = match token.place {
        Place::Here => "",
        Place::OtherOrganization => " in another Organization",
        Place::OtherCloud => " in another Cloud",
    };
    let live = if token.live { "live" } else { "revoked" };
    format!("{tok} {kind}{place} {live}")
}

/// `path` in the code's words: each step, then `mcp.json`, Cloud's tokens, each login, and
/// the tokens a warning named.
fn story(model: &McpLogin, path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    // Stateright pairs each state with the action taken from it; the row lists the action
    // that led to the state instead, so the last row shows the broken state.
    let mut led_by: Option<(State, Action)> = None;
    for (n, (state, action)) in path.iter().enumerate() {
        let what = led_by.map_or_else(
            || {
                if state.logins.iter().all(|login| login.pc == Pc::Idle) {
                    format!("start: {}; no login has started", start_file(state))
                } else {
                    format!("from the state above: mcp.json is {}", state.file)
                }
            },
            |(before, action)| reads_as(&before, action, state),
        );
        // A row that skips a guard is reached by a flag or by a direct edit to the model;
        // it names Broken only when a flag is set.
        let what = if model.broken == Broken::default() {
            what.replace(" (under Broken)", "")
        } else {
            what
        };
        led_by = action.map(|action| (*state, action));
        let tokens: Vec<String> = Tok::all()
            .filter(|tok| state.token(*tok).exists)
            .map(|tok| row(tok, state.token(tok)))
            .collect();
        let logins: Vec<String> = model
            .running()
            .map(|who| {
                let login = state.login(who);
                let read = login
                    .previous
                    .map_or_else(String::new, |previous| format!(", read {previous}"));
                format!("{}: {:?}{read}", name(who), login.pc)
            })
            .collect();
        let named: Vec<String> = Tok::all()
            .filter_map(|tok| {
                let token = state.token(tok);
                match (token.named_left, token.named_unrevoked) {
                    (true, _) => Some(format!("{tok} by a Left warning")),
                    (false, true) => Some(format!("{tok} by the failed-save warning")),
                    (false, false) => None,
                }
            })
            .collect();
        let lock = if model.fix {
            format!(
                " | lock: {}",
                state.lock.map_or_else(|| "free".to_owned(), name)
            )
        } else {
            String::new()
        };
        let _ = writeln!(
            story,
            "{n:2}. {what}\n    mcp.json: {}{lock} | Cloud: {} | {} | named: {}",
            state.file,
            if tokens.is_empty() {
                "no token".to_owned()
            } else {
                tokens.join(", ")
            },
            logins.join(" | "),
            if named.is_empty() {
                "none".to_owned()
            } else {
                named.join(", ")
            },
        );
    }
    story
}

/// The start file, with `P` as it names it.
fn start_file(state: &State) -> String {
    match state.file {
        File::Holds(tok) => {
            let token = state.token(tok);
            match (token.kind, token.place) {
                (Kind::Cli, _) => {
                    format!("mcp.json, edited, holds {tok}, a cli token in this Organization")
                }
                (Kind::Agent, Place::Here) => {
                    format!("mcp.json holds {tok}, an agent token in this Cloud and Organization")
                }
                (Kind::Agent, Place::OtherOrganization) => format!(
                    "mcp.json holds {tok}, an agent token in another Organization of this Cloud"
                ),
                (Kind::Agent, Place::OtherCloud) => format!(
                    "mcp.json holds {tok}, an agent token in another Cloud, under the same Organization slug"
                ),
            }
        }
        File::Absent => "there is no mcp.json".to_owned(),
        File::Unreadable => "mcp.json is unreadable".to_owned(),
    }
}

/// What `fate` decided for the token before, read from `before`, in the code's words.
fn fated(fate: Fate, before: &State, who: usize, durable: bool) -> String {
    let previous = before.login(who).previous;
    let id = previous.map_or_else(String::new, |tok| tok.to_string());
    match fate {
        Fate::NoPrevious => "there is no token before, so nothing is revoked (:143)".to_owned(),
        Fate::Revoke => {
            let place = before.token(before.previous(who)).place;
            let skipped = match place {
                Place::OtherCloud => {
                    format!(
                        "skips its Cloud check, though {id} is in another Cloud (under Broken), and "
                    )
                }
                Place::OtherOrganization => format!(
                    "skips its Organization check, though {id} is in another Organization (under Broken), and "
                ),
                Place::Here if !durable => {
                    "skips its Unflushed arm, though the save is Unflushed (under Broken), and "
                        .to_owned()
                }
                Place::Here => {
                    format!("finds {id} in this Cloud and Organization and the save Durable, and ")
                }
            };
            format!("fate {skipped}returns Ok({id}), so revoke is next (:140-141, :145, :237)")
        }
        Fate::Left(Why::Cloud) => {
            format!("fate leaves {id} live: it is in another Cloud (:213-220)")
        }
        Fate::Left(Why::Organization) => {
            format!("fate leaves {id} live: it is in another Organization (:221-228)")
        }
        Fate::Left(Why::Unflushed) => {
            format!("fate leaves {id} live: a crash may bring it back into mcp.json (:229-235)")
        }
        Fate::Left(why @ (Why::ListFailed | Why::DeleteFailed | Why::NotFound)) => {
            unreachable!("fate never leaves a token for {why:?}")
        }
    }
}

/// The `Left` warning's why, in the code's words.
fn left_why(why: Why) -> &'static str {
    match why {
        Why::Cloud => "it is in Ployz Cloud at <its Cloud> (:215)",
        Why::Organization => "it is in Organization <its Organization> (:223)",
        Why::Unflushed => {
            "a crash may bring it back into mcp.json, which could not be flushed (<error>) (:231-233)"
        }
        Why::ListFailed | Why::DeleteFailed => {
            "revoking it failed (Could not reach Cloud at <cloud>.) (:150; core/crates/ployz/src/cloud_login.rs:30)"
        }
        Why::NotFound => {
            "revoking it failed (no token or signed-in device <id> in this Organization) (:150; core/crates/ployz/src/cloud_login.rs:93-94)"
        }
    }
}

/// `action` from `before`, read with `after` for the branch it took.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per statement of the code the model copies"
)]
fn reads_as(before: &State, action: Action, after: &State) -> String {
    let who = action.who;
    let made = Tok::of(who);
    let locked = if after.lock == Some(who) && before.lock != Some(who) {
        "takes the lock beside mcp.json (the fix), and "
    } else {
        ""
    };
    let releases = if before.lock == Some(who) && after.lock.is_none() {
        "; the lock is released (the fix)"
    } else {
        ""
    };
    let what = match after.took.expect("every step takes a branch") {
        Branch::Read(File::Absent) => format!(
            "{locked}stored finds no mcp.json, so there is no token before (:92, :54); in_cloud's credential load and new_token's POST are next (core/crates/ployz/src/handlers/account.rs:139, :101-108)"
        ),
        Branch::Read(File::Unreadable) => format!(
            "{locked}stored finds mcp.json unreadable, so login warns \"The sign-in stored at <path> is unreadable. Replacing it.\" and goes on with no token before (:92, :53, :94-97; core/crates/ployz/src/cloud_login.rs:50); in_cloud's credential load and new_token's POST are next (core/crates/ployz/src/handlers/account.rs:139, :101-108)"
        ),
        Branch::Read(File::Holds(tok)) => format!(
            "{locked}stored reads mcp.json, which holds {tok}, its token before (:92, :50-52); in_cloud's credential load and new_token's POST are next (core/crates/ployz/src/handlers/account.rs:139, :101-108)"
        ),
        Branch::Minted(Kind::Agent) => format!(
            "new_token: POST /api/cli/tokens, and Cloud inserts {made}, an agent token named \"ployz mcp\" with the Full preset for 365 days (:101-108; dashboard/src/modules/identity/organization-token.server.ts:34-56, :70); the kind check and save are next (:109-110)"
        ),
        Branch::Minted(Kind::Cli) => format!(
            "new_token: POST /api/cli/tokens, and Cloud inserts {made} with kind cli, as a Cloud that ignores the kind the CLI asks for would (:101-108); the kind check is next (:109)"
        ),
        Branch::Failed(why) => {
            let failed = match why {
                Unmake::Refused => format!(
                    "the kind check finds {made} is a cli token where `ployz mcp` needs an agent token (:109, :119-124)"
                ),
                Unmake::SaveFailed => {
                    "save fails before its rename, say writing the temp file (:69-78)".to_owned()
                }
            };
            if matches!(after.pc(who), Pc::Done(_)) {
                format!(
                    "{failed}, and login returns the error without revoking {made} or warning (under Broken) (:137){releases}"
                )
            } else {
                format!("{failed}, so remove_token of {made} is next (:125-128)")
            }
        }
        Branch::Unmade { why, revoked } => {
            let error = match why {
                Unmake::Refused => {
                    "\"Cloud sent an unexpected reply: it made a cli token where `ployz mcp` needs an agent token\" (:120-123; core/crates/ployz/src/cloud_login.rs:46)"
                }
                Unmake::SaveFailed => {
                    "\"Could not use the sign-in stored at <path>.\" (core/crates/ployz/src/cloud_login.rs:48)"
                }
            };
            if revoked {
                format!(
                    "remove_token: DELETE /api/cli/tokens/{made} revokes the token it just made (:128), and login returns the error {error} (:137){releases}"
                )
            } else {
                format!(
                    "remove_token: DELETE /api/cli/tokens/{made} fails, Cloud unreachable, so login warns \"Could not revoke the token {made} just made: … Run `ployz token rm {made}`.\" (:128-135) and returns the error {error} (:137){releases}"
                )
            }
        }
        Branch::Saved {
            over,
            durable,
            fate,
            ..
        } => {
            let flush = if durable {
                "the directory flush succeeds, so save returns Durable (:82-84)"
            } else {
                "the directory flush fails, so save returns Unflushed (:82-85)"
            };
            let kind = if before.token(made).kind == Kind::Cli {
                ", a cli token, past no kind check (under Broken),"
            } else {
                ""
            };
            let synced = if after
                .login(who)
                .flushes
                .is_some_and(|flushes| flushes.bytes)
            {
                "flushes it"
            } else {
                "skips its sync_all (under Broken)"
            };
            format!(
                "save writes {made}{kind} to a temp file, {synced}, and renames it to mcp.json, where {over} was, without reading it (:71-79); {flush}; {}{}",
                fated(fate, before, who, durable),
                if releases.is_empty() {
                    ""
                } else {
                    "; the fix releases its lock right after the rename, before the note (under Broken)"
                }
            )
        }
        Branch::ListFailed => format!(
            "revoke: GET /api/cli/tokens fails, Cloud unreachable, so login leaves {} live (:245, :148-154)",
            before.previous(who)
        ),
        Branch::Listed { found: true, .. } => {
            let previous = before.previous(who);
            if listed_as_agent(before.token(previous)) {
                format!(
                    "revoke: GET /api/cli/tokens lists {previous} as an agent token (:245-252); its DELETE is next (:253)"
                )
            } else {
                format!(
                    "revoke: GET /api/cli/tokens answers, and with no listed-as-agent check its DELETE of {previous} is next (under Broken) (:245-253)"
                )
            }
        }
        Branch::Listed { found: false, .. } => {
            let previous = before.previous(who);
            let token = before.token(previous);
            let why = if token.kind == Kind::Cli {
                format!("lists {previous} as a cli token, not an agent token")
            } else if !token.live {
                format!("does not list {previous}, whose row another login's DELETE removed")
            } else {
                format!("does not list {previous}")
            };
            format!(
                "revoke: GET /api/cli/tokens {why}, so revoke returns Ok(false) and login says nothing of {previous} (:245-251, :147)"
            )
        }
        Branch::Deleted => format!(
            "revoke: DELETE /api/cli/tokens/{} removes its row (:253; dashboard/src/modules/identity/organization-token.server.ts:174-177), and revoke returns Ok(true) (:254, :146)",
            before.previous(who)
        ),
        Branch::DeleteFailed => format!(
            "revoke: DELETE /api/cli/tokens/{} fails, Cloud unreachable, so login leaves it live (:253, :148-154)",
            before.previous(who)
        ),
        Branch::DeleteNotFound => format!(
            "revoke: DELETE /api/cli/tokens/{} finds no row, since another login's DELETE removed it; with no Server Clear pending Cloud answers 404 (dashboard/src/modules/identity/organization-token.server.ts:174-177, :192; dashboard/src/routes/api/cli/-cli.handler.ts:229-230, :572-577), the CLI reads it as UnknownCredential (core/crates/ployz/src/cloud_account.rs:535-537), and login leaves it to name as still live (:148-154)",
            before.previous(who)
        ),
        Branch::Noted { left, .. } => {
            let login = before.login(who);
            let mut said = Vec::new();
            if !login.durable() {
                said.push("warns \"Saved the new agent token to <path>, but could not flush its directory (<error>), so a crash may lose it. …\" (:185-191)".to_owned());
            }
            if let Some(why) = left {
                said.push(format!(
                    "warns \"The agent token {} this replaced is still live: {}. To revoke it, ….\" (:192-197)",
                    before.previous(who),
                    left_why(why)
                ));
            }
            let stale = if after.file == File::Holds(made) {
                String::new()
            } else {
                format!(", while mcp.json holds {}", after.file)
            };
            said.push(format!(
                "prints \"`ployz mcp` now acts with its own agent token {made} in Organization …\" (:171-174){stale}"
            ));
            if login.revoked {
                said.push(format!(
                    "prints \"Revoked the agent token {} it replaced.\" (:175-177)",
                    before.previous(who)
                ));
            }
            format!("{}, and returns Ok{releases}", said.join(", "))
        }
    };
    let how = match action.how {
        How::Goes => "",
        How::Fails => " [fails]",
        How::Unflushed => " [unflushed]",
    };
    format!("{}{how}: {what}", name(who))
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

    /// The code as it is, with two logins, from every start file.
    fn as_is() -> McpLogin {
        McpLogin {
            logins: MAX_LOGINS,
            starts: &STARTS,
            fix: false,
            broken: Broken::default(),
        }
    }

    /// The fix: E-142's lock, from the read through the note.
    fn fixed() -> McpLogin {
        McpLogin {
            fix: true,
            ..as_is()
        }
    }

    fn as_is_without(broken: Broken) -> McpLogin {
        McpLogin { broken, ..as_is() }
    }

    fn fixed_without(broken: Broken) -> McpLogin {
        McpLogin { broken, ..fixed() }
    }

    fn check(model: McpLogin) -> impl Checker<McpLogin> {
        model.checker().threads(4).spawn_bfs().join()
    }

    /// The first path in BFS order whose last state breaks `property` under `model`, so no
    /// shorter path breaks it, printed as a story under the test's name. For a
    /// `sometimes`, such as a `COVERAGE` row's name, it is the first path that reaches it.
    /// It panics when no path does, as for an `always` that holds. The checker's own
    /// discovery is not that path: `check_block` stores every violation it visits, each
    /// overwriting the last, so a visitor keeps the first. One thread keeps the path the
    /// same from run to run.
    fn breaks(model: McpLogin, property: &'static str) -> Path {
        first_break(model, property, |_| true, true).expect("a path breaks the property")
    }

    /// As `first_break` over the whole model, and it panics when no path breaks.
    fn breaks_where(
        model: McpLogin,
        property: &'static str,
        and: impl Fn(&Path) -> bool + Send + Sync + 'static,
    ) -> Path {
        first_break(model, property, and, false).expect("a path breaks the property")
    }

    /// The first path in BFS order that breaks `property` and satisfies `and`, or `None`.
    /// The checker keeps one path to each state, the first in BFS order, so `and` sees only
    /// the first path to the state it ends in: a filter must read a fact the state holds,
    /// such as `raced`, not one only the path shows. Unless `stop_at_discovery`, it explores
    /// the whole model, since the first discovery may not pass `and`.
    fn first_break(
        model: McpLogin,
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

    /// The branch each step of `who` took, in order.
    fn steps_of(path: &Path, who: usize) -> Vec<Branch> {
        path.windows(2)
            .filter_map(|pair| match pair {
                [(_, Some(action)), (after, _)] if action.who == who => after.took,
                _ => None,
            })
            .collect()
    }

    /// The index in `path` of the state after `who`'s first step that took a branch `is`
    /// accepts.
    fn when(path: &Path, who: usize, is: fn(Branch) -> bool) -> Option<usize> {
        path.windows(2)
            .position(|pair| match pair {
                [(_, Some(action)), (after, _)] => action.who == who && after.took.is_some_and(is),
                _ => false,
            })
            .map(|index| index + 1)
    }

    fn is_read(branch: Branch) -> bool {
        matches!(branch, Branch::Read(_))
    }

    fn is_rename(branch: Branch) -> bool {
        matches!(branch, Branch::Saved { .. })
    }

    fn is_note(branch: Branch) -> bool {
        matches!(branch, Branch::Noted { .. })
    }

    /// Steps `actions` from `start` under `model`, each one offered where it is taken, and
    /// prints the path as a story under the test's name.
    fn follow(model: McpLogin, start: Start, actions: &[Action]) -> Path {
        let mut state = McpLogin::start(start);
        assert!(model.init_states().contains(&state), "{start:?} is a start");
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

    const fn goes(who: usize) -> Action {
        Action {
            who,
            how: How::Goes,
        }
    }

    /// The finding's schedule: A and B read P, A and B mint, A renames TA over P, B renames
    /// TB over TA, A lists P and deletes it, B lists after that DELETE, and each reports.
    const THE_RACE: [Action; 11] = [
        goes(A),
        goes(B),
        goes(A),
        goes(B),
        goes(A),
        goes(B),
        goes(A),
        goes(A),
        goes(B),
        goes(A),
        goes(B),
    ];

    /// The race. Both logins read P (:92) and mint (:101-108). A's save renames TA over P,
    /// and B's renames TB over TA, each without reading mcp.json again (:79). A lists P as
    /// an agent token and deletes it (:245-253). B lists after that DELETE, finds no P, and
    /// says nothing (:147). Both return Ok and print their note (:171-174). mcp.json holds
    /// TB. TA is live in Cloud, and no login revoked it or named it in a warning; A's note
    /// said `ployz mcp` now acts with TA while mcp.json held TB.
    #[test]
    fn two_logins_that_read_the_same_token_orphan_the_first_rename() {
        let path = follow(as_is(), Start::Here, &THE_RACE);
        assert_eq!(
            steps_of(&path, A),
            [
                Branch::Read(File::Holds(Tok::P)),
                Branch::Minted(Kind::Agent),
                Branch::Saved {
                    who: A,
                    over: File::Holds(Tok::P),
                    durable: true,
                    fate: Fate::Revoke,
                },
                Branch::Listed {
                    who: A,
                    found: true
                },
                Branch::Deleted,
                Branch::Noted { who: A, left: None },
            ]
        );
        assert_eq!(
            steps_of(&path, B),
            [
                Branch::Read(File::Holds(Tok::P)),
                Branch::Minted(Kind::Agent),
                Branch::Saved {
                    who: B,
                    over: File::Holds(Tok::of(A)),
                    durable: true,
                    fate: Fate::Revoke,
                },
                Branch::Listed {
                    who: B,
                    found: false
                },
                Branch::Noted { who: B, left: None },
            ]
        );
        let a_note = when(&path, A, is_note).expect("A notes");
        let a_noted = path.get(a_note).expect("the state after A's note").0;
        assert_eq!(a_noted.file, File::Holds(Tok::of(B)));
        assert!(!note_holds(&a_noted), "A's note is stale when printed");
        let end = last(&path);
        assert_eq!(end.file, File::Holds(Tok::of(B)));
        assert_eq!(end.pc(A), Pc::Done(Outcome::Ok));
        assert_eq!(end.pc(B), Pc::Done(Outcome::Ok));
        assert!(!end.token(Tok::P).live);
        let orphan = end.token(Tok::of(A));
        assert!(orphan.live && !orphan.named_left && !orphan.named_unrevoked);
        assert!(!accounted(&end, Tok::of(A)));
        assert!(end.raced);
        assert!(!every_token_accounted(&as_is(), &end));
    }

    /// With no mcp.json, both logins read none, both mint, and A then B renames. No revoke
    /// runs, and TA is live and unnamed.
    #[test]
    fn two_first_logins_orphan_the_first_rename() {
        let path = follow(
            as_is(),
            Start::Absent,
            &[
                goes(A),
                goes(B),
                goes(A),
                goes(B),
                goes(A),
                goes(B),
                goes(A),
                goes(B),
            ],
        );
        for who in [A, B] {
            assert_eq!(
                steps_of(&path, who).first(),
                Some(&Branch::Read(File::Absent))
            );
        }
        let end = last(&path);
        assert_eq!(end.file, File::Holds(Tok::of(B)));
        assert!(end.token(Tok::of(A)).live);
        assert!(!accounted(&end, Tok::of(A)));
        assert!(end.raced);
        assert!(!every_token_accounted(&as_is(), &end));
    }

    /// The first path that breaks `EVERY_TOKEN_ACCOUNTED` is a race: one login reads before
    /// the other's rename, and renames after it.
    #[test]
    fn the_first_orphan_is_a_race() {
        let path = breaks(as_is(), EVERY_TOKEN_ACCOUNTED);
        let raced = [(A, B), (B, A)].into_iter().any(|(first, second)| {
            let (Some(read), Some(own), Some(other)) = (
                when(&path, second, is_read),
                when(&path, second, is_rename),
                when(&path, first, is_rename),
            ) else {
                return false;
            };
            read < other && other < own
        });
        assert!(
            raced,
            "a login reads before the other's rename, and renames after it"
        );
        assert!(last(&path).raced);
    }

    /// The second route to a stale note, with no race: B reads after A's rename, so it
    /// reads TA, and B's rename lands before A's note. B then revokes TA, so every token is
    /// accounted. Only A's note is stale.
    #[test]
    fn a_login_that_reads_the_other_s_token_makes_its_note_stale() {
        let path = follow(
            as_is(),
            Start::Here,
            &[
                goes(A),
                goes(A),
                goes(A),
                goes(B),
                goes(B),
                goes(B),
                goes(A),
                goes(A),
                goes(A),
                goes(B),
                goes(B),
                goes(B),
            ],
        );
        assert_eq!(
            steps_of(&path, B).first(),
            Some(&Branch::Read(File::Holds(Tok::of(A))))
        );
        let a_note = when(&path, A, is_note).expect("A notes");
        assert!(when(&path, B, is_rename).is_some_and(|rename| rename < a_note));
        let noted = path.get(a_note).expect("the state after A's note").0;
        assert!(!note_holds(&noted));
        let end = last(&path);
        assert!(!end.raced);
        assert!(!end.token(Tok::of(A)).live, "B revoked TA");
        assert!(every_token_accounted(&as_is(), &end));
    }

    /// The first stale note has another login's rename between the printing login's rename
    /// and its note. BFS may return either route, so the test asserts only that.
    #[test]
    fn the_first_stale_note_follows_another_login_s_rename() {
        let path = breaks(as_is(), NOTE_HOLDS_WHEN_PRINTED);
        let Some(Branch::Noted { who, .. }) = last(&path).took else {
            panic!("the last step is a note");
        };
        let other = if who == A { B } else { A };
        let own = when(&path, who, is_rename).expect("the printing login renamed");
        let theirs = when(&path, other, is_rename).expect("the other login renamed");
        assert!(
            own < theirs,
            "the other's rename lands after this login's rename"
        );
    }

    /// The code as it is orphans a token only through a race: with every raced state
    /// rejected, no path breaks `EVERY_TOKEN_ACCOUNTED`.
    #[test]
    fn only_a_race_orphans_a_token() {
        assert!(
            first_break(
                as_is(),
                EVERY_TOKEN_ACCOUNTED,
                |path| !last(path).raced,
                false
            )
            .is_none()
        );
    }

    /// With every raced state rejected, the first stale note is the second route: the
    /// other login read the printing login's token.
    #[test]
    fn the_unraced_stale_note_is_the_second_route() {
        let path = first_break(
            as_is(),
            NOTE_HOLDS_WHEN_PRINTED,
            |path| !last(path).raced,
            false,
        )
        .expect("an unraced stale note");
        let end = last(&path);
        let Some(Branch::Noted { who, .. }) = end.took else {
            panic!("the last step is a note");
        };
        let other = if who == A { B } else { A };
        assert_eq!(end.login(other).previous, Some(Tok::of(who)));
    }

    /// The code as it is breaks the two finding properties, keeps the other three, and
    /// reaches every row the fix does not add.
    #[test]
    fn current_code_breaks_only_the_finding() {
        let checker = check(as_is());
        println!("as is: {} states", checker.unique_state_count());
        assert!(checker.discovery(EVERY_TOKEN_ACCOUNTED).is_some());
        assert!(checker.discovery(NOTE_HOLDS_WHEN_PRINTED).is_some());
        for property in [
            REVOKED_ONLY_WHEN_DURABLE,
            ONLY_AGENT_TOKEN_HELD,
            NO_CLI_TOKEN_REVOKED,
        ] {
            assert!(checker.discovery(property).is_none(), "{property} breaks");
        }
        for row in COVERAGE.iter().filter(|row| row.reach != Reach::Fix) {
            assert!(
                checker.discovery(row.name).is_some(),
                "{} is never reached",
                row.name
            );
        }
    }
    /// `persist_unflushed_bytes`: without the `sync_all` before the rename (:75-78), a
    /// login revokes P after a save whose bytes were never flushed.
    #[test]
    fn the_temp_file_is_flushed_before_the_rename() {
        let path = breaks(
            as_is_without(Broken {
                persist_unflushed_bytes: true,
                ..Broken::default()
            }),
            REVOKED_ONLY_WHEN_DURABLE,
        );
        let broken = last(&path);
        assert_eq!(broken.took, Some(Branch::Deleted));
        assert!(broken.logins.iter().any(|login| login.revoked
            && login.flushes
                == Some(Flushes {
                    bytes: false,
                    dir: true
                })));
    }

    /// `revoke_after_unflushed`: without `fate`'s Unflushed arm (:229-235), a login whose
    /// directory flush failed revokes P.
    #[test]
    fn an_unflushed_save_keeps_the_token_before() {
        let path = breaks(
            as_is_without(Broken {
                revoke_after_unflushed: true,
                ..Broken::default()
            }),
            REVOKED_ONLY_WHEN_DURABLE,
        );
        let broken = last(&path);
        assert_eq!(broken.took, Some(Branch::Deleted));
        assert!(
            broken
                .logins
                .iter()
                .any(|login| login.revoked && login.flushes.is_some_and(|flushes| !flushes.dir))
        );
    }

    /// The first unraced path that leaves a token unaccounted under `broken`, on the code as
    /// it is.
    fn unraced_orphan(broken: Broken) -> State {
        last(&breaks_where(
            as_is_without(broken),
            EVERY_TOKEN_ACCOUNTED,
            |path| !last(path).raced,
        ))
    }

    /// `fate_ignores_cloud`: without `fate`'s Cloud check (:213-220), a P in another Cloud
    /// under the same Organization slug reaches `revoke`, which lists this Cloud, finds no
    /// P and says nothing. P stays live and unnamed.
    #[test]
    fn fate_leaves_a_token_in_another_cloud() {
        let broken = unraced_orphan(Broken {
            fate_ignores_cloud: true,
            ..Broken::default()
        });
        let p = broken.token(Tok::P);
        assert_eq!(p.place, Place::OtherCloud);
        assert!(p.live && !accounted(&broken, Tok::P));
    }

    /// `fate_ignores_organization`: without `fate`'s Organization check (:221-228), a P
    /// in another Organization reaches `revoke`, which lists this one and says nothing.
    #[test]
    fn fate_leaves_a_token_in_another_organization() {
        let broken = unraced_orphan(Broken {
            fate_ignores_organization: true,
            ..Broken::default()
        });
        let p = broken.token(Tok::P);
        assert_eq!(p.place, Place::OtherOrganization);
        assert!(p.live && !accounted(&broken, Tok::P));
    }

    /// `revoke_unlisted`: without the listed-as-agent check (:246-252), a login revokes the
    /// cli token an edited mcp.json names.
    #[test]
    fn revoke_deletes_only_a_listed_agent_token() {
        let path = breaks(
            as_is_without(Broken {
                revoke_unlisted: true,
                ..Broken::default()
            }),
            NO_CLI_TOKEN_REVOKED,
        );
        let broken = last(&path);
        assert_eq!(broken.took, Some(Branch::Deleted));
        assert_eq!(broken.token(Tok::P).kind, Kind::Cli);
        assert!(!broken.token(Tok::P).live);
    }

    /// `keep_non_agent`: without the kind check (:109, :119-124), a login renames a cli
    /// token into mcp.json.
    #[test]
    fn the_kind_check_refuses_a_cli_token() {
        let path = breaks(
            as_is_without(Broken {
                keep_non_agent: true,
                ..Broken::default()
            }),
            ONLY_AGENT_TOKEN_HELD,
        );
        let broken = last(&path);
        let File::Holds(held) = broken.file else {
            panic!("mcp.json holds a token");
        };
        assert_eq!(broken.token(held).kind, Kind::Cli);
    }

    /// `failed_save_keeps_new_token`: without the revoke of the token just made and its
    /// warning (:125-136), a failed save leaves that token live, held by no file and
    /// named by no warning.
    #[test]
    fn a_failed_save_revokes_the_token_just_made() {
        let broken = unraced_orphan(Broken {
            failed_save_keeps_new_token: true,
            ..Broken::default()
        });
        let orphan = (0..MAX_LOGINS)
            .find(|who| !accounted(&broken, Tok::of(*who)) && counts(broken.token(Tok::of(*who))))
            .expect("a login's own token is unaccounted");
        assert_eq!(broken.pc(orphan), Pc::Done(Outcome::Err));
    }

    /// The fix holds, and every `sometimes` it reaches is reached.
    #[test]
    fn the_fix_holds() {
        let checker = check(fixed());
        println!("fixed: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// `fix_unlocked`: without the lock, the fix is the code as it is, and the race comes
    /// back.
    #[test]
    fn the_fix_needs_the_lock_from_the_read() {
        let path = breaks(
            fixed_without(Broken {
                fix_unlocked: true,
                ..Broken::default()
            }),
            EVERY_TOKEN_ACCOUNTED,
        );
        assert!(last(&path).raced);
    }

    /// `fix_releases_before_report`: a lock released right after the rename keeps every
    /// token accounted, but lets the other login read this one's token and rename before
    /// this one's note: the second route to a stale note.
    #[test]
    fn the_fix_needs_the_lock_through_the_report() {
        let early = fixed_without(Broken {
            fix_releases_before_report: true,
            ..Broken::default()
        });
        let path = breaks(early, NOTE_HOLDS_WHEN_PRINTED);
        let end = last(&path);
        let Some(Branch::Noted { who, .. }) = end.took else {
            panic!("the last step is a note");
        };
        let other = if who == A { B } else { A };
        assert_eq!(end.login(other).previous, Some(Tok::of(who)));
        assert!(!end.raced);
        let checker = check(early);
        assert!(checker.discovery(EVERY_TOKEN_ACCOUNTED).is_none());
    }

    /// Under the fix, the lock is free at the start, the first login to read takes it, and
    /// the other login is offered no step until the holder returns.
    #[test]
    fn the_fix_holds_off_a_second_login() {
        let model = fixed();
        let start = McpLogin::start(Start::Here);
        assert_eq!(start.lock, None);
        assert!(model.offers(&start, A, How::Goes) && model.offers(&start, B, How::Goes));
        let mut held = start;
        for _ in 0..5 {
            held = model.next_state(&held, goes(A)).expect("A steps");
            assert_eq!(held.lock, Some(A));
            for how in [How::Goes, How::Fails, How::Unflushed] {
                assert!(!model.offers(&held, B, how), "B is offered {how:?}");
            }
        }
        let done = model.next_state(&held, goes(A)).expect("A reports");
        assert_eq!(done.pc(A), Pc::Done(Outcome::Ok));
        assert_eq!(done.lock, None);
        assert!(model.offers(&done, B, How::Goes));
    }

    /// Under the fix, a holder whose save fails, or whose kind check refuses, revokes its
    /// token, releases the lock as it returns, and the next login reads the unchanged P.
    #[test]
    fn the_fix_releases_the_lock_when_a_login_fails() {
        let fails = |who| Action {
            who,
            how: How::Fails,
        };
        for a in [
            [goes(A), goes(A), fails(A), goes(A)],
            [goes(A), fails(A), goes(A), goes(A)],
        ] {
            let mut schedule = a.to_vec();
            schedule.push(goes(B));
            let path = follow(fixed(), Start::Here, &schedule);
            let end = last(&path);
            assert_eq!(end.pc(A), Pc::Done(Outcome::Err));
            assert!(!end.token(Tok::of(A)).live);
            assert_eq!(end.took, Some(Branch::Read(File::Holds(Tok::P))));
            assert_eq!(end.lock, Some(B));
        }
    }

    /// Under the fix, a first login with no mcp.json saves TA, and the next login reads TA
    /// as its token before and revokes it.
    #[test]
    fn the_fix_revokes_a_first_login_s_token() {
        let path = follow(
            fixed(),
            Start::Absent,
            &[
                goes(A),
                goes(A),
                goes(A),
                goes(A),
                goes(B),
                goes(B),
                goes(B),
                goes(B),
                goes(B),
                goes(B),
            ],
        );
        assert_eq!(
            steps_of(&path, B).first(),
            Some(&Branch::Read(File::Holds(Tok::of(A))))
        );
        let end = last(&path);
        assert_eq!(end.file, File::Holds(Tok::of(B)));
        assert!(!end.token(Tok::of(A)).live);
        assert!(every_token_accounted(&fixed(), &end));
    }

    /// Under the fix, a holder that leaves P live and named (Unflushed, or P in another
    /// Organization or Cloud) still hands its own token to the next login, which revokes
    /// it, while P stays named.
    #[test]
    fn the_fix_revokes_the_holder_s_token_and_keeps_the_earlier_one_named() {
        let unflushed = Action {
            who: A,
            how: How::Unflushed,
        };
        let b = [goes(B), goes(B), goes(B), goes(B), goes(B), goes(B)];
        for (start, a) in [
            (Start::Here, [goes(A), goes(A), unflushed, goes(A)]),
            (
                Start::OtherOrganization,
                [goes(A), goes(A), goes(A), goes(A)],
            ),
            (Start::OtherCloud, [goes(A), goes(A), goes(A), goes(A)]),
        ] {
            let mut schedule = a.to_vec();
            schedule.extend(b);
            let path = follow(fixed(), start, &schedule);
            let end = last(&path);
            let p = end.token(Tok::P);
            assert!(p.live && p.named_left, "{start:?}: P stays live and named");
            assert!(!end.token(Tok::of(A)).live, "{start:?}: B revoked TA");
            assert_eq!(end.file, File::Holds(Tok::of(B)));
            assert!(every_token_accounted(&fixed(), &end));
        }
    }

    /// The finding's logins under the fix: B is offered no read while A holds the lock, so
    /// B reads only after A returns. B reads TA, revokes it, and every `always` holds.
    #[test]
    fn the_fix_serializes_the_finding_s_logins() {
        let model = fixed();
        let after_a_read = model
            .next_state(&McpLogin::start(Start::Here), goes(A))
            .expect("A reads");
        assert!(!model.offers(&after_a_read, B, How::Goes));
        let path = follow(
            model,
            Start::Here,
            &[
                goes(A),
                goes(A),
                goes(A),
                goes(A),
                goes(A),
                goes(A),
                goes(B),
                goes(B),
                goes(B),
                goes(B),
                goes(B),
                goes(B),
            ],
        );
        assert_eq!(
            steps_of(&path, B).first(),
            Some(&Branch::Read(File::Holds(Tok::of(A))))
        );
        let properties = model.properties();
        for (state, _) in &path {
            for property in &properties {
                if matches!(property.expectation, Expectation::Always) {
                    assert!(
                        (property.condition)(&model, state),
                        "{} breaks",
                        property.name
                    );
                }
            }
        }
        let end = last(&path);
        assert!(!end.token(Tok::P).live && !end.token(Tok::of(A)).live);
        assert_eq!(end.file, File::Holds(Tok::of(B)));
    }

    /// `accounted` at its edges: held, revoked, named by a `Left` warning, named by the
    /// failed-save warning, and named by neither; `counts` leaves out a cli token and the
    /// unknowable token of an unreadable start.
    #[test]
    fn a_token_is_accounted_only_when_held_revoked_or_named() {
        let base = McpLogin::start(Start::Here);
        assert!(accounted(&base, Tok::P), "held");
        let mut moved = base;
        moved.file = File::Holds(Tok::of(A));
        *moved.token_mut(Tok::of(A)) = Token::live(Place::Here, Kind::Agent);
        assert!(!accounted(&moved, Tok::P), "named by neither");
        let mut revoked = moved;
        revoked.token_mut(Tok::P).live = false;
        assert!(accounted(&revoked, Tok::P), "revoked");
        let mut left = moved;
        left.token_mut(Tok::P).named_left = true;
        assert!(accounted(&left, Tok::P), "named by a Left warning");
        let mut unrevoked = moved;
        unrevoked.token_mut(Tok::P).named_unrevoked = true;
        assert!(
            accounted(&unrevoked, Tok::P),
            "named by the failed-save warning"
        );
        let cli = McpLogin::start(Start::Cli);
        assert!(!counts(cli.token(Tok::P)), "a cli token is not counted");
        let unreadable = McpLogin::start(Start::Unreadable);
        assert!(
            Tok::all().all(|tok| !counts(unreadable.token(tok))),
            "an unreadable start counts no token"
        );
    }
}
