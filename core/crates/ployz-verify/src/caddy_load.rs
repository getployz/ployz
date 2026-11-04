//! The daemon's Caddy watch loop: `watch_caddy` (ployzd/src/ingress.rs) deriving the
//! Ingress Projection each pass, loading it into the Ingress Proxy through Caddy's
//! admin or writing the Caddyfile a starting Caddy reads, and publishing the
//! routed-address claim (`PublishedRoutes.addresses`) that `ployz status` reports,
//! while Caddy restarts in place, is replaced, stops and starts, and the projection
//! changes.
//!
//! The property: at rest after a pass that read its inputs, the claim is `None` or
//! names the addresses of what Caddy serves. The harm when it breaks: status shows
//! every route confirmed while Caddy serves an older config, or one with a site left
//! out, so a Deploy reads reachable when it is not (DESIGN.md bet 8).
//!
//! Each watcher step is one statement of `watch_caddy` or of what it calls, in the
//! order the code runs them:
//!
//! - the `loaded` cache (`CaddyInput::loaded`), which skips the admin while Caddy
//!   runs and the last load matches;
//! - `connect`, and its `Err` arm that counts as a failure without the admin;
//! - the equal-input skip, and `CaddyCertificates::loaded_only` for a file written
//!   without the admin;
//! - `apply_caddy`: `write_certificate_files` (`BeforeServing`), `caddy::load`
//!   (`BeforeServing`), `caddy::write_caddyfile` (`AfterServing` with the Caddyfile
//!   unwritten) and `remove_stale_certificate_files` (`AfterServing` with it written);
//! - `CaddyCertificates::find_refused` statement by statement, and the `Probe` arms,
//!   where `Refused` re-derives with `continue` and skips the record write;
//! - the `(applied, admin)` match that sets `last_input` and accepts certificates;
//! - the keep rule (`refused && recorded_by == caddy`) and the record write;
//! - the retry wait, the read's `Err` arm that only logs, and the debounce wait.
//!
//! Caddy is an actor outside the watcher. It holds its container id, what its process
//! serves and the Caddyfile on disk. It restarts in place under the same id (`docker
//! restart`), which the watcher never sees, since the replicated observation carries no
//! start time (#2787); it is replaced by a new container; it stops and starts. A
//! started Caddy serves the Caddyfile. The replicated store changes the projection.
//!
//! Bounds: one Machine that takes ingress, one ingress container at a time (X, replaced
//! once by Y), two projections, one pinned certificate, Caddy events only while the
//! watcher waits and one per wait, so no restart lands mid-pass. Out of scope: the
//! route acks and `PublishedRoutes.routes`, `down` and the kept route, Caddy events
//! during a pass, two pinned certificates, the role turned off, and a Machine with no
//! ingress container.

use std::fmt::Write as _;

use stateright::{Checker, HasDiscoveries, Model, Property};

/// The newest local ingress container (`newest_local_ingress`).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Container {
    X,
    Y,
}

/// An Ingress Projection as `next_pass` derives it, or the config Caddy rendered from
/// one. `routed_addresses` gives each a different address map, so a claim names the
/// config it was derived from.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Config {
    /// Before the change: one HTTP site.
    P0,
    /// After the change: the HTTP site at a new address, and an HTTPS site pinning
    /// certificate `c`.
    P1,
    /// `P1` with `c`'s material left out: `without(P1, {c})` in a probe,
    /// `loaded_only(P1)` while `c` is not accepted, `quarantine(P1)` while `c` is
    /// refused. Without material the HTTPS site has no route, so its addresses differ.
    P1WithoutC,
}

impl Config {
    /// `pinned_stem` names `c`: the site has material and an HTTPS route.
    fn pins_c(self) -> bool {
        self == Self::P1
    }

    /// `without(self, {c})`.
    fn without_c(self) -> Self {
        match self {
            Self::P1 => Self::P1WithoutC,
            Self::P0 | Self::P1WithoutC => self,
        }
    }
}

/// What Caddy's process serves, and what a claim says it serves.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Served {
    /// The process is at rest (`serves_nothing`); as a claim, `Some({})`.
    Nothing,
    Config(Config),
}

impl Served {
    fn pins_c(self) -> bool {
        match self {
            Self::Nothing => false,
            Self::Config(config) => config.pins_c(),
        }
    }
}

/// The Ingress Proxy, outside the watcher.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Caddy {
    container: Container,
    serves: Served,
    /// The Caddyfile on disk, which a starting Caddy reads.
    caddyfile: Config,
    /// Restarted, replaced, stopped or started since the watcher last read its
    /// container.
    unseen: bool,
}

/// `CaddyInput` (ingress.rs).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct CaddyInput {
    projection: Config,
    caddy: Container,
    through_admin: bool,
}

impl CaddyInput {
    /// `CaddyInput::loaded`.
    fn loaded(self, projection: Config, caddy: Container) -> bool {
        self.through_admin && self.projection == projection && self.caddy == caddy
    }
}

/// `CaddyCertificates` for the one stem `c`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Default)]
struct Certificates {
    accepted_c: bool,
    refused_c: bool,
}

/// What `find_refused` left Caddy serving (`Probe`).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Probe {
    Unchanged,
    Reloaded,
    Refused,
}

/// `result` at the record write, by where the pass got it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Outcome {
    /// The `loaded` cache: the last load is what Caddy serves, so the pass counts as
    /// Ok without connecting.
    Cached,
    /// The input equals the last, so nothing was applied.
    Unchanged,
    /// `apply_caddy` returned Ok.
    Applied,
    /// `ApplyError::BeforeServing`: Caddy still serves what it had. Also `connect`'s
    /// `Err` arm, which the code maps to this.
    BeforeServing,
    /// `ApplyError::AfterServing`: Caddy serves the new config, and the Caddyfile or
    /// the stale certificate files may not match it.
    AfterServing,
}

