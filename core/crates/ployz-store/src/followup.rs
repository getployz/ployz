//! What a refused write tells its caller to do next.
//!
//! A Store function returns an [`RpcError`](ployz_core::RpcError), so a follow-up
//! travels in its details twice: as `next`, the command line a CLI user runs or the
//! `{tool, input}` object a dashboard acts on, and as `follow_up`, the typed form
//! that `tools::refusal` turns into a call or a fix for an agent. Both are written
//! from one [`FollowUp`], so the line and the agent's step cannot drift apart.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::scope::EnvironmentSummary;
use crate::{EnvironmentName, ProjectName};

const KEY: &str = "follow_up";
const NEXT: &str = "next";

/// An Environment, by the names a command line gives it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Place {
    pub(crate) project: ProjectName,
    pub(crate) environment: EnvironmentName,
}

impl Place {
    pub(crate) fn of(summary: &EnvironmentSummary) -> Self {
        Self {
            project: summary.project.clone(),
            environment: summary.name.clone(),
        }
    }

    /// `--env E --project P`.
    fn environment_first(&self) -> String {
        format!("--env {} --project {}", self.environment, self.project)
    }

    /// `--project P --env E`.
    fn project_first(&self) -> String {
        format!("--project {} --env {}", self.project, self.environment)
    }
}

/// Whether the Setup Commands left name one Service or several.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SetupLeft {
    One,
    Several,
}

/// How a Volume a command line adds is stored.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Storage {
    Docker,
    /// A size such as `10GiB`, written without spaces.
    Size(String),
}

/// The next step of a refused write: one variant per command a Store refusal names,
/// with what that command takes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum FollowUp {
    ServerAdd,
    ServerClean {
        namespace: String,
    },
    SetRegistryCredential {
        service: String,
    },
    SetStopFirst {
        service: String,
        place: Place,
    },
    RenameVariable {
        service: String,
        key: String,
        to: String,
        place: Place,
    },
    PatchHealthcheck {
        service: String,
        place: Place,
    },
    /// Sync without a value first, then set the secret where it lands.
    SyncThenSetSecret {
        label: String,
        place: Place,
    },
    ProjectNew,
    ProjectList,
    EnvironmentList {
        project: ProjectName,
    },
    EnvironmentNew {
        name: EnvironmentName,
        project: ProjectName,
    },
    EnvironmentBranch {
        from: EnvironmentName,
        project: ProjectName,
    },
    EnvironmentSync {
        place: Place,
    },
    EnvironmentSetup {
        place: Place,
        left: SetupLeft,
    },
    EnvironmentPr {
        repository: String,
        project: ProjectName,
    },
    GithubList {
        repository: String,
    },
    GithubConnect,
    Deploy {
        no_prune: bool,
    },
    DeploymentList {
        place: Place,
    },
    Diff {
        place: Place,
    },
    DomainAdd {
        service: String,
    },
    /// Remove each generated domain, by its prefix.
    DomainRemove {
        prefixes: Vec<String>,
        place: Place,
    },
    RenameSetupService {
        place: Place,
    },
    VolumeInspect {
        volume: String,
        place: Place,
    },
    VolumeAllowSharedWrites {
        volume: String,
        place: Place,
    },
    VolumeAdd {
        volume: String,
        /// `SERVICE:PATH` of each mount.
        mounts: Vec<String>,
        storage: Storage,
        place: Place,
    },
    /// Update the client. `beta` installs the highest published release on either
    /// channel: Publish only moves a pointer forward, and a stable Publish moves
    /// `beta` too (`core/scripts/promote-release.sh`), so it reaches whichever
    /// published Ployz upgraded the Store. The stable line reinstalls this same
    /// release when a beta did.
    UpdateClient {
        beta: bool,
    },
    ShowEnvironment {
        place: Place,
    },
    ShowProject {
        project: ProjectName,
    },
    UnprotectEnvironment {
        place: Place,
    },
    StopKeepingBranch {
        place: Place,
    },
    Remove {
        project: ProjectName,
        environment: Option<EnvironmentName>,
    },
}

/// What `details.next` holds.
enum Cli {
    Line(String),
    Tool(&'static str, Value),
}

impl FollowUp {
    /// `details.next`: a command line, or the `{tool, input}` object where the Store
    /// names a Store tool.
    pub(crate) fn cli(&self) -> Value {
        match self.render() {
            Cli::Line(line) => Value::String(line),
            Cli::Tool(tool, input) => json!({ "tool": tool, "input": input }),
        }
    }

    /// `fields` with this follow-up written beside them, as both readers expect it.
    pub(crate) fn within(&self, fields: Value) -> Value {
        let mut details = fields.as_object().cloned().unwrap_or_default();
        details.insert(NEXT.into(), self.cli());
        details.insert(KEY.into(), json!(self));
        Value::Object(details)
    }

    /// The details of a refusal that carries only this follow-up.
    pub(crate) fn details(&self) -> Value {
        self.within(Value::Null)
    }

    /// The typed form `details` carry, if they carry one.
    pub(crate) fn of(details: &Value) -> Option<Self> {
        serde_json::from_value(details.get(KEY)?.clone()).ok()
    }

    /// `details` without a follow-up: one that read what writes before it left, and
    /// the refusal rolled those back.
    pub(crate) fn remove_from(details: &mut Value) {
        if let Some(details) = details.as_object_mut() {
            details.remove(NEXT);
            details.remove(KEY);
        }
    }

