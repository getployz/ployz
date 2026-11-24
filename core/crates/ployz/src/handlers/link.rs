//! `ployz link` and `ployz ctx`: which Project and Environment a directory acts on, and
//! how commands reach the Servers.

use std::io;
use std::path::Path;
use std::time::Duration;

use clap::{ArgMatches, Command};
use ployz_store::{
    EnvironmentName, EnvironmentQuery, EnvironmentSummary, ProjectName, ProjectsQuery,
};
use serde::Serialize;

use super::store::{Backend, LOCAL_ORGANIZATION, Store, backend_at, scoped, store};
use super::{Error, config_path, leaf_matches};
use crate::cloud;
use crate::cloud::account::Credential;
use crate::cloud::login::{Account, CredentialStore, Organization};
use crate::context::{Config, ConfigError, ContextError, select_connections};
use crate::scope::{Link, Scope, acting_organization, find, here, load, save};
use crate::ui::{Fields, Next};

pub(super) fn link_command(command: Command) -> Command {
    scoped(command.about("Link this directory to a Project and Environment"))
}

pub(crate) fn scope(matches: &ArgMatches) -> Result<Scope, Error> {
    Scope::flags(matches)?.linked_here(&config_path(matches)?)
}

/// The scope from `PLOYZ_PROJECT`, `PLOYZ_ENV` and the directory link alone, for
/// shell completion, which sees no flags.
pub(crate) fn scope_from_env(config: &Path) -> Result<Scope, Error> {
    Scope::from_env()?.linked_here(config)
}

/// [`acting_organization`], asking Cloud for an Organization Token's, so a link
/// always records the Organization it was made in.
fn resolved_organization(config: &Path) -> Result<Option<Organization>, Error> {
    if let Some(acting) = acting_organization(config)? {
        return Ok(Some(acting));
    }
    let credentials = CredentialStore::beside(config);
    let organization = super::runtime()?.block_on(async {
        let credential = crate::cloud::account::from_env(&credentials).await?;
        crate::cloud::account::acting_in(&credential).await
    })?;
    Ok(Some(organization))
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
        environment: Scope::flags(matches)?.at(),
        path: None,
        all: false,
    };
    let view = store.read(&query)?;
    let linked = record(&config, view.environment)?;
    let followed = Next::new(&linked, Some("ployz status".to_owned()));
    crate::ui::done(
        &followed,
        format_args!(
            "Linked {} to {}/{}.",
            linked.directory, linked.project, linked.environment
        ),
    )?;
    followed.say();
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
    acting: Option<&str>,
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
            .is_some_and(|linked| linked.id == acting);
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
    pub(super) credential: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) cloud: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) account: Option<Account>,
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
            let organizations = runtime.block_on(cloud::account::organizations(credential))?;
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

/// How long `ployz ctx` waits for the Store to list the Projects.
const LISTING: Duration = Duration::from_secs(2);

