//! `ployz link` and `ployz status`: which Project and Environment a directory acts
//! on, and what is staged, deploying and needs attention there.
//!
//! Scope precedence: `--project`/`--env`, then `PLOYZ_PROJECT`/`PLOYZ_ENV`, then the
//! link of the nearest linked directory, then the Store's defaults (the only
//! Project, its Default Environment). A link's Environment applies only within its
//! own Project. Links live beside the Ployz config, never in the directory, so
//! nothing needs ignoring or uploading.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::parser::ValueSource;
use clap::{ArgMatches, Command};
use ployz_core::RpcErrorCode;
use ployz_store::{
    DeploymentId, DeploymentStatus, DeploymentSummary, DeploymentsQuery, DiffQuery,
    EnvironmentName, EnvironmentQuery, EnvironmentRef, EnvironmentSummary, ProjectName,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::store::{Backend, LOCAL_ORGANIZATION, Next, Store, next, scoped, store};
use super::{Error, config_path, leaf_matches};
use crate::cli::env;
use crate::cloud_account::{self, Credential, StoreCallError};
use crate::cloud_login::{Account, CredentialStore, Organization};
use crate::ui::{Fields, Hint};

pub(crate) fn link_command() -> Command {
    scoped(Command::new("link").about("Link this directory to a Project and Environment"))
}

pub(crate) fn status_command() -> Command {
    scoped(Command::new("status").about(
        "Show who you are, where commands act, and what is staged, deploying or needs attention",
    ))
}

/// Where a scope value came from.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Source {
    Flag,
    Env,
    Link,
}

impl Source {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Env => "env",
            Self::Link => "link",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct Named<T> {
    name: T,
    source: Source,
}

/// The Project and Environment a command acts on, as far as the command line, the
/// environment and the directory link say. `None` leaves it to the Store's default.
#[derive(Debug, Serialize)]
pub(crate) struct Scope {
    project: Option<Named<ProjectName>>,
    environment: Option<Named<EnvironmentName>>,
    /// The linked directory that supplied part of the scope.
    link: Option<String>,
}

impl Scope {
    pub(crate) fn at(&self) -> EnvironmentRef {
        EnvironmentRef {
            project: self.project.as_ref().map(|named| named.name.clone()),
            environment: self.environment.as_ref().map(|named| named.name.clone()),
        }
    }

    /// Fill what flags and environment variables left open from the directory link.
    fn linked(mut self, config: &Path) -> Result<Self, Error> {
        if self.project.is_some() && self.environment.is_some() {
            return Ok(self);
        }
        let Some((directory, link)) = find(config)? else {
            return Ok(self);
        };
        check_organization(config, &directory, &link)?;
        let project = self.project.get_or_insert(Named {
            name: link.project.clone(),
            source: Source::Link,
        });
        if self.environment.is_none() && project.name == link.project {
            self.environment = Some(Named {
                name: link.environment,
                source: Source::Link,
            });
        }
        let from_link = |source: Option<Source>| matches!(source, Some(Source::Link));
        if from_link(self.project.as_ref().map(|named| named.source))
            || from_link(self.environment.as_ref().map(|named| named.source))
        {
            self.link = Some(directory);
        }
        Ok(self)
    }
}

/// The scope a command acts on.
pub(crate) fn scope(matches: &ArgMatches) -> Result<Scope, Error> {
    given(matches)?.linked(&config_path(matches)?)
}

/// The scope from `PLOYZ_PROJECT`, `PLOYZ_ENV` and the directory link alone, for
/// shell completion, which sees no flags.
pub(crate) fn scope_from_env(config: &Path) -> Result<Scope, Error> {
    let named = |name: &str| std::env::var(name).ok();
    Scope {
        project: named(env::PROJECT)
            .map(|name| ProjectName::parse(name).map(env_named))
            .transpose()?,
        environment: named(env::ENVIRONMENT)
            .map(|name| EnvironmentName::parse(name).map(env_named))
            .transpose()?,
        link: None,
    }
    .linked(config)
}

fn env_named<T>(name: T) -> Named<T> {
    Named {
        name,
        source: Source::Env,
    }
}

/// `--project`/`--env` and their environment variables, without the link.
fn given(matches: &ArgMatches) -> Result<Scope, Error> {
    Ok(Scope {
        project: named(matches, "project", ProjectName::parse)?,
        environment: named(matches, "env", EnvironmentName::parse)?,
        link: None,
    })
}

fn named<T>(
    matches: &ArgMatches,
    id: &str,
    parse: fn(String) -> Result<T, ployz_core::RpcError>,
) -> Result<Option<Named<T>>, Error> {
    // Not every scoped command takes both flags (`env new` has no `--env`).
    let Ok(Some(value)) = matches.try_get_one::<String>(id) else {
        return Ok(None);
    };
    let source = match matches.value_source(id) {
        Some(ValueSource::EnvVariable) => Source::Env,
        _ => Source::Flag,
    };
    Ok(Some(Named {
        name: parse(value.clone())?,
        source,
    }))
}

/// A directory's link. Names, not IDs: they're what every command addresses.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Link {
    /// The Organization it was linked in; unknown under `PLOYZ_TOKEN`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    organization: Option<Organization>,
    project: ProjectName,
    environment: EnvironmentName,
}

