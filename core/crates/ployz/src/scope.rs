//! Which Project and Environment a command acts on, and the directory links that say.
//!
//! Scope precedence: `--project`/`--env`, then `PLOYZ_PROJECT`/`PLOYZ_ENV`, then the
//! link of the nearest linked directory, then the Store's defaults (the only
//! Project, its Default Environment). A link's Environment applies only within its
//! own Project. Links live beside the Ployz config, never in the directory, so
//! nothing needs ignoring or uploading.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::ArgMatches;
use clap::parser::ValueSource;
use ployz_core::{Hint, RpcErrorCode};
use ployz_store::{EnvironmentName, EnvironmentRef, ProjectName};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::cloud::account::{LOCAL_ORGANIZATION, local_mode};
use crate::cloud::login::{CredentialStore, Organization};
use crate::env;
use crate::failure::Failure as Error;

/// Where a scope value came from.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Source {
    Flag,
    Env,
    Link,
}

impl Source {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Env => "env",
            Self::Link => "link",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Named<T> {
    pub(crate) name: T,
    pub(crate) source: Source,
}

/// The Project and Environment a command acts on, as far as the command line, the
/// environment and the directory link say. `None` leaves it to the Store's default.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Scope {
    pub(crate) project: Option<Named<ProjectName>>,
    pub(crate) environment: Option<Named<EnvironmentName>>,
    /// The linked directory that supplied part of the scope.
    pub(crate) link: Option<String>,
}

impl Scope {
    pub(crate) fn at(&self) -> EnvironmentRef {
        EnvironmentRef {
            project: self.project.as_ref().map(|named| named.name.clone()),
            environment: self.environment.as_ref().map(|named| named.name.clone()),
        }
    }

    /// `--project`/`--env` and their environment variables, without the link.
    pub(crate) fn flags(matches: &ArgMatches) -> Result<Self, Error> {
        Ok(Self {
            project: named(matches, "project", ProjectName::parse)?,
            environment: named(matches, "env", EnvironmentName::parse)?,
            link: None,
        })
    }

    /// `PLOYZ_PROJECT` and `PLOYZ_ENV` alone, for shell completion, which sees no flags.
    pub(crate) fn from_env() -> Result<Self, Error> {
        Ok(Self {
            project: env_named(env::PROJECT, ProjectName::parse)?,
            environment: env_named(env::ENVIRONMENT, EnvironmentName::parse)?,
            link: None,
        })
    }

    /// The `project` and `environment` a Cloud-published command was given, then
    /// `PLOYZ_PROJECT`/`PLOYZ_ENV`.
    pub(crate) fn given(project: Option<&str>, environment: Option<&str>) -> Result<Self, Error> {
        Ok(Self {
            project: given_or_env(project, env::PROJECT, ProjectName::parse)?,
            environment: given_or_env(environment, env::ENVIRONMENT, EnvironmentName::parse)?,
            link: None,
        })
    }

    /// [`Self::linked`], refusing a link made in another Organization than this device
    /// acts in.
    pub(crate) fn linked_here(self, config: &Path) -> Result<Self, Error> {
        self.linked(config, |directory, link| {
            check_organization(config, directory, link)
        })
    }

    /// [`Self::linked`] for a caller whose credential acts in the Organization with ID
    /// `acting`, such as `ployz mcp`'s agent token, rather than in this device's sign-in.
    pub(crate) fn linked_in(self, config: &Path, acting: &str) -> Result<Self, Error> {
        self.linked(config, |directory, link| {
            check_acting_in(acting, directory, link)
        })
    }

