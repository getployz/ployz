//! What a tool answers: a value, or a refusal that says what to call instead.
//! Store errors are mapped here, so no CLI text or echoed value reaches an agent.

use ployz_core::{RpcError, RpcErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use ts_rs::TS;

use super::step::{DashboardPage, Fix};

/// A tool's answer.
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

    /// Stands for the refused call itself until the machine fills it in.
    fn same() -> Self {
        Self::new("", json!({}))
    }

    pub(crate) const fn is_same(&self) -> bool {
        self.tool.is_empty()
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
    pub retry: Call,
    pub writes: Writes,
    #[serde(default)]
    pub next: Vec<Call>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub details: Value,
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

    /// `details.<key>` set to `value`.
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
    let code = error.code.clone();
    let details = error.details;
    let mut refusal = Refusal::new(code.clone(), sanitize(&error.message, &code));
    refusal.writes = writes;
    refusal.did_you_mean = details
        .get("did_you_mean")
        .and_then(Value::as_str)
        .map(str::to_owned);
    refusal.valid = details
        .get("valid_children")
        .and_then(Value::as_array)
        .map(|valid| {
            valid
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if let Some(next) = details.get("next") {
        for next in next
            .as_array()
            .map_or_else(|| vec![next], |all| all.iter().collect())
        {
            match store_next(next) {
                Next::Call(call) => refusal.next.push(call),
                Next::Fix(fix) => refusal = refusal.with_fix(&fix),
                Next::Drop => {}
            }
        }
    }
    if code == RpcErrorCode::ConfirmationRequired {
        // Only a confirmation's retry may accept loss: the one the Store named.
        let mut input = call.input.clone();
        if let Value::Object(fields) = &mut input {
            fields.insert(
                "accept_volume_loss".into(),
                details.get("accept").cloned().unwrap_or_else(|| json!([])),
            );
            fields.insert(
                "version".into(),
                details.get("version").cloned().unwrap_or(Value::Null),
            );
        }
        refusal.retry = Call::new(&call.tool, input);
    }
    refusal
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

enum Next {
    Call(Call),
    Fix(Fix),
    Drop,
}

/// One Store `next`: a structured call, or a CLI line through [`CLI_NEXT`].
fn store_next(next: &Value) -> Next {
    if let Some(line) = next.as_str() {
        return cli_next(line);
    }
    let Some(tool) = next.get("tool").and_then(Value::as_str) else {
        return Next::Drop;
    };
    let input = next.get("input").cloned().unwrap_or_else(|| json!({}));
    match tool {
        "environment_show" => Next::Call(Call::new(tool, input)),
        "project_show" => Next::Call(Call::new(
            "environment_show",
            json!({ "project": input.get("project") }),
        )),
        // A removal is never proposed to a call that did not ask for one.
        _ => Next::Drop,
    }
}

/// What a CLI `next` line becomes.
#[derive(Clone, Copy, Debug)]
enum CliNext {
    Fix(FixRow),
    /// The tool, and the input key the first argument fills.
    Tool(&'static str, Option<&'static str>),
    VolumeSharedWrites,
}

/// `Fix` without data, so the table stays `const`.
#[derive(Clone, Copy, Debug)]
enum FixRow {
    ConnectServers,
    Open(DashboardPage),
}

/// Every CLI command a Store `next` names, by its words, and what an agent calls
/// instead. A test scans the Store's sources so a new one cannot slip through.
const CLI_NEXT: &[(&str, CliNext)] = &[
    ("server add", CliNext::Fix(FixRow::ConnectServers)),
    (
        "server clean",
        CliNext::Fix(FixRow::Open(DashboardPage::Servers)),
    ),
    ("set", CliNext::Fix(FixRow::Open(DashboardPage::Variables))),
    (
        "project new",
        CliNext::Fix(FixRow::Open(DashboardPage::Projects)),
    ),
    (
        "env new",
        CliNext::Fix(FixRow::Open(DashboardPage::Environments)),
    ),
    (
        "env branch",
        CliNext::Fix(FixRow::Open(DashboardPage::Environments)),
    ),
    (
        "env sync",
        CliNext::Fix(FixRow::Open(DashboardPage::Environments)),
    ),
    (
        "env setup",
        CliNext::Fix(FixRow::Open(DashboardPage::Environments)),
    ),
    (
        "env pr",
        CliNext::Tool("environment_pr", Some("repository")),
    ),
    ("github ls", CliNext::Tool("github_ls", Some("repository"))),
    ("github connect", CliNext::Tool("github_connect", None)),
    ("deployment ls", CliNext::Tool("deployment_show", None)),
    ("diff", CliNext::Tool("review", None)),
    ("domain add", CliNext::Tool("domain_add", Some("service"))),
    ("volume inspect", CliNext::Tool("environment_show", None)),
    ("volume set", CliNext::VolumeSharedWrites),
];

/// The table row for a CLI line's positional words, and how many words its
/// command takes.
fn cli_row(words: &[&str]) -> Option<(CliNext, usize)> {
    CLI_NEXT.iter().find_map(|(command, row)| {
        let width = command.split(' ').count();
        (words.get(..width)?.join(" ") == *command).then_some((*row, width))
    })
}

fn cli_next(line: &str) -> Next {
    let mut words = line.split_whitespace().skip_while(|word| *word == "ployz");
    let mut positional = Vec::new();
    let mut input = Map::new();
    while let Some(word) = words.next() {
        match word.strip_prefix("--") {
            Some(flag @ ("project" | "env")) => {
                if let Some(value) = words.next() {
                    let key = if flag == "env" {
                        "environment"
                    } else {
                        "project"
                    };
                    input.insert(key.into(), value.into());
                }
            }
            Some(_) => {}
            None => positional.push(word),
        }
    }
    let Some((row, width)) = cli_row(&positional) else {
        return Next::Drop;
    };
    // A placeholder such as NAME is for a human to fill, never an argument.
    let argument = positional
        .iter()
        .skip(width)
        .find(|word| !word.chars().all(|c| c.is_ascii_uppercase() || c == '_'))
        .copied();
    match row {
        CliNext::Fix(FixRow::ConnectServers) => Next::Fix(Fix::ConnectServers),
        CliNext::Fix(FixRow::Open(page)) => Next::Fix(Fix::OpenDashboard { page }),
        CliNext::Tool(tool, key) => {
            if let (Some(key), Some(argument)) = (key, argument) {
                input.insert(key.into(), argument.into());
            }
            if tool == "environment_pr" {
                input.insert("enabled".into(), true.into());
            }
            if matches!(tool, "github_ls" | "github_connect") {
                input.remove("project");
                input.remove("environment");
            }
            Next::Call(Call::new(tool, input.into()))
        }
        CliNext::VolumeSharedWrites => {
            if let Some(volume) = argument {
                input.insert("volume".into(), volume.into());
            }
            input.insert("shared_writes".into(), true.into());
            Next::Call(Call::new("volume_update", input.into()))
        }
    }
}

/// Settings agents reach for that are not Settings, and the tool that owns each.
pub const NOT_SETTINGS: &[(&str, &str)] = &[
    ("managedHostnames", "domain_add"),
    ("customHostnames", "domain_add"),
    ("domains", "domain_add"),
    ("hostnames", "domain_add"),
    ("volumes", "volume_add"),
];

/// The tool that owns `key` when it is no Setting.
pub(crate) fn not_setting(key: &str) -> Option<&'static str> {
    NOT_SETTINGS
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, tool)| *tool)
}

/// The sentence for a key that lives in another tool.
pub(crate) fn not_setting_sentence(tool: &str) -> String {
    match tool {
        "domain_add" => "Domains are not Service Settings; add and change them with domain_add and domain_update".to_owned(),
        _ => format!("That is not a Service Setting; use {tool}"),
    }
}

#[cfg(test)]
#[path = "refusal_tests.rs"]
mod tests;