fn links_path(config: &Path) -> PathBuf {
    config.with_file_name("links.json")
}

/// Every link, by canonical directory.
fn load(config: &Path) -> Result<BTreeMap<String, Link>, Error> {
    let path = links_path(config);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            Error::caused(
                RpcErrorCode::Internal,
                format!("Could not read directory links {}.", path.display()),
                error,
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error.into()),
    }
}

// ponytail: whole-file rewrite; two `link`s racing can lose one. Lock the file if that bites.
fn save(config: &Path, links: &BTreeMap<String, Link>) -> Result<(), Error> {
    let path = links_path(config);
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer_pretty(&mut file, links)?;
    file.persist(&path).map_err(|error| error.error)?;
    Ok(())
}

fn here() -> Result<String, Error> {
    std::env::current_dir()?
        .canonicalize()?
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::usage("cannot link a directory whose path is not UTF-8"))
}

/// The link of this directory or its nearest linked ancestor.
fn find(config: &Path) -> Result<Option<(String, Link)>, Error> {
    let links = load(config)?;
    if links.is_empty() {
        return Ok(None);
    }
    let here = here()?;
    Ok(Path::new(&here).ancestors().find_map(|dir| {
        let dir = dir.to_str()?;
        links.get(dir).map(|link| (dir.to_owned(), link.clone()))
    }))
}

/// The Organization commands act in, as far as this device knows without Cloud.
fn acting_organization(config: &Path) -> Result<Option<Organization>, Error> {
    if super::store::local_mode() {
        return Ok(Some(Organization {
            id: LOCAL_ORGANIZATION.to_owned(),
            slug: LOCAL_ORGANIZATION.to_owned(),
        }));
    }
    // ponytail: an Organization Token's Organization is only known to Cloud, so a link
    // is not checked against it; ask `GET organizations` here if tokens meet links.
    if std::env::var(env::TOKEN).is_ok_and(|token| !token.is_empty()) {
        return Ok(None);
    }
    Ok(CredentialStore::beside(config).organization()?)
}

/// [`acting_organization`], asking Cloud for an Organization Token's, so a link
/// always records the Organization it was made in.
fn resolved_organization(config: &Path) -> Result<Option<Organization>, Error> {
    if let Some(acting) = acting_organization(config)? {
        return Ok(Some(acting));
    }
    let credentials = CredentialStore::beside(config);
    let organization = super::runtime()?.block_on(async {
        let credential = crate::cloud_account::from_env(&credentials).await?;
        crate::cloud_account::acting_in(&credential).await
    })?;
    Ok(Some(organization))
}

/// A link made in another Organization never addresses this one's Project of the same name.
fn check_organization(config: &Path, directory: &str, link: &Link) -> Result<(), Error> {
    let (Some(linked), Some(acting)) = (&link.organization, acting_organization(config)?) else {
        return Ok(());
    };
    if linked.id == acting.id {
        return Ok(());
    }
    let local = |organization: &Organization| organization.id == LOCAL_ORGANIZATION;
    let next = if local(linked) || local(&acting) {
        "ployz link".to_owned()
    } else {
        format!("ployz org use {}", linked.slug)
    };
    Err(Error::detailed(
        RpcErrorCode::Conflict,
        format!(
            "{directory} is linked in Organization {}, but you are acting in {}",
            linked.slug, acting.slug
        ),
        json!({ "link": directory, "linked": linked.slug, "acting": acting.slug }),
    )
    .hint(Hint::Next(next)))
}

