//! Git-backed Services: creating one from a repository, and the Settings of its
//! source and build.
//!
//! The Store never talks to GitHub. Cloud checks which repositories the calling
//! Organization may read (through a GitHub installation, or as a public repository)
//! and which branches exist, and passes what it found as [`Trusted`] evidence. A
//! repository's identity and access come only from that evidence, so a caller can
//! name a repository but never claim an installation. Without evidence (the hidden
//! SQLite Store) no repository can be connected.

use crate::id::{BranchName, RepositoryId, RepositoryName};
use ployz_core::config::{
    AuthoredServiceConfig, BRANCH_MAX, BuildMethod, COMMAND_MAX, DOCKERFILE_PATH_MAX,
    REPOSITORY_MAX, SavedEnvironmentIntent, ServiceGitAccess, ServiceGitBranch, ServiceSource,
    parse_service_setting,
};
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use super::command::{Command, replayable};
use crate::error;
use crate::id::ServiceLineageId;
use crate::scope::EnvironmentRef;
use crate::settings::ServiceSetting;
use crate::storage::Tx;
use crate::{Actor, ServiceStaged, Trusted};

/// A repository Cloud checked the Organization may read, and the branches it saw.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedRepository {
    /// Its `owner/name`, as GitHub spells it.
    pub repository: RepositoryName,
    /// GitHub's ID for it.
    pub repository_id: RepositoryId,
    /// How Cloud reads it: publicly, or through a GitHub installation.
    pub access: ServiceGitAccess,
    /// Its default branch.
    pub default_branch: BranchName,
    /// Other branches Cloud saw exist.
    #[serde(default)]
    pub branches: Vec<BranchName>,
}

impl AuthorizedRepository {
    fn has_branch(&self, branch: &str) -> bool {
        self.default_branch.as_str() == branch
            || self.branches.iter().any(|name| name.as_str() == branch)
    }
}

impl Trusted {
    fn repository(&self, name: &str) -> Option<&AuthorizedRepository> {
        self.repositories
            .iter()
            .find(|found| found.repository.as_str().eq_ignore_ascii_case(name.trim()))
    }
}

/// Create a Service that builds `repository`'s `branch` (its default branch when omitted).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CreateGitService {
    /// The new Service's ID, also its lineage.
    pub id: ServiceLineageId,
    /// The Environment to create it in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its name, unique in the Environment.
    pub name: ServiceName,
    /// The GitHub repository, as `owner/name`.
    pub repository: RepositoryName,
    /// The branch to build; the repository's default branch when omitted.
    #[serde(default)]
    pub branch: Option<BranchName>,
}

pub(crate) fn create_git_service(
    tx: &mut dyn Tx,
    who: &Actor,
    create: &CreateGitService,
    trusted: &Trusted,
) -> Result<ServiceStaged, RpcError> {
    let command = Command::CreateGitService(create.clone());
    replayable(tx, who, &command, |tx| {
        let found = authorized(trusted, create.repository.as_str())?;
        let branch = create.branch.as_ref().unwrap_or(&found.default_branch);
        if !found.has_branch(branch.as_str()) {
            return Err(no_branch(found.repository.as_str(), create.name.as_str()));
        }
        let source = ServiceSource::Git {
            version: 2,
            repository: found.repository.to_string(),
            repository_id: found.repository_id.get(),
            access: found.access.clone(),
            root_dir: "/".into(),
            branch: ServiceGitBranch::Connected {
                name: branch.to_string(),
            },
        };
        super::command::insert_service(
            tx,
            who,
            &create.id,
            &create.environment,
            &create.name,
            source,
        )
    })
}

/// The Settings of a Git-backed Service's source and build.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GitSetting {
    Repository,
    Branch,
    RootDir,
    BuildMethod,
    DockerfilePath,
    BuildCommand,
}