/// Where the watcher is in `watch_caddy`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Pc {
    /// `wait_for_debounced_change`, or `wait_before_retry` after a failed pass. Caddy
    /// and the store act here.
    Wait,
    /// The top of the inner loop: `replicated.containers()` and `certificate_state()`,
    /// reached from the wait or from `Probe::Refused`'s `continue`.
    Read,
    /// `let loaded = !no_caddy && last_input.loaded(..)`.
    Cached,
    /// `connect().await`.
    Connect,
    /// `last_input.as_ref() == Some(&input)`.
    Same { admin: bool },
    /// `apply_caddy`: `write_certificate_files`.
    WriteCertificates { admin: bool },
    /// `apply_caddy`: `caddy::load`, which adapts and loads through the admin when it
    /// has one and only renders without.
    Load { admin: bool },
    /// `apply_caddy`: `caddy::write_caddyfile`.
    WriteCaddyfile { admin: bool },
    /// `apply_caddy`: `remove_stale_certificate_files`.
    RemoveStale { admin: bool },
    /// `find_refused`: `admin.loaded_certificates()`, then the suspects.
    ProbeLoaded,
    /// `find_refused`: `caddy::load(&without(projection, &excluded), ..)`.
    ProbeWithout,
    /// `find_refused`: the candidate with `c` back in, loaded twice on an admin error.
    ProbeCandidate,
    /// The `Probe` arms, then the `(applied, admin)` match.
    Settle {
        admin: bool,
        applied: Outcome,
        probe: Probe,
    },
    /// The keep rule and the record write, then the retry or the acks.
    Record {
        through_admin: bool,
        outcome: Outcome,
        /// `Probe::Refused` under the fix: record, then `continue` to the read.
        continues: bool,
    },
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    caddy: Caddy,
    /// What `next_pass` derives before `quarantine`.
    projection: Config,
    pc: Pc,
    last_input: Option<CaddyInput>,
    /// The Caddy whose admin confirmed the published addresses.
    recorded_by: Option<Container>,
    certificates: Certificates,
    /// `PublishedRoutes.addresses`.
    addresses: Option<Served>,
    /// What the watcher's last successful `write_caddyfile` wrote, `None` before the
    /// first. The fix reads it as `CaddyFile::Current` when it holds the claim.
    written: Option<Config>,
}

impl State {
    /// `certificates.quarantine(&mut projection)`: the projection the pass holds.
    fn derived(&self) -> Config {
        if self.projection.pins_c() && self.certificates.refused_c {
            self.projection.without_c()
        } else {
            self.projection
        }
    }

    /// `serves_nothing`: the Machine takes ingress and its ingress container is at
    /// rest.
    fn no_caddy(&self) -> bool {
        self.caddy.serves == Served::Nothing
    }

    /// `routed_addresses(&projection, &routes)`.
    fn addresses(&self) -> Served {
        Served::Config(self.derived())
    }

    /// What `apply_caddy` applies: the projection with the admin, `loaded_only` of it
    /// without.
    fn applying(&self, admin: bool) -> Config {
        let projection = self.derived();
        if admin || self.certificates.accepted_c {
            projection
        } else {
            projection.without_c()
        }
    }

    fn input(&self, admin: bool) -> CaddyInput {
        CaddyInput {
            projection: self.derived(),
            caddy: self.caddy.container,
            through_admin: admin,
        }
    }

    /// The claim names what the Caddyfile holds, as the fix's `CaddyFile::Current`.
    fn claim_is_written(&self) -> bool {
        self.written
            .is_some_and(|written| self.addresses == Some(Served::Config(written)))
    }

    /// The watcher waits after a pass that read its inputs. Caddy events the watcher
    /// has not had a pass to read are not its to answer for.
    fn at_rest(&self) -> bool {
        self.pc == Pc::Wait && !self.caddy.unseen
    }
}

/// Whether a call succeeded.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Call {
    Ok,
    Err,
}

/// What `connect` returned.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Admin {
    Some,
    None,
    Err,
}

/// What `caddy::load` returned through the admin.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Load {
    Ok,
    /// `caddy::Error::Admin`: Caddy refused the config.
    Refused,
    /// Any other error, such as a timeout.
    Failed,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    Read(Call),
    Cached,
    Connect(Admin),
    Same,
    WriteCertificates(Call),
    Load(Load),
    WriteCaddyfile(Call),
    RemoveStale(Call),
    ProbeLoaded(Call),
    ProbeWithout(Call),
    ProbeCandidate(Load),
    Settle,
    Record,
    /// Caddy restarts in place under the same container id and serves the Caddyfile.
    Restart,
    /// A new ingress container replaces the old and serves the Caddyfile.
    Replace,
    /// Caddy's process stops.
    Stop,
    /// A stopped Caddy starts and serves the Caddyfile.
    Start,
    /// The replicated store changes, so the next pass derives this projection.
    Change(Config),
}

/// A guard the code has, taken out to show what it keeps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Broken {
    /// `!no_caddy` in the `loaded` cache: a stopped Caddy still counts as serving
    /// what it loaded.
    cache_ignores_a_stopped_caddy: bool,
    /// `recorded_by = None` on `Probe::Reloaded`: the recording Caddy stands after a
    /// probe reloaded it.
    reloaded_keeps_recorded_by: bool,
    /// `recorded_by == caddy` in the keep rule: a refusal keeps the claim for any
    /// recording Caddy, not only the newest container.
    keep_ignores_the_container: bool,
    /// `if through_admin` in the record write: the addresses are claimed without the
    /// admin's confirmation.
    claims_without_admin: bool,
}

