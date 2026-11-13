//! What a tool answers: a value, or a refusal that says what to call instead.
//! Store errors are mapped here, so no CLI text or echoed value reaches an agent.

use ployz_core::{RpcError, RpcErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use ts_rs::TS;

use super::ToolId;
use super::step::{DashboardPage, Fix};
use crate::error::Confirmation;
use crate::followup::{FollowUp, Place};

/// A tool's answer: a value, or a [`Refusal`] that says what to call instead.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(into = "Wire", try_from = "Wire")]
#[ts(type = "{ ok: true, value: JsonValue } | { ok: false, refusal: Refusal }")]
pub enum ToolResult {
    Ok(Value),
    Refused(Box<Refusal>),
}

#[derive(Serialize, Deserialize)]
struct Wire {
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refusal: Option<Box<Refusal>>,
}

impl From<ToolResult> for Wire {
    fn from(result: ToolResult) -> Self {
        match result {
            ToolResult::Ok(value) => Self {
                ok: true,
                value: Some(value),
                refusal: None,
            },
            ToolResult::Refused(refusal) => Self {
                ok: false,
                value: None,
                refusal: Some(refusal),
            },
        }
    }
}

impl TryFrom<Wire> for ToolResult {
    type Error = &'static str;

    fn try_from(wire: Wire) -> Result<Self, &'static str> {
        match (wire.ok, wire.value, wire.refusal) {
            (true, Some(value), None) => Ok(Self::Ok(value)),
            (false, None, Some(refusal)) => Ok(Self::Refused(refusal)),
            _ => Err("a result holds a value when ok and a refusal otherwise"),
        }
    }
}

/// A tool and its input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Call {
    pub tool: String,
    pub input: Value,
}

impl Call {
    #[must_use]
    pub fn new(tool: &str, input: Value) -> Self {
        Self {
            tool: tool.to_owned(),
            input,
        }
    }

    #[must_use]
    pub fn to(tool: ToolId, input: Value) -> Self {
        Self::new(tool.name(), input)
    }

    fn same() -> Self {
        Self::new("", json!({}))
    }

    pub(crate) const fn is_same(&self) -> bool {
        self.tool.is_empty()
    }

    /// Whether a refusal leaves this retry out: none was set, or its input holds a
    /// secret's text, which no refusal hands back (#3346), whatever it refused.
    fn withheld(&self) -> bool {
        self.is_same() || crate::variables::holds_secret_text(&self.input)
    }
}

/// Whether a refused call wrote anything.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Writes {
    None,
    Committed,
    Unknown,
}

/// Why a call did not answer, and what to call instead. The message names
/// things but never echoes a value the caller sent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Refusal {
    pub code: RpcErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub did_you_mean: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub valid: Vec<String>,
    #[serde(default = "Call::same", skip_serializing_if = "Call::withheld")]
    #[ts(as = "Option<Call>", optional)]
    pub retry: Call,
    pub writes: Writes,
    #[serde(default)]
    pub next: Vec<Call>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub details: Value,
    /// The call sent text the Store keeps sealed, so no retry repeats it. Text under
    /// `secret` never reaches a retry anyway; see [`Call::withheld`].
    #[serde(skip)]
    #[ts(skip)]
    pub secret_sent: bool,
}

impl Refusal {
    pub(crate) fn new(code: RpcErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            field: None,
            expected: None,
            example: None,
            did_you_mean: None,
            valid: Vec::new(),
            retry: Call::same(),
            writes: Writes::None,
            next: Vec::new(),
            details: Value::Null,
            secret_sent: false,
        }
    }

    /// `invalid_argument` at `field`; the machine adds what fits there.
    pub(crate) fn invalid(message: impl Into<String>, field: &str) -> Self {
        Self {
            field: Some(field.to_owned()),
            ..Self::new(RpcErrorCode::InvalidArgument, message)
        }
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::new(RpcErrorCode::NotFound, message)
    }

    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::new(RpcErrorCode::Conflict, message)
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(RpcErrorCode::Internal, message)
    }

    /// Asked of Cloud on a local Store: nothing here can answer it.
    pub(crate) fn not_here(next: Option<Call>) -> Self {
        Self {
            next: next.into_iter().collect(),
            ..Self::new(
                RpcErrorCode::from("not_here"),
                "This needs Ployz Cloud; a local Config Store cannot answer it",
            )
            .with_fix(&Fix::UseCloud)
        }
    }

    pub(crate) fn with_next(mut self, next: Call) -> Self {
        self.next.push(next);
        self
    }

    pub(crate) fn with_valid<'a>(mut self, valid: impl IntoIterator<Item = &'a str>) -> Self {
        self.valid = valid.into_iter().map(str::to_owned).collect();
        self
    }

    pub(crate) fn with_did_you_mean(mut self, closest: Option<&str>) -> Self {
        self.did_you_mean = closest.map(str::to_owned);
        self
    }

    pub(crate) fn with_retry(mut self, retry: Call) -> Self {
        self.retry = retry;
        self
    }

    pub(crate) fn with_fix(self, fix: &Fix) -> Self {
        self.with_detail("fix", json!(fix))
    }

    pub(crate) fn with_detail(mut self, key: &str, value: Value) -> Self {
        let mut details = match self.details {
            Value::Object(details) => details,
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_) => Map::new(),
        };
        details.insert(key.into(), value);
        self.details = details.into();
        self
    }

    /// Whether the same call can succeed later: not after a claim, when the
    /// agent must first pick one of `valid`, nor when what it asks already holds,
    /// nor when it sent a secret's value.
    pub(crate) fn retries(&self) -> bool {
        !self.secret_sent
            && !matches!(self.fix(), Some(Fix::Claim { .. }))
            && self.code != RpcErrorCode::Ambiguous
            && self.details.get("settled") != Some(&Value::Bool(true))
    }

    /// The structured fix, if a human must act.
    #[must_use]
    pub fn fix(&self) -> Option<Fix> {
        self.details
            .get("fix")
            .and_then(|fix| serde_json::from_value(fix.clone()).ok())
    }
}

