//! The tools an agent runs Ployz through: a catalog, and a step machine that
//! plans each call as Store and Cloud reads and at most one write. No IO.

mod act;
pub mod cloud;
mod config;
mod copies;
mod deployment;
mod docs;
mod domain;
mod environment;
mod github;
mod history;
pub mod logs;
mod machine;
mod metrics;
mod project;
mod refusal;
mod release;
mod removal;
mod runtime;
mod schema;
mod server;
mod service;
pub mod status;
mod step;
mod volume;

pub use logs::{LineCap, LogExcerpt, LogText, SecretSet, Withheld};
pub use machine::{Place, STEP_BUDGET, State, StoreAt, ToolDef, WAIT_CAP_SECONDS, resume, start};
pub use refusal::{Call, NOT_SETTINGS, Refusal, ToolResult, Writes};
pub use schema::{lint, portable};
pub use status::{ServiceStatus, service_status};
pub use step::{
    AcceptedLoss, Act, Action, Answer, Ask, AttemptId, BodyDigest, CloudQuery, DashboardPage, Fix,
    Freshness, Gate, LogsQuery, MetricsQuery, MetricsSince, MetricsSubject, OperationsQuery,
    RequestKey, ServerLogsQuery, ServerName, ServerRef, ServiceVerb, Source, Step, Subject,
    Timestamp, VolumeRun, Write, WriteSlot,
};

use machine::{Entry, entry};

/// Every tool, in catalog order.
pub(crate) const TOOLS: &[Entry] = &[
    entry::<docs::Docs>(),
    entry::<environment::EnvironmentShow>(),
    entry::<project::ProjectCreate>(),
    entry::<project::ProjectRename>(),
    entry::<service::ServiceInspect>(),
    entry::<service::ServiceAdd>(),
    entry::<service::ServiceUpdate>(),
    entry::<service::ServiceRename>(),
    entry::<service::ServiceRemove>(),
    entry::<domain::DomainCheck>(),
    entry::<domain::DomainAdd>(),
    entry::<domain::DomainUpdate>(),
    entry::<domain::DomainRemove>(),
    entry::<volume::VolumeAdd>(),
    entry::<volume::VolumeUpdate>(),
    entry::<volume::VolumeRemove>(),
    entry::<config::ConfigWrite>(),
    entry::<config::ConfigRename>(),
    entry::<config::ConfigRemove>(),
    entry::<release::Review>(),
    entry::<release::DiscardInput>(),
    entry::<history::History>(),
    entry::<release::PublishInput>(),
    entry::<release::DeployInput>(),
    entry::<deployment::DeploymentList>(),
    entry::<deployment::DeploymentShow>(),
    entry::<deployment::DeploymentRetry>(),
    entry::<deployment::DeploymentStart>(),
    entry::<deployment::DeploymentCancel>(),
    entry::<environment::EnvironmentPr>(),
    entry::<environment::EnvironmentSetup>(),
    entry::<github::GithubLs>(),
    entry::<github::GithubConnect>(),
    entry::<server::ServerShow>(),
    entry::<server::ServerLogs>(),
    entry::<metrics::ServerMetrics>(),
    entry::<runtime::ServiceState>(),
    entry::<runtime::ServiceLogs>(),
    entry::<metrics::ServiceMetrics>(),
    entry::<copies::VolumeShow>(),
    entry::<runtime::ServiceRestart>(),
    entry::<runtime::ServiceStart>(),
    entry::<runtime::ServiceStop>(),
    entry::<server::ServerSet>(),
    entry::<server::ServerDrain>(),
    entry::<server::ServerUpgrade>(),
    entry::<server::ServerClean>(),
    entry::<copies::VolumeMirror>(),
    entry::<copies::VolumeMirrorRemove>(),
    entry::<copies::VolumeMove>(),
    entry::<copies::VolumeRelease>(),
    entry::<copies::VolumeRestore>(),
    entry::<removal::EnvironmentRemove>(),
    entry::<removal::ProjectRemove>(),
    entry::<server::ServerRemove>(),
    entry::<server::ServerForget>(),
];

/// What a model is told: every tool's name, description and schemas.
#[must_use]
pub fn catalog() -> Vec<ToolDef> {
    TOOLS.iter().map(|entry| (entry.def)()).collect()
}

/// A complete, decodable input for `tool`, if there is one by that name.
#[must_use]
pub fn example(tool: &str) -> Option<serde_json::Value> {
    TOOLS
        .iter()
        .find(|entry| entry.name == tool)
        .map(|entry| (entry.example)())
}

/// What a model reads once, before any tool.
pub const INSTRUCTIONS: &str = "Ployz runs Projects with Environments of Services on your servers. Names, not ids. Every result says where it ran as at. A refusal has code, message, field, expected, example, retry (a complete call), writes (none, committed or unknown) and next (the calls to make now). When writes is unknown, make the next call before anything else. Most writes are Staged: review shows them, discard undoes them, deploy ships them. Not here: branches, tokens, servers added, exec, live logs, up; the member does those in the terminal. project and environment default to the place you are in. request_key: any 8 to 64 letters, digits, - or _; send the same key again after a lost reply and you get the same run, never a second one. wait_seconds: wait up to that long (max 300) for the outcome; 0 returns once accepted. accept_volume_loss, every_copy and version come only from a refusal, for exactly the loss the member accepted. Reads say when they were seen; a server that did not answer is listed, never guessed. Logs never show a secret; do not ask a tool for one. Servers, their logs and metrics need Ployz Cloud. \
For how Ployz works or how to do something, read docs before answering from memory.";