    fn render(&self) -> Cli {
        let line = Cli::Line;
        match self {
            Self::ServerAdd => line("ployz server add".into()),
            Self::ServerClean { namespace } => line(format!(
                "ployz server clean --namespace {namespace} --confirm {namespace}"
            )),
            Self::SetRegistryCredential { service } => {
                line(format!("ployz set {service}.registryCredential --secret"))
            }
            Self::SetStopFirst { service, place } => line(format!(
                "ployz set {service}.updateOrder=stop-first {}",
                place.environment_first()
            )),
            Self::RenameVariable {
                service,
                key,
                to,
                place,
            } => line(format!(
                "ployz set {service}.env.{key} --rename {to} --update-references {}",
                place.environment_first()
            )),
            Self::PatchHealthcheck { service, place } => line(format!(
                "ployz set {service} --patch '{{\"healthcheck\":{{\"command\":\"pg_isready\"}}}}' {}",
                place.environment_first()
            )),
            Self::SyncThenSetSecret { label, place } => line(format!(
                "sync without --value first, then ployz set {label} --secret {}",
                place.environment_first()
            )),
            Self::ProjectNew => line("ployz project new NAME".into()),
            Self::ProjectList => line("ployz project ls".into()),
            Self::EnvironmentList { project } => line(format!("ployz env ls --project {project}")),
            Self::EnvironmentNew { name, project } => {
                line(format!("ployz env new {name} --project {project}"))
            }
            Self::EnvironmentBranch { from, project } => line(format!(
                "ployz env branch NAME --from {from} --project {project}"
            )),
            Self::EnvironmentSync { place } => line(format!(
                "ployz env sync --to ENV --project {} --env {}",
                place.project, place.environment
            )),
            Self::EnvironmentSetup {
                place,
                left: SetupLeft::One,
            } => line(format!(
                "ployz env setup --clear {}",
                place.environment_first()
            )),
            Self::EnvironmentSetup {
                place,
                left: SetupLeft::Several,
            } => line(format!(
                "ployz env setup --setup SERVICE=COMMAND {}",
                place.environment_first()
            )),
            Self::EnvironmentPr {
                repository,
                project,
            } => line(format!(
                "ployz env pr {repository} --from ENV --project {project}"
            )),
            Self::GithubList { repository } => line(format!("ployz github ls {repository}")),
            Self::GithubConnect => line("ployz github connect".into()),
            Self::Deploy { no_prune: false } => line("ployz deploy".into()),
            Self::Deploy { no_prune: true } => line("ployz deploy --no-prune".into()),
            Self::DeploymentList { place } => {
                line(format!("ployz deployment ls {}", place.project_first()))
            }
            Self::Diff { place } => line(format!("ployz diff {}", place.project_first())),
            Self::DomainAdd { service } => line(format!("ployz domain add {service}")),
            Self::DomainRemove { prefixes, place } => line(
                prefixes
                    .iter()
                    .map(|prefix| format!("ployz domain rm {prefix} {}", place.project_first()))
                    .collect::<Vec<_>>()
                    .join(" && "),
            ),
            Self::RenameSetupService { place } => line(format!(
                "ployz service rename setup NAME {}",
                place.environment_first()
            )),
            Self::VolumeInspect { volume, place } => line(format!(
                "ployz volume inspect {volume} {}",
                place.environment_first()
            )),
            Self::VolumeAllowSharedWrites { volume, place } => line(format!(
                "ployz volume set {volume} --shared-writes {}",
                place.environment_first()
            )),
            Self::VolumeAdd {
                volume,
                mounts,
                storage,
                place,
            } => {
                let mut words = vec!["ployz".to_owned(), "volume".into(), "add".into()];
                words.push(volume.clone());
                for mount in mounts {
                    words.push("--mount".into());
                    words.push(mount.clone());
                }
                match storage {
                    Storage::Docker => words.push("--docker".into()),
                    Storage::Size(size) => {
                        words.push("--size".into());
                        words.push(size.clone());
                    }
                }
                words.extend([
                    "--shared-writes".into(),
                    "--env".into(),
                    place.environment.to_string(),
                    "--project".into(),
                    place.project.to_string(),
                ]);
                line(shell_words::join(words))
            }
            Self::UpdateClient { beta: false } => line("curl -fsSL https://ployz.sh | sh".into()),
            Self::UpdateClient { beta: true } => {
                line("curl -fsSL https://ployz.sh | sh -s -- beta".into())
            }
            Self::ShowEnvironment { place } => Cli::Tool(
                "environment_show",
                json!({ "project": place.project, "environment": place.environment }),
            ),
            Self::ShowProject { project } => {
                Cli::Tool("project_show", json!({ "project": project }))
            }
            Self::UnprotectEnvironment { place } => Cli::Tool(
                "environment_protect",
                json!({
                    "project": place.project,
                    "environment": place.environment,
                    "protected": false,
                }),
            ),
            Self::StopKeepingBranch { place } => Cli::Tool(
                "keep_branch",
                json!({
                    "environment": {
                        "project": place.project,
                        "environment": place.environment,
                    },
                    "kept": false,
                }),
            ),
            Self::Remove {
                project,
                environment,
            } => Cli::Tool(
                "remove",
                match environment {
                    Some(environment) => {
                        json!({ "project": project, "environment": environment })
                    }
                    None => json!({ "project": project }),
                },
            ),
        }
    }
}

/// The command line a message names, or the tool call as JSON.
impl fmt::Display for FollowUp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.render() {
            Cli::Line(line) => f.write_str(&line),
            Cli::Tool(tool, input) => write!(f, "{}", json!({ "tool": tool, "input": input })),
        }
    }
}
