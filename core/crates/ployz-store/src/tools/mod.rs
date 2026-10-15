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

use machine::{Entry, Tool, entry};

macro_rules! tools {
    ($($module:ident::$tool:ident),* $(,)?) => {
        /// Every tool, in catalog order.
        pub(crate) const TOOLS: &[Entry] = &[$(entry::<$module::$tool>(),)*];

        /// One tool, named for its type.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum ToolId {
            $($tool,)*
        }

        impl ToolId {
            /// Every tool, in catalog order.
            pub const ALL: &[Self] = &[$(Self::$tool,)*];

            /// The name a model calls it by.
            #[must_use]
            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$tool => <$module::$tool as Tool>::NAME,)*
                }
            }

            /// Whether it only reads.
            #[must_use]
            pub fn read_only(self) -> bool {
                match self {
                    $(Self::$tool => <$module::$tool as Tool>::READ_ONLY,)*
                }
            }
        }
    };
}

tools! {
    docs::Docs,
    environment::EnvironmentShow,
    project::ProjectCreate,
    project::ProjectRename,
    service::ServiceInspect,
    service::ServiceAdd,
    service::ServiceUpdate,
    service::ServiceRename,
    service::ServiceRemove,
    domain::DomainCheck,
    domain::DomainAdd,
    domain::DomainUpdate,
    domain::DomainRemove,
    volume::VolumeAdd,
    volume::VolumeUpdate,
    volume::VolumeRemove,
    config::ConfigWrite,
    config::ConfigRename,
    config::ConfigRemove,
    release::Review,
    release::DiscardInput,
    history::History,
    release::DeployInput,
    deployment::DeploymentList,
    deployment::DeploymentShow,
    deployment::DeploymentRetry,
    deployment::DeploymentStart,
    deployment::DeploymentCancel,
    environment::EnvironmentPr,
    environment::EnvironmentSetup,
    github::GithubLs,
    github::GithubConnect,
    server::ServerShow,
    server::ServerLogs,
    metrics::ServerMetrics,
    runtime::ServiceState,
    runtime::ServiceLogs,
    metrics::ServiceMetrics,
    copies::VolumeShow,
    runtime::ServiceRestart,
    runtime::ServiceStart,
    runtime::ServiceStop,
    server::ServerSet,
    server::ServerDrain,
    server::ServerUpgrade,
    server::ServerClean,
    copies::VolumeMirror,
    copies::VolumeMirrorRemove,
    copies::VolumeMove,
    copies::VolumeRelease,
    copies::VolumeRestore,
    removal::EnvironmentRemove,
    removal::ProjectRemove,
    server::ServerRemove,
    server::ServerForget,
}

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
pub const INSTRUCTIONS: &str = "Ployz runs Projects with Environments of Services on your servers. Names, not ids. When the member's words could name more than one kind of thing (a repository or an image, a Service or a Project), read to settle which before writing. Every result says where it ran as at. A refusal has code, message, field, expected, example, retry (a complete call), writes (none, committed or unknown) and next (the calls to make now). When writes is unknown, make the next call before anything else. Most writes are Staged: review shows them, discard undoes them, deploy ships them. Not here: branches, tokens, servers added, exec, live logs, up; the member does those in the terminal. project and environment default to the place you are in. request_key: any 8 to 64 letters, digits, - or _; send the same key again after a lost reply and you get the same run, never a second one. wait_seconds: wait up to that long (max 300) for the outcome; 0 returns once accepted. accept_volume_loss, every_copy and version come only from a refusal, for exactly the loss the member accepted. Reads say when they were seen; a server that did not answer is listed, never guessed. Logs never show a secret; do not ask a tool for one. Servers, their logs and metrics need Ployz Cloud. \
For how Ployz works or how to do something, read docs before answering from memory.";