/// A Store error for `call` as a refusal, `writes` saying what the write did.
pub(crate) fn from_store(error: RpcError, call: &Call, writes: Writes) -> Refusal {
    from_store_with(error, call, writes, Confirmation::retry)
}

/// A Store error for `call` as a refusal whose confirmation, if it is one, is
/// retried as `retry` builds it: each tool takes the Volumes to accept its own way.
pub(crate) fn from_store_with(
    error: RpcError,
    call: &Call,
    writes: Writes,
    retry: impl FnOnce(&Confirmation, &Call) -> Call,
) -> Refusal {
    let code = error.code.clone();
    let details = error.details;
    let mut refusal = Refusal::new(code.clone(), sanitize(&error.message, &code));
    refusal.writes = writes;
    refusal.did_you_mean = details
        .get("did_you_mean")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let projects = details.get("projects");
    if projects.is_some() {
        refusal.field = Some("project".into());
    }
    refusal.valid = projects
        .or_else(|| details.get("valid_children"))
        .and_then(Value::as_array)
        .map(|valid| {
            valid
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    match FollowUp::of(&details).map(told) {
        Some(Told::Call(call)) => refusal.next.push(call),
        Some(Told::Fix(fix)) => refusal = refusal.with_fix(&fix),
        Some(Told::Nothing) | None => {}
    }
    if let Some(url) = details.get("claim_url").and_then(Value::as_str) {
        refusal = refusal.with_fix(&Fix::Claim { url: url.into() });
    }
    if details.get("settled") == Some(&Value::Bool(true)) {
        refusal = refusal.with_detail("settled", Value::Bool(true));
    }
    if code == RpcErrorCode::ConfirmationRequired {
        refusal.retry = retry(&Confirmation::of(&details), call);
    }
    refusal
}

impl Confirmation {
    /// The call again accepting the Volumes this names, at the version it binds.
    /// Only a confirmation's retry may accept loss: the one the Store named.
    pub(crate) fn retry(&self, call: &Call) -> Call {
        self.retry_accepting(call, json!(self.accept.clone().unwrap_or_default()))
    }

    /// The call again with `accept` as the Volumes to accept, at the version this
    /// binds.
    pub(crate) fn retry_accepting(&self, call: &Call, accept: Value) -> Call {
        let mut input = call.input.clone();
        if let Value::Object(fields) = &mut input {
            fields.insert("accept_volume_loss".into(), accept);
            fields.insert("version".into(), json!(self.version));
        }
        Call::new(&call.tool, input)
    }
}

/// A Store message up to any CLI clause it carries: the next tool says it instead.
fn sanitize(message: &str, code: &RpcErrorCode) -> String {
    let cut = ["ployz ", "--"]
        .iter()
        .filter_map(|marker| message.find(marker))
        .min();
    let Some(cut) = cut else {
        return message.to_owned();
    };
    let head = message.get(..cut).unwrap_or_default();
    let head = head
        .rfind([':', ';', '('])
        .and_then(|at| head.get(..at))
        .unwrap_or(head)
        .trim_end_matches([' ', ',', '.', ':', ';', '(', '`', '\'', '"']);
    match head.is_empty() {
        true => format!("The Config Store refused this call ({code})"),
        false => head.to_owned(),
    }
}

/// What an agent is told of a [`FollowUp`].
enum Told {
    Call(Call),
    Fix(Fix),
    /// No tool call does what the follow-up does, or it is a person's to run.
    Nothing,
}

fn told(follow_up: FollowUp) -> Told {
    let open = |page| Told::Fix(Fix::OpenDashboard { page });
    let place =
        |place: &Place| json!({ "project": place.project, "environment": place.environment });
    match follow_up {
        FollowUp::ServerAdd => Told::Fix(Fix::ConnectServers),
        FollowUp::ServerClean { .. } => open(DashboardPage::Servers),
        FollowUp::SetRegistryCredential { .. }
        | FollowUp::SetStopFirst { .. }
        | FollowUp::RenameVariable { .. }
        | FollowUp::PatchHealthcheck { .. } => open(DashboardPage::Variables),
        FollowUp::EnvironmentNew { .. }
        | FollowUp::EnvironmentBranch { .. }
        | FollowUp::EnvironmentSync { .. }
        | FollowUp::EnvironmentSetup { .. }
        // Unprotecting is a person's call, made where they can see what it guards.
        | FollowUp::UnprotectEnvironment { .. } => open(DashboardPage::Environments),
        FollowUp::ProjectNew => Told::Call(Call::to(ToolId::ProjectCreate, json!({}))),
        FollowUp::ProjectList => Told::Call(Call::to(ToolId::EnvironmentShow, json!({}))),
        FollowUp::EnvironmentList { project } | FollowUp::ShowProject { project } => Told::Call(
            Call::to(ToolId::EnvironmentShow, json!({ "project": project })),
        ),
        FollowUp::ShowEnvironment { place: at } | FollowUp::VolumeInspect { place: at, .. } => {
            Told::Call(Call::to(ToolId::EnvironmentShow, place(&at)))
        }
        FollowUp::EnvironmentPr {
            repository,
            project,
        } => Told::Call(Call::to(
            ToolId::EnvironmentPr,
            json!({ "project": project, "repository": repository, "enabled": true }),
        )),
        FollowUp::GithubList { repository } => Told::Call(Call::to(
            ToolId::GithubLs,
            json!({ "repository": repository }),
        )),
        FollowUp::GithubConnect => Told::Call(Call::to(ToolId::GithubConnect, json!({}))),
        FollowUp::Deploy { .. } => Told::Call(Call::to(ToolId::DeployInput, json!({}))),
        FollowUp::DeploymentList { place: at } => {
            Told::Call(Call::to(ToolId::DeploymentList, place(&at)))
        }
        FollowUp::Diff { place: at } => Told::Call(Call::to(ToolId::Review, place(&at))),
        FollowUp::DomainAdd { service } => Told::Call(Call::to(
            ToolId::DomainAdd,
            json!({ "service": service }),
        )),
        FollowUp::RenameSetupService { place: at } => Told::Call(Call::to(
            ToolId::ServiceRename,
            json!({
                "project": at.project,
                "environment": at.environment,
                "service": "setup",
            }),
        )),
        FollowUp::VolumeAllowSharedWrites { volume, place: at } => Told::Call(Call::to(
            ToolId::VolumeUpdate,
            json!({
                "project": at.project,
                "environment": at.environment,
                "volume": volume,
                "shared_writes": true,
            }),
        )),
        // A removal is never proposed to a call that did not ask for one.
        FollowUp::DomainRemove { .. } | FollowUp::Remove { .. } => Told::Nothing,
        // volume_add takes no shared writes.
        FollowUp::VolumeAdd { .. } => Told::Nothing,
        FollowUp::SyncThenSetSecret { .. }
        | FollowUp::StopKeepingBranch { .. }
        | FollowUp::UpdateClient { .. } => Told::Nothing,
    }
}

/// Settings agents reach for that are not Settings, and the tool that owns each.
pub const NOT_SETTINGS: &[(&str, ToolId)] = &[
    ("managedHostnames", ToolId::DomainAdd),
    ("customHostnames", ToolId::DomainAdd),
    ("domains", ToolId::DomainAdd),
    ("hostnames", ToolId::DomainAdd),
    ("volumes", ToolId::VolumeAdd),
];

/// The tool that owns `key` when it is no Setting.
pub(crate) fn not_setting(key: &str) -> Option<ToolId> {
    NOT_SETTINGS
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, tool)| *tool)
}

/// The sentence for a key that lives in another tool.
pub(crate) fn not_setting_sentence(tool: ToolId) -> String {
    match tool == ToolId::DomainAdd {
        true => "Domains are not Service Settings; add and change them with domain_add and domain_update".to_owned(),
        false => format!("That is not a Service Setting; use {}", tool.name()),
    }
}

#[cfg(test)]
#[path = "refusal_tests.rs"]
mod tests;