impl GitSetting {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Repository => "repository",
            Self::Branch => "branch",
            Self::RootDir => "rootDir",
            Self::BuildMethod => "buildMethod",
            Self::DockerfilePath => "dockerfilePath",
            Self::BuildCommand => "buildCommand",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Repository => "Repository",
            Self::Branch => "Branch",
            Self::RootDir => "Root directory",
            Self::BuildMethod => "Build method",
            Self::DockerfilePath => "Dockerfile path",
            Self::BuildCommand => "Build command",
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::Repository => {
                "The GitHub repository the Service builds, as owner/name. Ployz checks the Organization can read it; a change keeps the branch if the new repository has it, else uses its default branch."
            }
            Self::Branch => "The branch the Service builds. Ployz checks it exists.",
            Self::RootDir => {
                "The repository directory the build starts in. Unset means the repository root."
            }
            Self::BuildMethod => "How the image is built: Railpack detects it, or a Dockerfile.",
            Self::DockerfilePath => {
                "The Dockerfile to build, relative to the root directory. Unset means Dockerfile."
            }
            Self::BuildCommand => {
                "Overrides Railpack's build command. Unset keeps detection; Dockerfiles ignore it."
            }
        }
    }

    /// The core config field, as change rows name it.
    pub(crate) const fn field(self) -> &'static str {
        match self {
            Self::Repository => "source.repository",
            Self::Branch => "source.branch",
            Self::RootDir => "source.rootDir",
            Self::BuildMethod => "build.buildMethod",
            Self::DockerfilePath => "build.dockerfilePath",
            Self::BuildCommand => "build.command",
        }
    }

    pub(crate) fn default(self) -> Value {
        match self {
            Self::BuildMethod => json!("railpack"),
            Self::Repository
            | Self::Branch
            | Self::RootDir
            | Self::DockerfilePath
            | Self::BuildCommand => Value::Null,
        }
    }

    pub(crate) fn expected(self) -> Value {
        match self {
            Self::Repository => json!({
                "type": "string",
                "pattern": "^[A-Za-z0-9-]+/[A-Za-z0-9_.-]+$",
                "maxLength": REPOSITORY_MAX,
            }),
            Self::Branch => json!({ "type": "string", "minLength": 1, "maxLength": BRANCH_MAX }),
            Self::RootDir => json!({ "type": "string", "pattern": "^/[A-Za-z0-9/._-]*$" }),
            Self::BuildMethod => json!({ "type": "string", "enum": ["dockerfile", "railpack"] }),
            Self::DockerfilePath => {
                json!({ "type": "string", "minLength": 1, "maxLength": DOCKERFILE_PATH_MAX })
            }
            Self::BuildCommand => {
                json!({ "type": "string", "minLength": 1, "maxLength": COMMAND_MAX })
            }
        }
    }

    pub(crate) fn examples(self) -> Value {
        match self {
            Self::Repository => json!(["acme/web"]),
            Self::Branch => json!(["main"]),
            Self::RootDir => json!(["/apps/web"]),
            Self::BuildMethod => json!(["dockerfile"]),
            Self::DockerfilePath => json!(["docker/web.Dockerfile"]),
            Self::BuildCommand => json!(["npm run build"]),
        }
    }

    pub(crate) fn value(self, config: &AuthoredServiceConfig) -> Value {
        let build = &config.build;
        match (self, &config.source) {
            (Self::Repository, ServiceSource::Git { repository, .. }) => json!(repository),
            (Self::Branch, ServiceSource::Git { branch, .. }) => match branch {
                ServiceGitBranch::Connected { name } => json!(name),
                ServiceGitBranch::Disconnected { .. } => Value::Null,
            },
            (Self::RootDir, ServiceSource::Git { root_dir, .. }) if root_dir != "/" => {
                json!(root_dir)
            }
            (Self::BuildMethod, _) => json!(build.build_method),
            (Self::DockerfilePath, _) => json!(build.dockerfile_path),
            (Self::BuildCommand, _) => json!(build.command),
            (Self::Repository | Self::Branch | Self::RootDir, _) => Value::Null,
        }
    }

    /// A change row's value as `get` shows it: a branch by name, never its stored shape.
    pub(crate) fn shown(self, value: Value) -> Value {
        match self {
            Self::Branch => value
                .get("name")
                .or_else(|| value.get("previousName"))
                .cloned()
                .unwrap_or(Value::Null),
            // Never its ID or access: only its name.
            Self::Repository if value.is_object() => {
                value.get("repository").cloned().unwrap_or(Value::Null)
            }
            Self::Repository
            | Self::RootDir
            | Self::BuildMethod
            | Self::DockerfilePath
            | Self::BuildCommand => value,
        }
    }

    /// Write `value`, already coerced. A repository comes only from `trusted` evidence.
    pub(crate) fn set(
        self,
        config: &mut AuthoredServiceConfig,
        value: Value,
        trusted: &Trusted,
    ) -> Result<(), RpcError> {
        let setting = ServiceSetting::Git(self);
        let Value::String(text) = value else {
            return Err(setting.invalid("expected text"));
        };
        match self {
            Self::BuildMethod => {
                config.build.build_method = setting.decode(json!(text))?;
                validate_build(config, self)
            }
            Self::DockerfilePath => {
                config.build.dockerfile_path = Some(text);
                validate_build(config, self)
            }
            Self::BuildCommand => {
                config.build.command = Some(text);
                validate_build(config, self)
            }
            Self::Repository => {
                let ServiceSource::Git {
                    repository,
                    repository_id,
                    access,
                    branch,
                    ..
                } = &mut config.source
                else {
                    return Err(not_git(setting));
                };
                if repository.eq_ignore_ascii_case(text.trim()) {
                    return Ok(());
                }
                let found = authorized(trusted, &text)?;
                let keeps_branch = matches!(branch,
                    ServiceGitBranch::Connected { name } if found.has_branch(name));
                if !keeps_branch {
                    *branch = ServiceGitBranch::Connected {
                        name: found.default_branch.to_string(),
                    };
                }
                *repository = found.repository.to_string();
                *repository_id = found.repository_id.get();
                *access = found.access.clone();
                Ok(())
            }
            Self::Branch => {
                let ServiceSource::Git { branch, .. } = &mut config.source else {
                    return Err(not_git(setting));
                };
                *branch = ServiceGitBranch::Connected {
                    name: branch_named(&text)?.to_string(),
                };
                Ok(())
            }
            Self::RootDir => {
                let ServiceSource::Git { root_dir, .. } = &mut config.source else {
                    return Err(not_git(setting));
                };
                *root_dir = setting.decode(
                    parse_service_setting(json!({ "field": "rootDir", "value": text }))
                        .map_err(|error| setting.invalid(&error.message))?,
                )?;
                Ok(())
            }
        }
    }

    pub(crate) fn unset(self, config: &mut AuthoredServiceConfig) -> Result<(), RpcError> {
        let setting = ServiceSetting::Git(self);
        match self {
            Self::Repository | Self::Branch => Err(setting.invalid(
                "a repository Service always builds a repository and branch; set another one",
            )),
            Self::RootDir => match &mut config.source {
                ServiceSource::Git { root_dir, .. } => {
                    "/".clone_into(root_dir);
                    Ok(())
                }
                ServiceSource::Empty { .. } | ServiceSource::Image { .. } => Err(not_git(setting)),
            },
            Self::BuildMethod => {
                config.build.build_method = BuildMethod::default();
                Ok(())
            }
            Self::DockerfilePath => {
                config.build.dockerfile_path = None;
                Ok(())
            }
            Self::BuildCommand => {
                config.build.command = None;
                Ok(())
            }
        }
    }
}