    /// Fill what flags and environment variables left open from the directory link,
    /// once `check` accepts the link's Organization.
    fn linked(
        mut self,
        config: &Path,
        check: impl FnOnce(&str, &Link) -> Result<(), Error>,
    ) -> Result<Self, Error> {
        if self.project.is_some() && self.environment.is_some() {
            return Ok(self);
        }
        let Some((directory, link)) = find(config)? else {
            return Ok(self);
        };
        // Another Project leaves the link nothing to supply, so its Organization doesn't matter.
        if self
            .project
            .as_ref()
            .is_some_and(|named| named.name != link.project)
        {
            return Ok(self);
        }
        check(&directory, &link)?;
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

/// Refuse a `PLOYZ_PROJECT` or `PLOYZ_ENV` that names nothing, as [`Scope::given`]
/// would, before the command it scopes is known. A typed name skips its variable.
pub(crate) fn check_env(typed_project: bool, typed_environment: bool) -> Result<(), Error> {
    if !typed_project {
        env_named(env::PROJECT, ProjectName::parse)?;
    }
    if !typed_environment {
        env_named(env::ENVIRONMENT, EnvironmentName::parse)?;
    }
    Ok(())
}

/// `given`, else the environment variable `var`, read only when nothing was given so
/// a bad variable never blocks a typed name.
fn given_or_env<T>(
    given: Option<&str>,
    var: &str,
    parse: fn(String) -> Result<T, ployz_core::RpcError>,
) -> Result<Option<Named<T>>, Error> {
    let Some(name) = given else {
        return env_named(var, parse);
    };
    Ok(Some(Named {
        name: parse(name.to_owned())?,
        source: Source::Flag,
    }))
}

fn env_named<T>(
    var: &str,
    parse: fn(String) -> Result<T, ployz_core::RpcError>,
) -> Result<Option<Named<T>>, Error> {
    let Some(name) = std::env::var_os(var) else {
        return Ok(None);
    };
    // A set name that isn't UTF-8 names nothing, so it is refused as an empty one is.
    let name = name.into_string().unwrap_or_default();
    Ok(Some(Named {
        name: parse(name)?,
        source: Source::Env,
    }))
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
pub(crate) struct Link {
    /// The Organization it was linked in; unknown under `PLOYZ_TOKEN`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) organization: Option<Organization>,
    pub(crate) project: ProjectName,
    pub(crate) environment: EnvironmentName,
}

fn links_path(config: &Path) -> PathBuf {
    crate::config_dir::ConfigDir::new(config).links()
}

/// Every link, by canonical directory.
pub(crate) fn load(config: &Path) -> Result<BTreeMap<String, Link>, Error> {
    let path = links_path(config);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            Error::caused(
                RpcErrorCode::Internal,
                format!(
                    "Could not read directory links {}. Delete it, then run ployz link again in each directory you linked.",
                    path.display()
                ),
                error,
            )
            .hint(Hint::Next(format!(
                "rm {}",
                shell_words::quote(&path.to_string_lossy())
            )))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error.into()),
    }
}

// ponytail: whole-file rewrite; two `link`s racing can lose one. Lock the file if that bites.
pub(crate) fn save(config: &Path, links: &BTreeMap<String, Link>) -> Result<(), Error> {
    let contents = serde_json::to_vec_pretty(links)?;
    crate::atomic_file::replace(
        &links_path(config),
        &contents,
        crate::atomic_file::OWNER_ONLY,
    )
    .map_err(std::io::Error::from)?;
    Ok(())
}

pub(crate) fn here() -> Result<String, Error> {
    std::env::current_dir()?
        .canonicalize()?
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::usage("cannot link a directory whose path is not UTF-8"))
}

