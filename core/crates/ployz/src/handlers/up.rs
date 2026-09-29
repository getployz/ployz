//! `ployz up`: from the directory it runs in to a running app. It adds a Server
//! first when asked (the Organization's first founds its Cluster), creates a
//! Project named after the directory unless one is linked or named, links the
//! directory, gives an Environment without Services one (with a generated domain
//! on Cloud), then uploads the directory, deploys it and prints its URLs.

use std::path::Path;

use clap::{ArgMatches, Command, ValueHint};
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    AddDomain, CreateProject, CreateService, DeploymentView, DomainName, DomainsQuery,
    EnvironmentId, EnvironmentRef, ProjectId, ProjectName, ServiceId, ServicesQuery,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::deploy::{Request, open_events, say_view, upload_and_ship};
use super::store::{failed, mint, scoped, store};
use super::{Error, config_path, leaf_matches};
use crate::cli::{base, value};
use crate::cloud_account::StoreCallError;
use crate::failure::USAGE_EXIT;
use crate::output::say;

pub(crate) fn command() -> Command {
    super::deploy::following(scoped(base(
        "up",
        "Deploy this directory: create and link its Project if needed, upload, build and deploy it",
    )))
    .arg(
        value("server", None)
            .value_name("USER@HOST")
            .value_hint(ValueHint::Hostname)
            .help("First add this Server over SSH, as `ployz server add` does; the first founds the Cluster"),
    )
}

#[derive(Serialize)]
struct Up<'a> {
    directory: &'a str,
    /// The Server `--server` added, as `ployz server add` reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    server: Option<Value>,
    deployment: &'a DeploymentView,
    /// Where the app answers over HTTPS, once Cloud named its hostnames.
    urls: Vec<String>,
    /// The Environment in Cloud's dashboard.
    #[serde(skip_serializing_if = "Option::is_none")]
    dashboard: Option<String>,
    next: &'a str,
}