/// The scope, the acting Organization and how Servers are reached: `ployz ctx`.
/// Only with no Project named does it ask the Store, once, what the Projects are.
pub(super) fn show(root: &ArgMatches) -> Result<(), Error> {
    #[derive(Serialize)]
    struct Context<'a> {
        organization: Option<Organization>,
        credential: Option<&'static str>,
        #[serde(flatten)]
        scope: &'a Scope,
        /// What an unnamed Project falls back to, when the Store said.
        #[serde(skip_serializing_if = "Option::is_none")]
        default: Option<Fallback>,
        servers: Servers,
    }
    let matches = leaf_matches(root);
    let config = config_path(matches)?;
    let scope = scope(matches)?;
    let default = scope.project.is_none().then(|| fallback(&config)).flatten();
    let organization = acting_organization(&config)?;
    let credential = if super::store::local_mode() {
        Some("local")
    } else if crate::cloud::account::env_token().is_some() {
        Some("token")
    } else {
        organization.as_ref().map(|_| "device")
    };
    let context = Context {
        organization,
        credential,
        scope: &scope,
        default,
        servers: servers(matches, &config)?,
    };
    let only = context
        .default
        .as_ref()
        .and_then(|default| default.project.as_ref().zip(default.environment.as_ref()));
    let hint = if context.credential.is_none() && context.servers.needs_login() {
        Some("ployz login")
    } else {
        match (&scope.project, &context.default, only) {
            (Some(_), ..) | (None, _, Some(_)) => None,
            (None, Some(Fallback { projects: 0, .. }), None) => Some("ployz project new NAME"),
            (None, _, None) => Some("ployz link --project PROJECT"),
        }
    }
    .map(str::to_owned);
    // No Server reached blocks more than an unnamed Project does.
    let hint = context
        .servers
        .none
        .as_ref()
        .and_then(|(_, next)| next.clone())
        .or(hint);
    let project = match (&scope.project, &context.default) {
        (Some(project), _) => format!("{} ({})", project.name, project.source.as_str()),
        (
            None,
            Some(Fallback {
                project: Some(project),
                ..
            }),
        ) => {
            format!("{project} (the only Project)")
        }
        (None, Some(Fallback { projects: 0, .. })) => {
            "(none: this Organization has no Project yet)".to_owned()
        }
        (None, Some(Fallback { projects, .. })) => {
            format!("(none: this Organization has {projects} Projects)")
        }
        (None, None) => "(none)".to_owned(),
    };
    let environment = match (&scope.environment, &scope.project, only) {
        (Some(environment), ..) => {
            format!("{} ({})", environment.name, environment.source.as_str())
        }
        (None, Some(_), _) => "the Default Environment".to_owned(),
        (None, None, Some((_, environment))) => format!("{environment} (the Default Environment)"),
        (None, None, None) => "(none)".to_owned(),
    };
    let record = Fields::new()
        .field(
            "organization",
            context.organization.as_ref().map_or(
                if context.credential.is_none() {
                    "(none: not signed in)"
                } else {
                    "(the token's)"
                },
                |organization| organization.slug.as_str(),
            ),
        )
        .field("project", project)
        .field("environment", environment)
        .field("servers", context.servers.label());
    let followed = Next::new(&context, hint);
    crate::ui::fields(&followed, &record)?;
    followed.say();
    Ok(())
}

/// The Projects the Store holds, and with just one its Default Environment.
#[derive(Serialize)]
struct Fallback {
    projects: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<ProjectName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    environment: Option<EnvironmentName>,
}

/// What an unnamed Project falls back to, or `None` when the Store can't say:
/// signed out, offline, or not answering within [`LISTING`].
fn fallback(config: &Path) -> Option<Fallback> {
    let listed = backend_at(config)
        .ok()??
        .read_within(&ProjectsQuery {}, LISTING)?;
    let only = match listed.projects.as_slice() {
        [only] => Some(only),
        _ => None,
    };
    Some(Fallback {
        projects: listed.projects.len(),
        project: only.map(|only| only.name.clone()),
        environment: only.map(|only| only.default_environment.clone()),
    })
}

/// How live commands reach Servers.
#[derive(Serialize)]
struct Servers {
    /// `connect` (`--connect`), `context`, `cloud` (Server Access) or `local` (this host's daemon).
    via: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_socket_exists: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_socket_status: Option<LocalSocketStatus>,
    /// With `via` `none`, why, and the step the failing command names.
    #[serde(skip)]
    none: Option<(&'static str, Option<String>)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LocalSocketStatus {
    Visible,
    Missing,
    PermissionDenied,
    Unknown,
}

impl Servers {
    fn local(result: io::Result<bool>) -> Self {
        // Absent when the probe failed: the socket may exist behind an unreadable directory.
        let exists = result.as_ref().ok().copied();
        let status = match result {
            Ok(true) => LocalSocketStatus::Visible,
            Ok(false) => LocalSocketStatus::Missing,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                LocalSocketStatus::PermissionDenied
            }
            Err(_) => LocalSocketStatus::Unknown,
        };
        Self {
            via: "local",
            context: None,
            local_socket_exists: exists,
            local_socket_status: Some(status),
            none: None,
        }
    }