#[derive(Serialize)]
pub(super) struct Linked {
    pub(super) directory: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    organization: Option<Organization>,
    project: ProjectName,
    environment: EnvironmentName,
}

/// `ployz link`: resolve the scope in the Store now, then record it for this directory.
pub(super) fn link(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let config = config_path(matches)?;
    let store = store(root)?;
    let query = EnvironmentQuery {
        environment: given(matches)?.at(),
        path: None,
        all: false,
    };
    let view = store.read(&query)?;
    let linked = record(&config, view.environment)?;
    let hint = Some("ployz status".to_owned());
    crate::ui::done(
        &Next::new(&linked, hint.clone()),
        format_args!(
            "Linked {} to {}/{}.",
            linked.directory, linked.project, linked.environment
        ),
    )?;
    if let Some(hint) = hint {
        crate::ui::hint(&Hint::Next(hint));
    }
    Ok(())
}

/// Link this directory to `environment`, which the Store just resolved.
pub(super) fn record(config: &Path, environment: EnvironmentSummary) -> Result<Linked, Error> {
    let linked = Linked {
        directory: here()?,
        organization: resolved_organization(config)?,
        project: environment.project,
        environment: environment.name,
    };
    let mut links = load(config)?;
    links.insert(
        linked.directory.clone(),
        Link {
            organization: linked.organization.clone(),
            project: linked.project.clone(),
            environment: linked.environment.clone(),
        },
    );
    save(config, &links)?;
    Ok(linked)
}

/// Point this device's links to Project `old` at `new` after a rename, in the
/// `acting` Organization. A link from another Organization stays.
/// Returns how many moved.
pub(super) fn rename_project(
    config: &Path,
    acting: Option<&Organization>,
    old: &ProjectName,
    new: &ProjectName,
) -> Result<usize, Error> {
    let Some(acting) = acting else {
        return Ok(0);
    };
    let mut links = load(config)?;
    let mut moved = 0;
    for link in links.values_mut() {
        let same = link
            .organization
            .as_ref()
            .is_some_and(|linked| linked.id == acting.id);
        if same && &link.project == old {
            link.project = new.clone();
            moved += 1;
        }
    }
    if moved > 0 {
        save(config, &links)?;
    }
    Ok(moved)
}

/// Link this directory to `environment` unless it or an ancestor is already linked,
/// so a one-off `--env` leaves the link alone. Returns the linked directory.
pub(super) fn record_unless_linked(
    config: &Path,
    environment: EnvironmentSummary,
) -> Result<String, Error> {
    match find(config)? {
        Some((directory, _)) => Ok(directory),
        None => Ok(record(config, environment)?.directory),
    }
}

/// Who commands act as.
#[derive(Serialize)]
pub(super) struct Identity {
    /// `device` (a signed-in device), `token` (`PLOYZ_TOKEN`), or `local` (hidden test mode).
    credential: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) cloud: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    account: Option<Account>,
    pub(super) organization: Option<Organization>,
}

pub(super) fn identity(store: &Store) -> Result<Identity, Error> {
    Ok(match store.backend() {
        Backend::Local(..) => Identity {
            credential: "local",
            cloud: None,
            account: None,
            organization: Some(Organization {
                id: LOCAL_ORGANIZATION.to_owned(),
                slug: LOCAL_ORGANIZATION.to_owned(),
            }),
        },
        Backend::Cloud(_, Credential::Device(signed_in)) => Identity {
            credential: "device",
            cloud: Some(signed_in.cloud.clone()),
            account: Some(signed_in.account.clone()),
            organization: Some(signed_in.organization.clone()),
        },
        Backend::Cloud(runtime, credential @ Credential::Token { cloud, .. }) => {
            let organizations = runtime.block_on(cloud_account::organizations(credential))?;
            Identity {
                credential: "token",
                cloud: Some(cloud.clone()),
                account: None,
                organization: organizations
                    .into_iter()
                    .find(|organization| organization.current)
                    .map(|organization| Organization {
                        id: organization.id,
                        slug: organization.slug,
                    }),
            }
        }
    })
}