pub(super) fn up(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let config = config_path(matches)?;
    let events = open_events(matches)?;
    let directory = std::env::current_dir()?.canonicalize()?;
    let server = matches
        .get_one::<String>("server")
        .map(|destination| add_server(matches, &config, destination))
        .transpose()?
        .flatten();
    let store = store(root)?;
    let scope = super::link::scope(matches)?.at();
    let name = directory_name(&directory);
    let environment = match scope.project {
        Some(_) => scope,
        None => found_project(&store, &name, scope.environment)?,
    };
    let listed = store
        .services(&ServicesQuery { environment })
        .map_err(failed(matches, &["up"]))?;
    let linked = super::link::record(&config, listed.environment.clone())?;
    let environment = EnvironmentRef {
        project: Some(listed.environment.project.clone()),
        environment: Some(listed.environment.name.clone()),
    };
    if listed.services.is_empty() {
        add_service(matches, &store, &environment, name)?;
    }
    let identity = super::link::identity(&store)?;
    let shipped = upload_and_ship(
        matches,
        &store,
        Request {
            environment: environment.clone(),
            services: Vec::new(),
            version: None,
            source: Some(directory),
            accept: Vec::new(),
        },
        events,
    )?;
    let view = &shipped.view;
    // ponytail: the Deployment already ran, so a failed read only leaves the URLs out.
    let urls = store
        .domains(&DomainsQuery {
            environment,
            service: None,
        })
        .map(|domains| {
            domains
                .domains
                .into_iter()
                .filter_map(|row| match row.domain.name {
                    DomainName::Generated { hostname, .. } => hostname,
                    DomainName::Custom { hostname } => Some(hostname.as_str().to_owned()),
                })
                .map(|hostname| format!("https://{hostname}"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let dashboard = identity
        .cloud
        .zip(identity.organization)
        .map(|(cloud, organization)| {
            format!(
                "{cloud}/cloud/{}/{}/{}",
                organization.slug, view.environment.project, view.namespace
            )
        });
    let up = Up {
        directory: &linked.directory,
        server,
        deployment: view,
        urls,
        dashboard,
        next: &shipped.hint,
    };
    crate::output::finish(&up, || {
        say_view(view);
        for url in &up.urls {
            say!("Open {url}");
        }
        if let Some(dashboard) = &up.dashboard {
            say!("Dashboard: {dashboard}");
        }
    })?;
    shipped.ran
}

/// Run `ployz server add DESTINATION` as this command's first step.
fn add_server(
    matches: &ArgMatches,
    config: &Path,
    destination: &str,
) -> Result<Option<Value>, Error> {
    let config = config.to_string_lossy();
    let timeout = crate::cli::ssh_timeout(matches).as_secs().to_string();
    let args = [
        "ployz",
        "--ployz-config",
        &config,
        "--ssh-timeout",
        &timeout,
        "server",
        "add",
        "--",
        destination,
    ];
    let add = crate::cli::command()
        .try_get_matches_from(args)
        .map_err(|error| Error::usage(error.render().to_string()).with_exit(USAGE_EXIT))?;
    let (handler, _) = super::handler_for("server add").expect("server add has a handler");
    let (added, server) = crate::output::captured(|| handler(&add));
    added?;
    Ok(server)
}

/// A new Project named after the directory, with its Default Environment unless
/// `--env` names another. A taken name says how to deploy into that Project instead.
fn found_project(
    store: &super::store::Store,
    name: &ServiceName,
    environment: Option<ployz_store::EnvironmentName>,
) -> Result<EnvironmentRef, Error> {
    let create = CreateProject {
        id: ProjectId::parse(mint())?,
        name: ProjectName::parse(name.as_str().to_owned())?,
        default_environment: EnvironmentId::parse(mint())?,
    };
    let created = store.create_project(&create).map_err(|mut error| {
        if let StoreCallError::Refused(refusal) = &mut error
            && refusal.code == RpcErrorCode::Conflict
            && let Some(details) = refusal.details.as_object_mut()
        {
            let next = shell_words::join(["ployz", "up", "--project", name.as_str()]);
            details.insert("next".into(), json!(next));
        }
        Error::from(error)
    })?;
    say!("Created Project {}.", created.project.name);
    Ok(EnvironmentRef {
        project: Some(created.project.name),
        environment,
    })
}

/// The Service the upload builds, named like the directory, and on Cloud a
/// generated domain for it.
fn add_service(
    matches: &ArgMatches,
    store: &super::store::Store,
    environment: &EnvironmentRef,
    name: ServiceName,
) -> Result<(), Error> {
    store
        .create_service(&CreateService {
            id: ServiceId::parse(mint())?,
            environment: environment.clone(),
            name: name.clone(),
            image: None,
        })
        .map_err(failed(matches, &["up"]))?;
    say!("Added Service {name}.");
    // The hidden local Store has no Cluster Domain to generate one under.
    if store.local().is_none() {
        store
            .add_domain(&AddDomain {
                environment: environment.clone(),
                service: name,
                hostname: None,
                port: None,
            })
            .map_err(failed(matches, &["up"]))?;
    }
    Ok(())
}

/// The directory's name as a Project and Service name, else `app`.
fn directory_name(directory: &Path) -> ServiceName {
    let lowered = directory
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .replace(|c: char| !c.is_ascii_alphanumeric(), "-");
    let name = lowered
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    ServiceName::parse(name.as_str())
        .or_else(|_| ServiceName::parse("app"))
        .expect("app is a Service name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_names_its_project_or_falls_back_to_app() {
        for (directory, name) in [
            ("/src/my_Shop.v2", "my-shop-v2"),
            ("/src/--web--", "web"),
            ("/src/___", "app"),
            ("/", "app"),
        ] {
            assert_eq!(directory_name(Path::new(directory)).as_str(), name);
        }
    }
}
