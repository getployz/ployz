//! The step machine. A tool is a plan replayed from the start on every resume:
//! each read, write or wait it reaches either takes its recorded answers or
//! stops the run with the step that gets them. Replay keeps a plan straight-line
//! code while [`State`] stays plain data a host can store between steps.

use ployz_core::decode::decode;
use ployz_core::{RpcError, RpcErrorCode};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use ts_rs::TS;

use super::cloud::Operation;
use super::refusal::{Call, Refusal, ToolResult, Writes, from_store};
use super::schema;
use super::step::{
    Act, Action, Ask, AttemptId, BodyDigest, Fix, Gate, RequestKey, Step, Write, WriteSlot,
};
use crate::{
    Batch, BatchCommand, Command, EnvironmentName, EnvironmentRef, EnvironmentSummary, ProjectName,
    Query, Revision, ServicesQuery, Tell, View, Written,
};

/// Read and write steps one call may take; waits are bounded by time instead.
pub const STEP_BUDGET: u8 = 3;

/// The most a call may wait, in seconds.
pub const WAIT_CAP_SECONDS: u32 = 300;

/// Where a call runs: the Environment open in the client, and which Store
/// answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Place {
    pub project: Option<ProjectName>,
    pub environment: Option<EnvironmentName>,
    pub store: StoreAt,
}

/// Which Store a call reaches.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum StoreAt {
    /// Ployz Cloud: it also answers about Servers and GitHub.
    Cloud,
    /// A Config Store on this computer: Store reads and writes only.
    Local,
}

/// A call between steps: what it was asked and every answer so far.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    tool: String,
    input: Value,
    place: Place,
    call_key: String,
    rounds: Vec<Round>,
    pending: Option<Kind>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Round {
    kind: Kind,
    answers: Vec<Result<Value, RpcError>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Kind {
    Read { count: usize },
    Write,
    Wait { ms: u32, count: usize },
}

impl Kind {
    const fn answers(self) -> usize {
        match self {
            Self::Read { count } | Self::Wait { count, .. } => count,
            Self::Write => 1,
        }
    }
}

/// Why a plan stopped before answering.
pub(crate) enum Stop {
    /// It needs this step's answers.
    Step(Box<Step>, Kind),
    /// It refuses.
    Refuse(Box<Refusal>),
}

impl From<Refusal> for Stop {
    fn from(refusal: Refusal) -> Self {
        Self::Refuse(Box::new(refusal))
    }
}

/// One tool: its input type, how it describes itself, and its plan.
pub(crate) trait Tool: DeserializeOwned + JsonSchema {
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    const GATE: Gate = Gate::Never;
    const READ_ONLY: bool;
    /// The Store read a local call is pointed to when this tool needs Cloud.
    const LOCAL_NEXT: Option<&'static str> = None;

    /// A complete input that decodes.
    fn example() -> Value;

    /// Read, write and wait through `cx`, then render the answer.
    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop>;
}

/// What a model is told about one tool.
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub read_only: bool,
    pub gate: Gate,
}

/// What one run of a plan sees: the recorded rounds and what it has used.
pub(crate) struct Cx<'run> {
    rounds: &'run [Round],
    cursor: usize,
    steps: u8,
    waited_ms: u32,
    slot: Option<WriteSlot>,
    place: &'run Place,
    call: Call,
    call_key: &'run str,
    local_next: Option<Call>,
}