/// The link of this directory or its nearest linked ancestor.
pub(crate) fn find(config: &Path) -> Result<Option<(String, Link)>, Error> {
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
pub(crate) fn acting_organization(config: &Path) -> Result<Option<Organization>, Error> {
    if local_mode() {
        return Ok(Some(Organization {
            id: LOCAL_ORGANIZATION.to_owned(),
            slug: LOCAL_ORGANIZATION.to_owned(),
        }));
    }
    // ponytail: an Organization Token's Organization is only known to Cloud, so a link
    // is not checked against it; ask `GET organizations` here if tokens meet links.
    if crate::cloud::account::env_token().is_some() {
        return Ok(None);
    }
    Ok(CredentialStore::beside(config).organization()?)
}

/// A link made in another Organization never addresses this one's Project of the same name.
fn check_organization(config: &Path, directory: &str, link: &Link) -> Result<(), Error> {
    let (Some(linked), Some(acting)) = (&link.organization, acting_organization(config)?) else {
        return Ok(());
    };
    linked_in_acting(directory, linked, &acting)
}

/// Refuse a link made in another Organization than `acting`. One with the same
/// name but another ID was replaced, so switching to it by name never helps.
fn linked_in_acting(
    directory: &str,
    linked: &Organization,
    acting: &Organization,
) -> Result<(), Error> {
    if linked.id == acting.id {
        return Ok(());
    }
    let local = |organization: &Organization| organization.id == LOCAL_ORGANIZATION;
    let replaced = linked.slug == acting.slug;
    let next = if local(linked) || local(acting) || replaced {
        "ployz link".to_owned()
    } else {
        format!("ployz org use {}", linked.slug)
    };
    let message = if replaced {
        format!(
            "{directory} is linked in an earlier Organization {}, which another Organization of that name replaced. Link it again",
            linked.slug
        )
    } else {
        format!(
            "{directory} is linked in Organization {}, but you are acting in {}",
            linked.slug, acting.slug
        )
    };
    Err(Error::detailed(
        RpcErrorCode::Conflict,
        message,
        json!({ "link": directory, "linked": linked.slug, "acting": acting.slug }),
    )
    .hint(Hint::Next(next)))
}

/// A link made in another Organization than `acting` would send its names to the
/// wrong Organization, so it stops the call.
fn check_acting_in(acting: &str, directory: &str, link: &Link) -> Result<(), Error> {
    let Some(linked) = &link.organization else {
        return Ok(());
    };
    if linked.id == acting {
        return Ok(());
    }
    let next = if linked.id == LOCAL_ORGANIZATION {
        format!("Link {directory} again.")
    } else {
        format!(
            "Run `ployz org use {}`, then `ployz mcp login`, or link {directory} again.",
            linked.slug
        )
    };
    Err(Error::detailed(
        RpcErrorCode::Conflict,
        format!(
            "{directory} is linked in Organization {}, but `ployz mcp` acts in another \
             Organization. {next}",
            linked.slug
        ),
        json!({ "link": directory, "linked": linked.slug }),
    )
    .hint(Hint::Next(next)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linked_in(id: &str, slug: &str) -> Link {
        Link {
            organization: Some(Organization {
                id: id.to_owned(),
                slug: slug.to_owned(),
            }),
            project: ProjectName::parse("shop".to_owned()).unwrap(),
            environment: EnvironmentName::parse("production".to_owned()).unwrap(),
        }
    }

    #[test]
    fn unreadable_links_name_the_file_and_how_to_start_over() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        std::fs::write(links_path(&config), "[[[").unwrap();
        let error = load(&config).unwrap_err();
        assert!(
            error
                .to_string()
                .ends_with("Delete it, then run ployz link again in each directory you linked."),
            "{error}"
        );
        assert_eq!(
            error.hints(),
            [Hint::Next(format!(
                "rm {}",
                shell_words::quote(&links_path(&config).to_string_lossy())
            ))]
        );
    }

    #[test]
    fn a_link_from_another_organization_than_the_credential_s_stops_the_call() {
        let link = linked_in("org_globex", "globex");
        let refused = check_acting_in("org_acme", "/src/shop", &link).unwrap_err();
        let message = refused.to_string();
        assert!(
            message.contains("linked in Organization globex"),
            "{message}"
        );
        assert!(message.contains("ployz org use globex"), "{message}");
        check_acting_in("org_globex", "/src/shop", &link).unwrap();
    }

    #[test]
    fn a_link_to_a_replaced_organization_of_the_same_name_says_link_again() {
        let acting = Organization {
            id: "org_new".to_owned(),
            slug: "ada".to_owned(),
        };
        let linked = Organization {
            id: "org_old".to_owned(),
            ..acting.clone()
        };
        let refused = linked_in_acting("/src/shop", &linked, &acting).unwrap_err();
        let message = refused.to_string();
        assert!(message.contains("earlier Organization ada"), "{message}");
        assert_eq!(refused.hints(), [Hint::Next("ployz link".to_owned())]);
        let other = Organization {
            id: "org_globex".to_owned(),
            slug: "globex".to_owned(),
        };
        let refused = linked_in_acting("/src/shop", &other, &acting).unwrap_err();
        assert_eq!(
            refused.hints(),
            [Hint::Next("ployz org use globex".to_owned())]
        );
        linked_in_acting("/src/shop", &acting, &acting).unwrap();
    }

    #[test]
    fn a_link_that_names_no_organization_is_not_checked() {
        let link = Link {
            organization: None,
            ..linked_in("org_globex", "globex")
        };
        check_acting_in("org_acme", "/src/shop", &link).unwrap();
    }
}