/// Working State that isn't deployed yet.
#[derive(Serialize)]
struct Staged {
    /// How many changes, counting each node and each Setting.
    changes: usize,
    /// Whether Saved State already holds them.
    published: bool,
    /// The `ployz diff` version they'd deploy by.
    version: String,
}

/// Something that needs someone to act, with the command that acts.
#[derive(Serialize)]
struct Attention {
    /// An error code, `deployment_failed` or `deployment_unknown`.
    reason: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    deployment: Option<DeploymentId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
}

#[derive(Serialize)]
struct Status {
    identity: Identity,
    scope: Scope,
    /// The Environment the scope resolves to; none when it doesn't resolve.
    environment: Option<EnvironmentSummary>,
    staged: Option<Staged>,
    /// Deployments queued or running, newest first.
    deploying: Vec<DeploymentSummary>,
    attention: Vec<Attention>,
}

/// Enough recent Deployments to hold every in-flight one and the latest that ended.
const RECENT: usize = 5;

/// `ployz status`: one call that orients an agent.
pub(super) fn status(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let identity = identity(&store)?;
    let scope = scope(matches)?;
    let mut attention = Vec::new();
    let diff = match store.try_read(&DiffQuery {
        environment: scope.at(),
    }) {
        Ok(diff) => Some(diff),
        // The scope doesn't resolve: say so, and still say who and where.
        Err(StoreCallError::Refused(error))
            if matches!(error.code, RpcErrorCode::NotFound | RpcErrorCode::Ambiguous) =>
        {
            let next = match error.details.get("next").and_then(|next| next.as_str()) {
                Some(next) => next.to_owned(),
                // Ambiguous, or the linked Project is gone: pick one to act in.
                None => "ployz link --project PROJECT".to_owned(),
            };
            let next = Some(next);
            attention.push(Attention {
                reason: error.code.to_string(),
                message: crate::ui::row(&error),
                deployment: None,
                next,
            });
            None
        }
        Err(error) => return Err(store.fail(error)),
    };
    let mut deploying = Vec::new();
    if diff.is_some() {
        let page = store.read(&DeploymentsQuery {
            environment: scope.at(),
            limit: Some(RECENT),
            cursor: None,
        })?;
        let ended = page.deployments.iter().find(|deployment| {
            !deployment.status.in_flight() && deployment.status != DeploymentStatus::Superseded
        });
        if let Some(ended) = ended {
            let reason = match ended.status {
                DeploymentStatus::Failed => Some("deployment_failed"),
                DeploymentStatus::Unknown => Some("deployment_unknown"),
                DeploymentStatus::Queued
                | DeploymentStatus::Superseded
                | DeploymentStatus::Running
                | DeploymentStatus::Applied
                | DeploymentStatus::Cancelling
                | DeploymentStatus::Cancelled => None,
            };
            if let Some(reason) = reason {
                attention.push(Attention {
                    reason: reason.to_owned(),
                    message: format!("Deployment #{} did not complete", ended.number),
                    deployment: Some(ended.id.clone()),
                    next: Some(super::deploy::show_hint(matches, ended.number)),
                });
            }
        }
        deploying = page
            .deployments
            .into_iter()
            .filter(|deployment| deployment.status.in_flight())
            .collect();
    }
    let staged = diff.as_ref().map(|diff| Staged {
        changes: diff.total_count,
        published: diff.published,
        version: diff.version.clone(),
    });
    let hint = attention
        .first()
        .and_then(|attention| attention.next.clone())
        .or_else(|| {
            staged
                .as_ref()
                .is_some_and(|staged| staged.changes > 0)
                .then(|| next(matches, &["diff"]))
        })
        .or_else(|| {
            deploying
                .first()
                .map(|deployment| super::deploy::show_hint(matches, deployment.number))
        });
    let status = Status {
        identity,
        scope,
        environment: diff.map(|diff| diff.environment),
        staged,
        deploying,
        attention,
    };
    crate::ui::fields(&Next::new(&status, hint.clone()), &status_record(&status))?;
    if let Some(hint) = hint {
        crate::ui::hint(&Hint::Next(hint));
    }
    Ok(())
}