impl Cx<'_> {
    /// The call being run.
    pub(crate) const fn call(&self) -> &Call {
        &self.call
    }

    /// The Environment `project` and `environment` name, else the place's.
    pub(crate) fn scope(
        &self,
        project: Option<&ProjectName>,
        environment: Option<&EnvironmentName>,
    ) -> EnvironmentRef {
        // A Project named alone means its default Environment, not the place's.
        let environment = match (environment, project) {
            (Some(environment), _) => Some(environment.clone()),
            (None, Some(_)) => None,
            (None, None) => self.place.environment.clone(),
        };
        EnvironmentRef {
            project: project.cloned().or_else(|| self.place.project.clone()),
            environment,
        }
    }

    /// The key this call's write lands under.
    pub(crate) fn request_key(&self, given: Option<RequestKey>) -> RequestKey {
        RequestKey::effective(given, self.call_key)
    }

    pub(crate) const fn is_local(&self) -> bool {
        matches!(self.place.store, StoreAt::Local)
    }

    /// Answers to `asks`, read in one step.
    pub(crate) fn read(&mut self, asks: Vec<Ask>) -> Result<Vec<Result<Value, RpcError>>, Stop> {
        if self.is_local() && asks.iter().any(|ask| matches!(ask, Ask::Cloud(_))) {
            return Err(Refusal::not_here(self.local_next.clone()).into());
        }
        self.steps += 1;
        let kind = Kind::Read { count: asks.len() };
        match self.recorded(kind)? {
            Some(answers) => Ok(answers),
            None => self.next_step(Step::Read { asks }, kind),
        }
    }

    /// One Store query's answer.
    pub(crate) fn one<Q: crate::Ask>(&mut self, query: &Q) -> Result<Q::View, Stop> {
        let mut answers = self.read(vec![store(query)?])?.into_iter();
        self.view::<Q>(answers.next())
    }

    /// A Store answer as `Q`'s view; a Store error becomes the refusal.
    pub(crate) fn view<Q: crate::Ask>(
        &self,
        answer: Option<Result<Value, RpcError>>,
    ) -> Result<Q::View, Stop> {
        let answer = answer.ok_or_else(|| Refusal::internal("A read came back short"))?;
        let value = answer.map_err(|error| from_store(error, &self.call, Writes::None))?;
        let view = serde_json::from_value::<View>(value)
            .map_err(|_| Refusal::internal("The Config Store answered in an unknown shape"))?;
        Q::view(view).map_err(|error| from_store(error, &self.call, Writes::None).into())
    }

    /// A Cloud answer decoded as `T`.
    pub(crate) fn cloud<T: DeserializeOwned>(
        &self,
        answer: Option<Result<Value, RpcError>>,
    ) -> Result<T, Stop> {
        let answer = answer.ok_or_else(|| Refusal::internal("A read came back short"))?;
        let value = answer.map_err(|error| from_store(error, &self.call, Writes::None))?;
        serde_json::from_value(value)
            .map_err(|_| Refusal::internal("Ployz Cloud answered in an unknown shape").into())
    }

    /// The call's one write: a Store command.
    pub(crate) fn write<C: Tell>(&mut self, command: &C) -> Result<C::Written, Stop> {
        self.write_with(command, from_store)
    }

    /// The call's one write, a Store refusal read by `refuse` instead of
    /// [`from_store`], for a refusal whose details the tool reshapes.
    pub(crate) fn write_with<C: Tell>(
        &mut self,
        command: &C,
        refuse: impl FnOnce(RpcError, &Call, Writes) -> Refusal,
    ) -> Result<C::Written, Stop> {
        let Some(slot) = self.slot.take() else {
            return Err(Refusal::internal("A call writes at most once").into());
        };
        self.steps += 1;
        let Some(answers) = self.recorded(Kind::Write)? else {
            let command = serde_json::from_value::<Command>(command.to_command())
                .map_err(|_| Refusal::internal("A command did not encode"))?;
            return self.next_step(slot.spend(Write::Store(command)), Kind::Write);
        };
        let answer = answers
            .into_iter()
            .next()
            .ok_or_else(|| Refusal::internal("A write came back without an answer"))?;
        let value = answer.map_err(|error| {
            let writes = writes_of(&error);
            refuse(error, &self.call, writes)
        })?;
        let written = serde_json::from_value::<Written>(value)
            .map_err(|_| Refusal::internal("The Config Store answered in an unknown shape"))?;
        C::written(written).map_err(|error| from_store(error, &self.call, Writes::Committed).into())
    }

    /// Refuse a call only Cloud can run before it reads anything.
    pub(crate) fn cloud_only(&self) -> Result<(), Stop> {
        match self.is_local() {
            true => Err(Refusal::not_here(self.local_next.clone()).into()),
            false => Ok(()),
        }
    }

    /// The call's one write: an operation Cloud runs on Servers, keyed so a
    /// retry with the same key returns the same run. A refusal names `subject`,
    /// the read that shows what happened.
    pub(crate) fn act(
        &mut self,
        action: Action,
        given: Option<RequestKey>,
        subject: Call,
    ) -> Result<Operation, Stop> {
        self.cloud_only()?;
        let Some(slot) = self.slot.take() else {
            return Err(Refusal::internal("A call writes at most once").into());
        };
        self.steps += 1;
        let Some(answers) = self.recorded(Kind::Write)? else {
            let key = self.request_key(given);
            let act = Act {
                attempt: AttemptId::derive(&self.call.tool, &key),
                digest: BodyDigest::of(&self.call.tool, &self.call.input),
                action,
            };
            return self.next_step(slot.spend(Write::Act(act)), Kind::Write);
        };
        let answer = answers
            .into_iter()
            .next()
            .ok_or_else(|| Refusal::internal("A write came back without an answer"))?;
        let value = answer.map_err(|error| refused_act(error, &self.call, subject))?;
        serde_json::from_value(value)
            .map_err(|_| Refusal::internal("Ployz Cloud answered in an unknown shape").into())
    }

    /// Wait the next backoff (1, 2, 4, then 10 s) within `wait_seconds`, then read
    /// `then`; `None` once the time is spent. A wait never decides an outcome.
    pub(crate) fn poll(
        &mut self,
        wait_seconds: u32,
        attempt: u32,
        then: Vec<Ask>,
    ) -> Result<Option<Vec<Result<Value, RpcError>>>, Stop> {
        let cap = wait_seconds.min(WAIT_CAP_SECONDS).saturating_mul(1000);
        let left = cap.saturating_sub(self.waited_ms);
        let backoff = match attempt {
            0 => 1_000,
            1 => 2_000,
            2 => 4_000,
            _ => 10_000,
        };
        let ms = backoff.min(left);
        if ms == 0 {
            return Ok(None);
        }
        self.waited_ms += ms;
        let kind = Kind::Wait {
            ms,
            count: then.len(),
        };
        match self.recorded(kind)? {
            Some(answers) => Ok(Some(answers)),
            None => self.next_step(Step::Wait { ms, then }, kind),
        }
    }

    /// The call's one write, refused as stale when the Store finds the
    /// Environment moved past `expect`, and any other refusal read by `refuse`.
    pub(crate) fn write_expecting<C: Tell>(
        &mut self,
        command: &C,
        expect: Option<u64>,
        refuse: impl FnOnce(RpcError, &Call, Writes) -> Refusal,
    ) -> Result<C::Written, Stop> {
        self.write_with(command, |error, call, writes| {
            let moved =
                error.code == RpcErrorCode::Conflict && error.details.get("revision").is_some();
            match expect.is_some() && moved {
                true => stale_refusal(call),
                false => refuse(error, call, writes),
            }
        })
    }

    /// The call's one write: `commands` applied together, only while the
    /// Environment is still at `expect`.
    pub(crate) fn batch(
        &mut self,
        environment: &EnvironmentRef,
        expect: Option<u64>,
        commands: Vec<BatchCommand>,
    ) -> Result<Vec<Written>, Stop> {
        let batch = Batch {
            environment: environment.clone(),
            commands,
            expect: expect.map(Revision),
        };
        self.write_expecting(&batch, expect, from_store)
            .map(|batched| batched.results)
    }

    /// An ID for what this call creates, the same on every retry of the call.
    pub(crate) fn mint<I: TryFrom<String>>(&self, given: Option<RequestKey>) -> Result<I, Stop> {
        let key = self.request_key(given);
        I::try_from(AttemptId::derive(&self.call.tool, &key).to_string())
            .map_err(|_| Refusal::internal("An ID did not mint").into())
    }

    /// Refuse before writing when the Environment moved past `expect`. For a
    /// command no batch takes: another write can still land between the read
    /// and this call's write.
    pub(crate) fn expect(
        &mut self,
        scope: &EnvironmentRef,
        expect: Option<u64>,
    ) -> Result<(), Stop> {
        let Some(expect) = expect else {
            return Ok(());
        };
        let view = self.one(&ServicesQuery {
            environment: scope.clone(),
        })?;
        match view.environment.revision == Revision(expect) {
            true => Ok(()),
            false => Err(stale(&self.call)),
        }
    }

    fn recorded(&mut self, kind: Kind) -> Result<Option<Vec<Result<Value, RpcError>>>, Stop> {
        if usize::from(self.steps) > usize::from(STEP_BUDGET) {
            return Err(Refusal::internal("A call takes at most three reads and writes").into());
        }
        let Some(round) = self.rounds.get(self.cursor) else {
            return Ok(None);
        };
        self.cursor += 1;
        if round.kind != kind {
            return Err(Refusal::internal("A resumed call took another path").into());
        }
        Ok(Some(round.answers.clone()))
    }

    fn next_step<T>(&self, step: Step, kind: Kind) -> Result<T, Stop> {
        Err(Stop::Step(Box::new(step), kind))
    }
}