    fn needs_login(&self) -> bool {
        matches!(
            self.local_socket_status,
            Some(
                LocalSocketStatus::Missing
                    | LocalSocketStatus::PermissionDenied
                    | LocalSocketStatus::Unknown
            )
        )
    }

    fn label(&self) -> String {
        if let Some((why, _)) = &self.none {
            return format!("(none: {why})");
        }
        if let Some(name) = &self.context {
            return format!("via context {name}");
        }
        match self.local_socket_status {
            Some(LocalSocketStatus::Missing) => "(none: no local daemon socket)".to_owned(),
            Some(LocalSocketStatus::PermissionDenied) => {
                "(none: can't read the local daemon socket; run with sudo or join the ployz group)"
                    .to_owned()
            }
            Some(LocalSocketStatus::Unknown) => {
                "(none: can't check the local daemon socket)".to_owned()
            }
            Some(LocalSocketStatus::Visible) | None => format!("via {}", self.via),
        }
    }
}

/// In `route::connect`'s order. A `--connect` target isn't echoed: it can carry a capability.
fn servers(matches: &ArgMatches, config: &Path) -> Result<Servers, Error> {
    let via = |via, context| Servers {
        via,
        context,
        local_socket_exists: None,
        local_socket_status: None,
        none: None,
    };
    let none = |why, next| Servers {
        via: "none",
        context: None,
        local_socket_exists: None,
        local_socket_status: None,
        none: Some((why, next)),
    };
    if matches.get_one::<String>("connect").is_some() {
        return Ok(via("connect", None));
    }
    if let Some(context) = matches.get_one::<String>("context") {
        return Ok(via("context", Some(context.clone())));
    }
    let token = crate::cloud::account::env_token().is_some();
    if token || CredentialStore::beside(config).organization()?.is_some() {
        return Ok(via("cloud", None));
    }
    let local = ployz_core::MachineLayout::production().socket();
    let config = match Config::load(config) {
        Ok(config) => config,
        Err(ConfigError::Read { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(Servers::local(local.try_exists()));
        }
        Err(error) => return Err(error.into()),
    };
    if let Some(context) = config.current_context() {
        return Ok(via("context", Some(context.to_owned())));
    }
    // A config file sends commands to its current context, never the local socket, so
    // without one they reach no Server: say what they say.
    Ok(
        match select_connections(None, Some(&config), None, false, &local) {
            Err(ContextError::NoCurrentContext(_)) => none(
                "no current context",
                Some(super::rerun(matches, &["ctx", "use", "CONTEXT"])),
            ),
            Err(ContextError::NoContexts(_)) => {
                none("no contexts", super::context::no_contexts_next(matches))
            }
            _ => Servers::local(local.try_exists()),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_socket_check_distinguishes_visibility_and_permission() {
        let cases = [
            (
                Ok(true),
                LocalSocketStatus::Visible,
                "via local",
                Some(true),
            ),
            (
                Ok(false),
                LocalSocketStatus::Missing,
                "(none: no local daemon socket)",
                Some(false),
            ),
            (
                Err(io::Error::from(io::ErrorKind::PermissionDenied)),
                LocalSocketStatus::PermissionDenied,
                "(none: can't read the local daemon socket; run with sudo or join the ployz group)",
                None,
            ),
            (
                Err(io::Error::other("unavailable")),
                LocalSocketStatus::Unknown,
                "(none: can't check the local daemon socket)",
                None,
            ),
        ];
        for (result, status, label, exists) in cases {
            let servers = Servers::local(result);
            assert_eq!(servers.label(), label);
            assert_eq!(servers.needs_login(), exists != Some(true));
            let json = serde_json::to_value(&servers).unwrap();
            assert_eq!(
                json.pointer("/local_socket_exists"),
                exists.map(serde_json::Value::Bool).as_ref()
            );
            assert_eq!(
                json.pointer("/local_socket_status"),
                Some(&serde_json::to_value(status).unwrap())
            );
        }
    }
}