/// Whether `service` builds `branch` of repository `repository_id`.
pub(crate) fn tracks(
    service: &ployz_core::config::SavedServiceIntent,
    repository_id: RepositoryId,
    branch: &BranchName,
) -> bool {
    matches!(
        &service.config.source,
        ServiceSource::Git { repository_id: at, branch: ServiceGitBranch::Connected { name }, .. }
            if *at == repository_id.get() && name == branch.as_str()
    )
}

/// Refuse any Git source in `after` whose repository, access or branch changed from
/// `before` unless `trusted` evidence vouches for exactly that repository and branch.
pub(crate) fn check_sources(
    before: &SavedEnvironmentIntent,
    after: &SavedEnvironmentIntent,
    trusted: &Trusted,
) -> Result<(), RpcError> {
    for service in &after.services {
        let ServiceSource::Git {
            repository,
            repository_id,
            access,
            branch,
            ..
        } = &service.config.source
        else {
            continue;
        };
        let identity = |source: &ServiceSource| match source {
            ServiceSource::Git {
                repository_id,
                access,
                branch,
                ..
            } => Some((*repository_id, access.clone(), branch.clone())),
            ServiceSource::Empty { .. } | ServiceSource::Image { .. } => None,
        };
        let was = before
            .services
            .iter()
            .find(|old| old.id == service.id)
            .and_then(|old| identity(&old.config.source));
        if was == identity(&service.config.source) {
            continue;
        }
        let ServiceGitBranch::Connected { name } = branch else {
            continue;
        };
        let vouched = trusted.repositories.iter().any(|found| {
            found.repository_id.get() == *repository_id
                && found.access == *access
                && found.has_branch(name)
        });
        if !vouched {
            return Err(no_branch(repository, &service.slug));
        }
    }
    Ok(())
}

fn not_git(setting: ServiceSetting) -> RpcError {
    setting.invalid("this Service does not build from a repository")
}

/// The branch asked for isn't one Cloud saw in `repository`; the branch is never echoed.
fn no_branch(repository: &str, service: &str) -> RpcError {
    error::not_found(
        format!("Repository {repository} has no such branch that Ployz could check"),
        json!({
            "setting": "branch",
            "service": service,
            "next": format!("ployz github ls {repository}"),
        }),
    )
}

/// The evidence for `repository`, or `not_found` naming how to connect it.
fn authorized<'a>(
    trusted: &'a Trusted,
    repository: &str,
) -> Result<&'a AuthorizedRepository, RpcError> {
    if RepositoryName::parse(repository.trim()).is_err() {
        return Err(ServiceSetting::Git(GitSetting::Repository).invalid("expected owner/name"));
    }
    trusted.repository(repository).ok_or_else(|| {
        error::not_found(
            "No repository by that name that this Organization can read: connect it through GitHub, or name a public one",
            json!({ "setting": "repository", "next": "ployz github connect" }),
        )
    })
}

/// The branch `text` names, trimmed, or why it names none.
fn branch_named(text: &str) -> Result<BranchName, RpcError> {
    BranchName::parse(text.trim())
        .map_err(|_| ServiceSetting::Git(GitSetting::Branch).invalid("expected a Git branch name"))
}

fn validate_build(config: &mut AuthoredServiceConfig, setting: GitSetting) -> Result<(), RpcError> {
    let setting = ServiceSetting::Git(setting);
    let build = serde_json::to_value(&config.build).expect("build settings are JSON");
    let build = parse_service_setting(json!({ "field": "build", "value": build }))
        .map_err(|error| setting.invalid(&error.message))?;
    config.build = setting.decode(build)?;
    Ok(())
}