/// A Store query as an ask.
pub(crate) fn store<Q: crate::Ask>(query: &Q) -> Result<Ask, Stop> {
    serde_json::from_value::<Query>(query.to_query())
        .map(Ask::Store)
        .map_err(|_| Refusal::internal("A query did not encode").into())
}

/// A refused act: what it wrote, the run it met, and the read that shows the
/// subject now.
fn refused_act(error: RpcError, call: &Call, subject: Call) -> Stop {
    let writes = writes_of(&error);
    let kept = ["run", "move_to"].map(|key| (key, error.details.get(key).cloned()));
    let fix = error
        .details
        .get("fix")
        .and_then(|fix| serde_json::from_value::<Fix>(fix.clone()).ok());
    let mut refusal = from_store(error, call, writes).with_next(subject);
    for (key, value) in kept {
        if let Some(value) = value {
            refusal = refusal.with_detail(key, value);
        }
    }
    if let Some(fix) = fix {
        refusal = refusal.with_fix(&fix);
    }
    refusal.into()
}

/// What a refused write says it wrote.
fn writes_of(error: &RpcError) -> Writes {
    match error.details.get("writes").and_then(Value::as_str) {
        Some("unknown") => Writes::Unknown,
        Some("committed") => Writes::Committed,
        _ => Writes::None,
    }
}