/// The fix E-20 lands, in two parts, each closing one finding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Fix {
    /// The keep rule keeps the claim only while the Caddyfile holds what was recorded
    /// (`CaddyFile::Current`), so a refusal after a failed write claims nothing.
    keep_needs_current_file: bool,
    /// `Probe::Refused` runs the record write before its `continue`, so the claim is
    /// written on every exit from a pass.
    refused_probe_records: bool,
}

impl Fix {
    const BOTH: Self = Self {
        keep_needs_current_file: true,
        refused_probe_records: true,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CaddyLoad {
    /// Caddy may restart in place under the same container id.
    restarts: bool,
    /// The projection read may fail.
    reads_fail: bool,
    fix: Fix,
    broken: Broken,
}

impl CaddyLoad {
    /// The code as it is, with Caddy and the store doing everything the bounds allow.
    const AS_IS: Self = Self {
        restarts: true,
        reads_fail: true,
        fix: Fix {
            keep_needs_current_file: false,
            refused_probe_records: false,
        },
        broken: Broken {
            cache_ignores_a_stopped_caddy: false,
            reloaded_keeps_recorded_by: false,
            keep_ignores_the_container: false,
            claims_without_admin: false,
        },
    };

    fn step(&self, state: &mut State, action: Action) {
        match action {
            Action::Read(Call::Ok) => {
                // `quarantine` keeps `c` refused only while the projection still pins it.
                state.certificates.refused_c &= state.projection.pins_c();
                state.caddy.unseen = false;
                state.pc = Pc::Cached;
            }
            Action::Read(Call::Err) => state.pc = Pc::Wait,
            Action::Cached => {
                let running = !state.no_caddy() || self.broken.cache_ignores_a_stopped_caddy;
                let loaded = running
                    && state
                        .last_input
                        .is_some_and(|last| last.loaded(state.derived(), state.caddy.container));
                state.pc = if loaded {
                    Pc::Record {
                        through_admin: true,
                        outcome: Outcome::Cached,
                        continues: false,
                    }
                } else {
                    Pc::Connect
                };
            }
            Action::Connect(Admin::Some) => state.pc = Pc::Same { admin: true },
            Action::Connect(Admin::None) => state.pc = Pc::Same { admin: false },
            Action::Connect(Admin::Err) => {
                state.pc = Pc::Record {
                    through_admin: false,
                    outcome: Outcome::BeforeServing,
                    continues: false,
                };
            }
            Action::Same => {
                let Pc::Same { admin } = state.pc else {
                    return;
                };
                state.pc = if state.last_input == Some(state.input(admin)) {
                    Pc::Record {
                        through_admin: admin,
                        outcome: Outcome::Unchanged,
                        continues: false,
                    }
                } else {
                    Pc::WriteCertificates { admin }
                };
            }
            Action::WriteCertificates(call) => {
                let Pc::WriteCertificates { admin } = state.pc else {
                    return;
                };
                state.pc = match call {
                    Call::Ok => Pc::Load { admin },
                    Call::Err => Pc::Settle {
                        admin,
                        applied: Outcome::BeforeServing,
                        probe: Probe::Unchanged,
                    },
                };
            }
            Action::Load(load) => {
                let Pc::Load { admin } = state.pc else {
                    return;
                };
                state.pc = match load {
                    Load::Ok => {
                        if admin {
                            state.caddy.serves = Served::Config(state.applying(admin));
                        }
                        Pc::WriteCaddyfile { admin }
                    }
                    Load::Refused => Pc::ProbeLoaded,
                    Load::Failed => Pc::Settle {
                        admin,
                        applied: Outcome::BeforeServing,
                        probe: Probe::Unchanged,
                    },
                };
            }
            Action::WriteCaddyfile(call) => {
                let Pc::WriteCaddyfile { admin } = state.pc else {
                    return;
                };
                state.pc = match call {
                    Call::Ok => {
                        let applying = state.applying(admin);
                        state.caddy.caddyfile = applying;
                        state.written = Some(applying);
                        Pc::RemoveStale { admin }
                    }
                    Call::Err => Pc::Settle {
                        admin,
                        applied: if admin {
                            Outcome::AfterServing
                        } else {
                            Outcome::BeforeServing
                        },
                        probe: Probe::Unchanged,
                    },
                };
            }
            Action::RemoveStale(call) => {
                let Pc::RemoveStale { admin } = state.pc else {
                    return;
                };
                state.pc = Pc::Settle {
                    admin,
                    applied: match call {
                        Call::Ok => Outcome::Applied,
                        Call::Err => Outcome::AfterServing,
                    },
                    probe: Probe::Unchanged,
                };
            }
            Action::ProbeLoaded(call) => {
                if call == Call::Ok {
                    state.certificates.accepted_c = state.caddy.serves.pins_c();
                }
                let suspect = state.derived().pins_c() && !state.certificates.accepted_c;
                state.pc = if suspect {
                    Pc::ProbeWithout
                } else {
                    Pc::Settle {
                        admin: true,
                        applied: Outcome::BeforeServing,
                        probe: Probe::Unchanged,
                    }
                };
            }
            Action::ProbeWithout(call) => {
                state.pc = match call {
                    Call::Ok => {
                        state.caddy.serves = Served::Config(state.derived().without_c());
                        Pc::ProbeCandidate
                    }
                    Call::Err => Pc::Settle {
                        admin: true,
                        applied: Outcome::BeforeServing,
                        probe: Probe::Unchanged,
                    },
                };
            }
            Action::ProbeCandidate(load) => {
                let probe = match load {
                    Load::Ok => {
                        state.caddy.serves = Served::Config(state.derived());
                        state.certificates.accepted_c = true;
                        Probe::Reloaded
                    }
                    Load::Refused => {
                        state.certificates.refused_c = true;
                        state.certificates.accepted_c = false;
                        Probe::Refused
                    }
                    Load::Failed => {
                        state.certificates.accepted_c = false;
                        Probe::Reloaded
                    }
                };
                state.pc = Pc::Settle {
                    admin: true,
                    applied: Outcome::BeforeServing,
                    probe,
                };
            }
            Action::Settle => {
                let Pc::Settle {
                    admin,
                    applied,
                    probe,
                } = state.pc
                else {
                    return;
                };
                match probe {
                    Probe::Unchanged => {}
                    Probe::Reloaded => {
                        state.last_input = None;
                        if !self.broken.reloaded_keeps_recorded_by {
                            state.recorded_by = None;
                        }
                    }
                    Probe::Refused => {
                        state.last_input = None;
                        state.recorded_by = None;
                    }
                }
                match (applied, admin) {
                    (Outcome::Applied, true) => {
                        state.certificates.accepted_c = state.derived().pins_c();
                        state.last_input = Some(state.input(admin));
                    }
                    (Outcome::Applied, false) => state.last_input = Some(state.input(admin)),
                    (Outcome::BeforeServing, true) => {}
                    (Outcome::AfterServing, true) => {
                        state.certificates.accepted_c = state.derived().pins_c();
                        state.last_input = None;
                    }
                    (Outcome::BeforeServing | Outcome::AfterServing, false) => {
                        state.last_input = None;
                    }
                    (Outcome::Cached | Outcome::Unchanged, _) => {}
                }
                state.pc = match probe {
                    Probe::Refused if self.fix.refused_probe_records => Pc::Record {
                        through_admin: true,
                        outcome: applied,
                        continues: true,
                    },
                    Probe::Refused => Pc::Read,
                    Probe::Unchanged | Probe::Reloaded => Pc::Record {
                        through_admin: admin,
                        outcome: applied,
                        continues: false,
                    },
                };
            }
            Action::Record => {
                let Pc::Record {
                    through_admin,
                    outcome,
                    continues,
                } = state.pc
                else {
                    return;
                };
                if !self.keeps(state, through_admin, outcome) {
                    let claimed = through_admin || self.broken.claims_without_admin;
                    let refused = refused(through_admin, outcome);
                    state.addresses = if claimed {
                        (!refused).then(|| state.addresses())
                    } else {
                        state.no_caddy().then_some(Served::Nothing)
                    };
                    state.recorded_by = (through_admin && state.addresses.is_some())
                        .then_some(state.caddy.container);
                }
                state.pc = if continues { Pc::Read } else { Pc::Wait };
            }
            Action::Restart | Action::Start => {
                state.caddy.serves = Served::Config(state.caddy.caddyfile);
                state.caddy.unseen = true;
            }
            Action::Replace => {
                state.caddy.container = Container::Y;
                state.caddy.serves = Served::Config(state.caddy.caddyfile);
                state.caddy.unseen = true;
            }
            Action::Stop => {
                state.caddy.serves = Served::Nothing;
                state.caddy.unseen = true;
            }
            Action::Change(projection) => state.projection = projection,
        }
    }

    /// `replicated.containers()` and `certificate_state()` at the top of the inner
    /// loop, which succeed, or fail when the bounds allow.
    fn reads(&self, actions: &mut Vec<Action>) {
        actions.push(Action::Read(Call::Ok));
        if self.reads_fail {
            actions.push(Action::Read(Call::Err));
        }
    }

    /// The keep rule: a refused load leaves the record standing while the newest
    /// observed ingress container is the one whose admin confirmed it. The fix adds:
    /// and the Caddyfile holds what was recorded.
    fn keeps(&self, state: &State, through_admin: bool, outcome: Outcome) -> bool {
        refused(through_admin, outcome)
            && state.recorded_by.is_some()
            && (self.broken.keep_ignores_the_container
                || state.recorded_by == Some(state.caddy.container))
            && (!self.fix.keep_needs_current_file || state.claim_is_written())
    }
}

/// `let refused = through_admin && matches!(result, Err(ApplyError::BeforeServing(_)))`.
fn refused(through_admin: bool, outcome: Outcome) -> bool {
    through_admin && outcome == Outcome::BeforeServing
}

impl Model for CaddyLoad {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        [Config::P0, Config::P1]
            .into_iter()
            .map(|projection| State {
                caddy: Caddy {
                    container: Container::X,
                    serves: Served::Config(Config::P0),
                    caddyfile: Config::P0,
                    unseen: false,
                },
                projection,
                pc: Pc::Read,
                last_input: None,
                recorded_by: None,
                certificates: Certificates::default(),
                addresses: None,
                written: None,
            })
            .collect()
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        match state.pc {
            Pc::Wait => {
                if !state.caddy.unseen {
                    match state.caddy.serves {
                        Served::Nothing => actions.push(Action::Start),
                        Served::Config(_) => {
                            actions.push(Action::Stop);
                            if self.restarts {
                                actions.push(Action::Restart);
                            }
                        }
                    }
                    if state.caddy.container == Container::X {
                        actions.push(Action::Replace);
                    }
                }
                for projection in [Config::P0, Config::P1] {
                    if projection != state.projection {
                        actions.push(Action::Change(projection));
                    }
                }
                self.reads(actions);
            }
            Pc::Read => self.reads(actions),
            Pc::Cached => actions.push(Action::Cached),
            Pc::Connect => {
                if state.no_caddy() {
                    actions.push(Action::Connect(Admin::None));
                } else {
                    actions.extend([
                        Action::Connect(Admin::Some),
                        Action::Connect(Admin::None),
                        Action::Connect(Admin::Err),
                    ]);
                }
            }
            Pc::Same { .. } => actions.push(Action::Same),
            Pc::WriteCertificates { .. } => actions.extend([
                Action::WriteCertificates(Call::Ok),
                Action::WriteCertificates(Call::Err),
            ]),
            Pc::Load { admin: true } => actions.extend([
                Action::Load(Load::Ok),
                Action::Load(Load::Refused),
                Action::Load(Load::Failed),
            ]),
            Pc::Load { admin: false } => actions.push(Action::Load(Load::Ok)),
            Pc::WriteCaddyfile { .. } => actions.extend([
                Action::WriteCaddyfile(Call::Ok),
                Action::WriteCaddyfile(Call::Err),
            ]),
            Pc::RemoveStale { .. } => actions.extend([
                Action::RemoveStale(Call::Ok),
                Action::RemoveStale(Call::Err),
            ]),
            Pc::ProbeLoaded => actions.extend([
                Action::ProbeLoaded(Call::Ok),
                Action::ProbeLoaded(Call::Err),
            ]),
            Pc::ProbeWithout => actions.extend([
                Action::ProbeWithout(Call::Ok),
                Action::ProbeWithout(Call::Err),
            ]),
            Pc::ProbeCandidate => actions.extend([
                Action::ProbeCandidate(Load::Ok),
                Action::ProbeCandidate(Load::Refused),
                Action::ProbeCandidate(Load::Failed),
            ]),
            Pc::Settle { .. } => actions.push(Action::Settle),
            Pc::Record { .. } => actions.push(Action::Record),
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = *last;
        self.step(&mut state, action);
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always(CLAIM_SERVED, |_, state: &State| {
                !state.at_rest()
                    || state
                        .addresses
                        .is_none_or(|claim| claim == state.caddy.serves)
            }),
            Property::always(CLAIM_CONFIRMED, |_, state: &State| {
                !state.at_rest()
                    || !matches!(state.addresses, Some(Served::Config(_)))
                    || state.recorded_by.is_some()
            }),
            Property::sometimes(
                "Caddy loads the projection through its admin and the claim names it",
                |_, state: &State| {
                    state.at_rest()
                        && state.addresses == Some(Served::Config(Config::P1))
                        && state.recorded_by.is_some()
                },
            ),
            Property::sometimes(
                "the Caddyfile write fails after Caddy loaded the projection",
                |_, state: &State| {
                    matches!(
                        state.pc,
                        Pc::Settle {
                            applied: Outcome::AfterServing,
                            ..
                        }
                    ) && state.written != Some(state.derived())
                },
            ),
            Property::sometimes(
                "a refusal keeps the claim of the Caddy that confirmed it",
                |model: &Self, state: &State| {
                    matches!(
                        state.pc,
                        Pc::Record {
                            through_admin: true,
                            outcome: Outcome::BeforeServing,
                            ..
                        }
                    ) && model.keeps(state, true, Outcome::BeforeServing)
                },
            ),
            Property::sometimes("a probe quarantines the certificate", |_, state: &State| {
                state.certificates.refused_c
            }),
            Property::sometimes(
                "a probe reloads Caddy and refuses nothing",
                |_, state: &State| {
                    matches!(
                        state.pc,
                        Pc::Settle {
                            probe: Probe::Reloaded,
                            ..
                        }
                    )
                },
            ),
            Property::sometimes(
                "a load refused before serving leaves Caddy unchanged",
                |_, state: &State| {
                    matches!(
                        state.pc,
                        Pc::Settle {
                            admin: true,
                            applied: Outcome::BeforeServing,
                            probe: Probe::Unchanged,
                        }
                    )
                },
            ),
            Property::sometimes(
                "a stopped Caddy claims to serve nothing",
                |_, state: &State| state.at_rest() && state.addresses == Some(Served::Nothing),
            ),
            Property::sometimes(
                "a pass without the admin writes the Caddyfile and claims nothing",
                |_, state: &State| {
                    matches!(
                        state.pc,
                        Pc::Record {
                            through_admin: false,
                            outcome: Outcome::Applied,
                            ..
                        }
                    ) && !state.no_caddy()
                },
            ),
            Property::sometimes(
                "a pass without the admin fails before serving",
                |_, state: &State| {
                    matches!(
                        state.pc,
                        Pc::Record {
                            through_admin: false,
                            outcome: Outcome::BeforeServing,
                            ..
                        }
                    )
                },
            ),
            Property::sometimes("the loaded cache skips the admin", |_, state: &State| {
                matches!(
                    state.pc,
                    Pc::Record {
                        outcome: Outcome::Cached,
                        ..
                    }
                )
            }),
            Property::sometimes("an equal input skips the apply", |_, state: &State| {
                matches!(
                    state.pc,
                    Pc::Record {
                        outcome: Outcome::Unchanged,
                        ..
                    }
                )
            }),
            Property::sometimes("a new container confirms the claim", |_, state: &State| {
                state.at_rest() && state.recorded_by == Some(Container::Y)
            }),
        ];
        if self.restarts {
            properties.push(Property::sometimes(
                "a restarted Caddy serves a Caddyfile older than the claim",
                |_, state: &State| {
                    state.caddy.unseen
                        && state.caddy.serves == Served::Config(state.caddy.caddyfile)
                        && state
                            .addresses
                            .is_some_and(|claim| claim != state.caddy.serves)
                },
            ));
        }
        if self.reads_fail {
            properties.push(Property::sometimes(
                "a read fails and the pass ends without a record write",
                |_, state: &State| state.pc == Pc::Wait && state.caddy.unseen,
            ));
        }
        if self.fix.keep_needs_current_file {
            properties.push(Property::sometimes(
                "a refusal while the Caddyfile is stale claims nothing",
                |_, state: &State| {
                    matches!(
                        state.pc,
                        Pc::Record {
                            through_admin: true,
                            outcome: Outcome::BeforeServing,
                            ..
                        }
                    ) && state.recorded_by == Some(state.caddy.container)
                        && !state.claim_is_written()
                },
            ));
        }
        if self.fix.refused_probe_records {
            properties.push(Property::sometimes(
                "a refused probe records before it re-derives",
                |_, state: &State| {
                    matches!(
                        state.pc,
                        Pc::Record {
                            continues: true,
                            ..
                        }
                    )
                },
            ));
        }
        properties
    }
}

const CLAIM_SERVED: &str = "at rest, the published addresses are what Caddy serves";
const CLAIM_CONFIRMED: &str = "at rest, a claim of addresses names the Caddy that confirmed it";

/// `path` in the code's words: each statement, and the claim, Caddy and the watcher's
/// locals once it ran.
fn story(path: stateright::Path<State, Action>) -> String {
    let mut story = String::new();
    // Stateright pairs each state with the action taken from it; the row lists the
    // action that led to the state instead, so the last row shows the broken state.
    let mut led_by: Option<(State, Action)> = None;
    for (n, (state, action)) in path.into_vec().into_iter().enumerate() {
        let what = led_by.map_or_else(
            || "start".to_owned(),
            |(before, action)| reads_as(&before, action),
        );
        led_by = action.map(|action| (state, action));
        let served = |served: Served| match served {
            Served::Nothing => "nothing".to_owned(),
            Served::Config(config) => format!("{config:?}"),
        };
        let claim = state.addresses.map_or_else(
            || "None".to_owned(),
            |claim| format!("Some({})", served(claim)),
        );
        let last_input = state.last_input.map_or_else(
            || "None".to_owned(),
            |last| {
                format!(
                    "{:?}/{:?}/{}",
                    last.projection,
                    last.caddy,
                    if last.through_admin { "admin" } else { "file" }
                )
            },
        );
        let _ = writeln!(
            story,
            "{n:2}. {what}\n    claim={claim} recorded_by={:?} | caddy {:?} serves {} file {:?}{} | projection {:?} last_input={last_input} accepted={} refused={}",
            state.recorded_by,
            state.caddy.container,
            served(state.caddy.serves),
            state.caddy.caddyfile,
            if state.caddy.unseen { " (unseen)" } else { "" },
            state.projection,
            state.certificates.accepted_c,
            state.certificates.refused_c,
        );
    }
    story
}

/// `action` as the watcher, Caddy or the store ran it from `before`.
fn reads_as(before: &State, action: Action) -> String {
    let admin = |admin: bool| {
        if admin {
            "through the admin"
        } else {
            "without the admin"
        }
    };
    match action {
        Action::Read(Call::Ok) => format!(
            "watcher: replicated.containers() and certificate_state() read; next_pass derives {:?}, quarantine leaves {:?} (watch_caddy)",
            before.projection,
            before.derived()
        ),
        Action::Read(Call::Err) => {
            "watcher: the read fails; the Err arm logs and goes to wait_for_debounced_change (watch_caddy)".to_owned()
        }
        Action::Cached => format!(
            "watcher: `loaded` = !no_caddy && last_input.loaded(projection, caddy): {} (watch_caddy, CaddyInput::loaded)",
            !before.no_caddy()
                && before
                    .last_input
                    .is_some_and(|last| last.loaded(before.derived(), before.caddy.container))
        ),
        Action::Connect(Admin::Some) => "watcher: connect() returns Some(admin) (watch_caddy)".to_owned(),
        Action::Connect(Admin::None) => {
            "watcher: connect() returns None, the admin socket is down (watch_caddy)".to_owned()
        }
        Action::Connect(Admin::Err) => {
            "watcher: connect() fails; counted as (false, Err(BeforeServing)) (watch_caddy)".to_owned()
        }
        Action::Same => "watcher: last_input == input? (watch_caddy)".to_owned(),
        Action::WriteCertificates(Call::Ok) => {
            "watcher: write_certificate_files succeeds (apply_caddy)".to_owned()
        }
        Action::WriteCertificates(Call::Err) => {
            "watcher: write_certificate_files fails: Err(BeforeServing) (apply_caddy)".to_owned()
        }
        Action::Load(Load::Ok) => {
            let Pc::Load { admin: with_admin } = before.pc else {
                return String::new();
            };
            format!(
                "watcher: caddy::load({:?}) {} succeeds (apply_caddy)",
                before.applying(with_admin),
                admin(with_admin)
            )
        }
        Action::Load(Load::Refused) => format!(
            "watcher: caddy::load({:?}) refused by the admin: Err(BeforeServing(Admin)), so find_refused runs (apply_caddy, watch_caddy)",
            before.derived()
        ),
        Action::Load(Load::Failed) => format!(
            "watcher: caddy::load({:?}) fails short of the admin: Err(BeforeServing) (apply_caddy)",
            before.derived()
        ),
        Action::WriteCaddyfile(Call::Ok) => {
            let Pc::WriteCaddyfile { admin } = before.pc else {
                return String::new();
            };
            format!(
                "watcher: caddy::write_caddyfile writes {:?} (apply_caddy)",
                before.applying(admin)
            )
        }
        Action::WriteCaddyfile(Call::Err) => {
            let Pc::WriteCaddyfile { admin: with_admin } = before.pc else {
                return String::new();
            };
            format!(
                "watcher: caddy::write_caddyfile fails {}: Err({}), the Caddyfile still holds {:?} (apply_caddy)",
                admin(with_admin),
                if with_admin { "AfterServing" } else { "BeforeServing" },
                before.caddy.caddyfile
            )
        }
        Action::RemoveStale(Call::Ok) => {
            "watcher: remove_stale_certificate_files succeeds: Ok (apply_caddy)".to_owned()
        }
        Action::RemoveStale(Call::Err) => {
            "watcher: remove_stale_certificate_files fails: Err(AfterServing), the Caddyfile is written (apply_caddy)".to_owned()
        }
        Action::ProbeLoaded(Call::Ok) => format!(
            "watcher: admin.loaded_certificates() replaces `accepted` with what Caddy serves (find_refused): c accepted = {}",
            before.caddy.serves.pins_c()
        ),
        Action::ProbeLoaded(Call::Err) => {
            "watcher: admin.loaded_certificates() fails, `accepted` stays (find_refused)".to_owned()
        }
        Action::ProbeWithout(Call::Ok) => format!(
            "watcher: caddy::load(without(projection, suspects)) loads {:?} (find_refused)",
            before.derived().without_c()
        ),
        Action::ProbeWithout(Call::Err) => {
            "watcher: caddy::load(without(projection, suspects)) fails: Probe::Unchanged (find_refused)".to_owned()
        }
        Action::ProbeCandidate(Load::Ok) => {
            "watcher: the candidate with c loads: accept(projection), Probe::Reloaded (find_refused)".to_owned()
        }
        Action::ProbeCandidate(Load::Refused) => {
            "watcher: the candidate with c is refused twice: c quarantined, accept(without c), Probe::Refused (find_refused)".to_owned()
        }
        Action::ProbeCandidate(Load::Failed) => {
            "watcher: the candidate with c fails short of the admin: break, accept(without c), Probe::Reloaded (find_refused)".to_owned()
        }
        Action::Settle => {
            let Pc::Settle { admin: with_admin, applied, probe } = before.pc else {
                return String::new();
            };
            let arm = match probe {
                Probe::Unchanged => "Probe::Unchanged",
                Probe::Reloaded => "Probe::Reloaded clears last_input and recorded_by",
                Probe::Refused => {
                    "Probe::Refused clears last_input and recorded_by and `continue`s"
                }
            };
            format!(
                "watcher: {arm}; the ({applied:?}, {}) arm (watch_caddy)",
                admin(with_admin)
            )
        }
        Action::Record => {
            let Pc::Record { through_admin, outcome, .. } = before.pc else {
                return String::new();
            };
            format!(
                "watcher: refused = {}; the keep rule checks recorded_by {:?} against caddy {:?}, then the record write (watch_caddy)",
                refused(through_admin, outcome),
                before.recorded_by,
                before.caddy.container
            )
        }
        Action::Restart => format!(
            "caddy: container {:?} restarts in place under the same id and serves its Caddyfile, {:?}",
            before.caddy.container, before.caddy.caddyfile
        ),
        Action::Replace => format!(
            "caddy: a new container replaces {:?} and serves the Caddyfile, {:?}",
            before.caddy.container, before.caddy.caddyfile
        ),
        Action::Stop => "caddy: the process stops".to_owned(),
        Action::Start => format!(
            "caddy: the process starts and serves the Caddyfile, {:?}",
            before.caddy.caddyfile
        ),
        Action::Change(projection) => {
            format!("store: the containers or certificates change; the next pass derives {projection:?}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(model: CaddyLoad) -> impl Checker<CaddyLoad> {
        model.checker().threads(4).spawn_bfs().join()
    }

    /// A path on which `property` fails under `model`, printed as a story.
    fn breaks(model: CaddyLoad, property: &'static str) -> Vec<(State, Option<Action>)> {
        let checker = model
            .checker()
            .threads(4)
            .finish_when(HasDiscoveries::AllOf([property].into()))
            .spawn_bfs()
            .join();
        let path = checker.assert_any_discovery(property);
        println!("{property} fails under {model:?}:\n{}", story(path.clone()));
        path.into_vec()
    }

    fn last(path: &[(State, Option<Action>)]) -> State {
        path.last().expect("a path has a state").0
    }

    fn took(path: &[(State, Option<Action>)], action: Action) -> bool {
        path.iter().any(|(_, taken)| *taken == Some(action))
    }

    /// The finding (#2787). Caddy loads P1 through its admin, then `write_caddyfile`
    /// fails, so the Caddyfile still holds P0 while the claim names P1 and
    /// `recorded_by` names the container. Caddy restarts in place under the same id
    /// and serves P0. The retry is refused before serving, and the keep rule finds
    /// `recorded_by == caddy`, so the claim stays P1. This test passes by finding
    /// that path; it starts failing once the code ties the claim to the Caddyfile.
    #[test]
    fn current_code_keeps_a_claim_a_restarted_caddy_no_longer_serves() {
        let path = breaks(
            CaddyLoad {
                reads_fail: false,
                ..CaddyLoad::AS_IS
            },
            CLAIM_SERVED,
        );
        let last = last(&path);
        assert!(took(&path, Action::WriteCaddyfile(Call::Err)));
        assert!(took(&path, Action::Restart));
        assert_eq!(last.recorded_by, Some(last.caddy.container));
        assert_eq!(last.caddy.serves, Served::Config(last.caddy.caddyfile));
        assert!(
            last.addresses
                .is_some_and(|claim| claim != last.caddy.serves)
        );
    }

    /// The second finding. A probe quarantines `c`: `Probe::Refused` clears
    /// `recorded_by` and `continue`s past the record write, so the claim from the
    /// pass before stands while Caddy serves the probe config. The next read fails,
    /// and its `Err` arm only logs, so the watcher waits on that claim. This test
    /// passes by finding that path; it starts failing once the claim is written on
    /// every exit from a pass.
    #[test]
    fn current_code_leaves_the_pre_probe_claim_standing_when_the_next_read_fails() {
        let path = breaks(
            CaddyLoad {
                restarts: false,
                ..CaddyLoad::AS_IS
            },
            CLAIM_SERVED,
        );
        let last = last(&path);
        assert!(took(&path, Action::ProbeCandidate(Load::Refused)));
        assert!(took(&path, Action::Read(Call::Err)));
        assert_eq!(last.recorded_by, None);
        assert!(last.certificates.refused_c);
        assert_eq!(last.caddy.serves, Served::Config(Config::P1WithoutC));
        assert!(
            last.addresses
                .is_some_and(|claim| claim != last.caddy.serves)
        );
    }

    /// Where the code already holds: while Caddy never restarts in place and every
    /// read succeeds, every claim at rest is what Caddy serves. Every other event
    /// still happens: Caddy stops, starts and is replaced, loads are refused, probes
    /// reload and quarantine, and the projection changes.
    #[test]
    fn current_code_holds_while_caddy_never_restarts_in_place_and_every_read_succeeds() {
        let checker = check(CaddyLoad {
            restarts: false,
            reads_fail: false,
            ..CaddyLoad::AS_IS
        });
        println!(
            "as is, no restart, reads succeed: {} states",
            checker.unique_state_count()
        );
        checker.assert_properties();
    }

    /// The fix: a refusal keeps the claim only while the Caddyfile holds what was
    /// recorded, and `Probe::Refused` records before it re-derives. Every property
    /// holds with Caddy restarting in place and reads failing, and every `sometimes`
    /// is reached.
    #[test]
    fn a_claim_tied_to_the_caddyfile_and_written_on_every_exit_holds() {
        let checker = check(CaddyLoad {
            fix: Fix::BOTH,
            ..CaddyLoad::AS_IS
        });
        println!("fixed: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// Without `!no_caddy` in the `loaded` cache, a pass that sees Caddy stopped
    /// still counts the last load as served and claims its addresses.
    #[test]
    fn the_cache_needs_a_running_caddy() {
        let path = breaks(
            CaddyLoad {
                fix: Fix::BOTH,
                broken: Broken {
                    cache_ignores_a_stopped_caddy: true,
                    ..Broken::default()
                },
                ..CaddyLoad::AS_IS
            },
            CLAIM_SERVED,
        );
        let last = last(&path);
        assert_eq!(last.caddy.serves, Served::Nothing);
        assert!(matches!(last.addresses, Some(Served::Config(_))));
    }

    /// Without `recorded_by = None` on `Probe::Reloaded`, the keep rule finds the
    /// recording container and keeps a claim the probe itself replaced.
    #[test]
    fn a_reloaded_probe_must_clear_the_recording_caddy() {
        let path = breaks(
            CaddyLoad {
                fix: Fix::BOTH,
                broken: Broken {
                    reloaded_keeps_recorded_by: true,
                    ..Broken::default()
                },
                ..CaddyLoad::AS_IS
            },
            CLAIM_SERVED,
        );
        let last = last(&path);
        assert!(took(&path, Action::ProbeWithout(Call::Ok)));
        assert_eq!(last.recorded_by, Some(last.caddy.container));
    }

    /// Without `recorded_by == caddy` in the keep rule, a new container that started
    /// from a stale Caddyfile inherits the old container's claim on its first
    /// refusal. The fix's Caddyfile check covers this on its own, so the guard shows
    /// its worth on the keep rule as it is, with the probe fix in and restarts out,
    /// where the code holds with the guard and breaks without it.
    #[test]
    fn the_keep_rule_needs_the_recording_container() {
        let model = CaddyLoad {
            restarts: false,
            fix: Fix {
                keep_needs_current_file: false,
                refused_probe_records: true,
            },
            ..CaddyLoad::AS_IS
        };
        let checker = check(model);
        println!(
            "keep rule as is, no restart: {} states",
            checker.unique_state_count()
        );
        checker.assert_properties();
        let path = breaks(
            CaddyLoad {
                broken: Broken {
                    keep_ignores_the_container: true,
                    ..Broken::default()
                },
                ..model
            },
            CLAIM_SERVED,
        );
        let last = last(&path);
        assert!(took(&path, Action::Replace));
        assert_eq!(last.recorded_by, Some(Container::X));
        assert_eq!(last.caddy.container, Container::Y);
    }

    /// Without `if through_admin` in the record write, a pass that never reached
    /// the admin, because `connect` failed or found it down, claims the projection's
    /// addresses while Caddy serves what it last loaded.
    #[test]
    fn the_record_needs_the_admins_confirmation() {
        let path = breaks(
            CaddyLoad {
                fix: Fix::BOTH,
                broken: Broken {
                    claims_without_admin: true,
                    ..Broken::default()
                },
                ..CaddyLoad::AS_IS
            },
            CLAIM_SERVED,
        );
        let last = last(&path);
        assert!(
            took(&path, Action::Connect(Admin::None)) || took(&path, Action::Connect(Admin::Err))
        );
        assert_eq!(last.recorded_by, None);
        assert!(matches!(last.addresses, Some(Served::Config(_))));
        assert!(
            last.addresses
                .is_some_and(|claim| claim != last.caddy.serves)
        );
    }
}