fn status_record(status: &Status) -> Fields {
    let identity = &status.identity;
    let mut record = Fields::new();
    if let Some(cloud) = &identity.cloud {
        record.push("cloud", cloud);
    }
    record.push(
        "organization",
        identity
            .organization
            .as_ref()
            .map_or("?", |organization| organization.slug.as_str()),
    );
    record.push(
        "signed in as",
        identity
            .account
            .as_ref()
            .map_or(identity.credential, |account| account.email.as_str()),
    );
    if let Some(environment) = &status.environment {
        record.push(
            "environment",
            format_args!("{}/{}", environment.project, environment.name),
        );
    }
    if let Some(link) = &status.scope.link {
        record.push("linked from", link);
    }
    if let Some(staged) = &status.staged {
        record.push(
            "staged",
            format_args!(
                "{} {}",
                staged.changes,
                if staged.changes == 1 {
                    "change"
                } else {
                    "changes"
                }
            ),
        );
    }
    for deployment in &status.deploying {
        record.push(
            "deploying",
            format_args!(
                "#{} {}",
                deployment.number,
                super::store::word(&deployment.status)
            ),
        );
    }
    for attention in &status.attention {
        record.push(
            "needs attention",
            crate::ui::Cell::status(&attention.message, crate::ui::Tone::Bad),
        );
    }
    record
}

/// The scope, the acting Organization and how Servers are reached, without asking
/// the Store or Cloud: `ployz ctx`.
pub(super) fn show(root: &ArgMatches) -> Result<(), Error> {
    #[derive(Serialize)]
    struct Context<'a> {
        organization: Option<Organization>,
        #[serde(flatten)]
        scope: &'a Scope,
        servers: Servers,
    }
    let matches = leaf_matches(root);
    let config = config_path(matches)?;
    let scope = scope(matches)?;
    let context = Context {
        organization: acting_organization(&config)?,
        scope: &scope,
        servers: servers(matches, &config)?,
    };
    let record = Fields::new()
        .field(
            "organization",
            context
                .organization
                .as_ref()
                .map_or("(the token's)", |organization| organization.slug.as_str()),
        )
        .field(
            "project",
            scope.project.as_ref().map_or_else(
                || "the only Project".to_owned(),
                |project| format!("{} ({})", project.name, project.source.as_str()),
            ),
        )
        .field(
            "environment",
            scope.environment.as_ref().map_or_else(
                || "the Default Environment".to_owned(),
                |environment| format!("{} ({})", environment.name, environment.source.as_str()),
            ),
        )
        .field(
            "servers",
            context.servers.context.as_ref().map_or_else(
                || format!("via {}", context.servers.via),
                |name| format!("via context {name}"),
            ),
        );
    crate::ui::fields(&context, &record)
}

/// How live commands reach Servers.
#[derive(Serialize)]
struct Servers {
    /// `connect` (`--connect`), `context`, `cloud` (Server Access) or `local` (this host's daemon).
    via: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
}

/// In `server::connect`'s order. A `--connect` target isn't echoed: it can carry a capability.
fn servers(matches: &ArgMatches, config: &Path) -> Result<Servers, Error> {
    let via = |via, context| Servers { via, context };
    if matches.get_one::<String>("connect").is_some() {
        return Ok(via("connect", None));
    }
    if let Some(context) = matches.get_one::<String>("context") {
        return Ok(via("context", Some(context.clone())));
    }
    let token = std::env::var(env::TOKEN).is_ok_and(|token| !token.is_empty());
    if token || CredentialStore::beside(config).organization()?.is_some() {
        return Ok(via("cloud", None));
    }
    let current = crate::context::Config::load_or_empty(config)?
        .current_context()
        .map(str::to_owned);
    Ok(match current {
        Some(context) => via("context", Some(context)),
        None => via("local", None),
    })
}