/// `conflict` for a write whose `expect_revision` no longer holds: read again.
pub(crate) fn stale(call: &Call) -> Stop {
    stale_refusal(call).into()
}

fn stale_refusal(call: &Call) -> Refusal {
    let mut next = Map::new();
    for key in ["project", "environment", "service"] {
        if let Some(value) = call.input.get(key) {
            next.insert(key.into(), value.clone());
        }
    }
    let tool = match next.contains_key("service") {
        true => "service_inspect",
        false => "environment_show",
    };
    Refusal::conflict("The Environment changed since you read it; read it again before writing")
        .with_next(Call::new(tool, next.into()))
}

/// The one result a batch wrote that `pick` takes, the last when several do.
pub(crate) fn written<T>(
    results: Vec<Written>,
    pick: impl FnMut(Written) -> Option<T>,
) -> Result<T, Stop> {
    results
        .into_iter()
        .rev()
        .find_map(pick)
        .ok_or_else(|| Refusal::internal("The Config Store answered in an unknown shape").into())
}

/// Where a result ran: `project/environment`.
pub(crate) fn at(environment: &EnvironmentSummary) -> String {
    format!("{}/{}", environment.project, environment.name)
}

/// One registered tool, as plain functions so the table is `const`.
pub(crate) struct Entry {
    pub(crate) name: &'static str,
    pub(crate) def: fn() -> ToolDef,
    pub(crate) example: fn() -> Value,
    run: fn(&mut Cx<'_>, &Value) -> Result<Value, Stop>,
    local_next: Option<&'static str>,
}

/// `T`'s table entry.
pub(crate) const fn entry<T: Tool>() -> Entry {
    Entry {
        name: T::NAME,
        def: def::<T>,
        example: T::example,
        run: run::<T>,
        local_next: T::LOCAL_NEXT,
    }
}

fn def<T: Tool>() -> ToolDef {
    ToolDef {
        name: T::NAME,
        description: format!("{} {}", T::DESCRIPTION, T::GATE.sentence()),
        input_schema: schema::input::<T>(),
        output_schema: json!({ "type": "object" }),
        read_only: T::READ_ONLY,
        gate: T::GATE,
    }
}

fn run<T: Tool>(cx: &mut Cx<'_>, input: &Value) -> Result<Value, Stop> {
    let input = decode::<T>(input).map_err(|error| {
        let field = error.path().unwrap_or_default();
        Refusal::invalid(
            format!("This input does not fit: {}", error.reason()),
            field,
        )
    })?;
    input.plan(cx)
}

/// Start `tool` on `input` at `place`. `call_key` is the transport's ID for this
/// call, stable across its retries.
#[must_use]
pub fn start(tool: &str, input: Value, place: &Place, call_key: &str) -> (Step, State) {
    let state = State {
        tool: tool.to_owned(),
        input,
        place: place.clone(),
        call_key: call_key.to_owned(),
        rounds: Vec::new(),
        pending: None,
    };
    advance(state)
}

/// Resume a call with the answers to the step it last returned, in order.
#[must_use]
pub fn resume(mut state: State, answers: Vec<Result<Value, RpcError>>) -> (Step, State) {
    let Some(kind) = state.pending.take() else {
        return done(
            state,
            Refusal::internal("This call is not waiting for answers"),
        );
    };
    if answers.len() != kind.answers() {
        return done(
            state,
            Refusal::internal("The answers do not match the step that asked for them"),
        );
    }
    state.rounds.push(Round { kind, answers });
    advance(state)
}

fn advance(mut state: State) -> (Step, State) {
    let tools = super::TOOLS;
    let Some(entry) = tools.iter().find(|entry| entry.name == state.tool) else {
        let known = tools.iter().map(|entry| entry.name);
        let refusal = Refusal::not_found("There is no tool by that name")
            .with_did_you_mean(crate::error::did_you_mean(&state.tool, known.clone()))
            .with_valid(known);
        return done(state, refusal);
    };
    let mut cx = Cx {
        rounds: &state.rounds,
        cursor: 0,
        steps: 0,
        waited_ms: 0,
        slot: Some(WriteSlot::new()),
        place: &state.place,
        call: Call::new(&state.tool, state.input.clone()),
        call_key: &state.call_key,
        local_next: entry.local_next.map(|tool| local_next(tool, &state.input)),
    };
    let outcome = (entry.run)(&mut cx, &state.input);
    let call = cx.call;
    match outcome {
        Ok(value) => (
            Step::Done {
                result: ToolResult::Ok(value),
            },
            state,
        ),
        Err(Stop::Step(step, kind)) => {
            state.pending = Some(kind);
            (*step, state)
        }
        Err(Stop::Refuse(refusal)) => {
            let refusal = finish(*refusal, entry, &call, &state.call_key);
            (
                Step::Done {
                    result: ToolResult::Refused(Box::new(refusal)),
                },
                state,
            )
        }
    }
}

/// A local call's next: the Store read, in the same place.
fn local_next(tool: &str, input: &Value) -> Call {
    let mut next = Map::new();
    for key in ["project", "environment"] {
        if let Some(value) = input.get(key) {
            next.insert(key.into(), value.clone());
        }
    }
    Call::new(tool, next.into())
}

fn done(state: State, refusal: Refusal) -> (Step, State) {
    let call = Call::new(&state.tool, state.input.clone());
    let refusal = match refusal.retry.is_same() {
        true => refusal.with_retry(call),
        false => refusal,
    };
    (
        Step::Done {
            result: ToolResult::Refused(Box::new(refusal)),
        },
        state,
    )
}

/// Fill what every refusal carries: a complete retry, and for a bad argument
/// what fits there and an example.
fn finish(mut refusal: Refusal, entry: &Entry, call: &Call, call_key: &str) -> Refusal {
    if refusal.retry.is_same() {
        refusal.retry = call.clone();
    }
    let input_schema = (entry.def)().input_schema;
    let keyed = input_schema.pointer("/properties/request_key").is_some();
    if refusal.writes == Writes::Unknown && keyed {
        // The same key lands the write once, so the retry repeats it.
        let given = call
            .input
            .get("request_key")
            .and_then(Value::as_str)
            .and_then(|key| RequestKey::parse(key).ok());
        if let Value::Object(input) = &mut refusal.retry.input {
            input.insert(
                "request_key".into(),
                RequestKey::effective(given, call_key).as_str().into(),
            );
        }
    }
    if refusal.code == RpcErrorCode::InvalidArgument {
        let field = refusal.field.get_or_insert_with(String::new).clone();
        if refusal.expected.is_none() {
            refusal.expected = Some(schema::at(&input_schema, &field));
        }
        if refusal.example.is_none() {
            refusal.example = Some((entry.example)());
        }
    }
    refusal
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;
