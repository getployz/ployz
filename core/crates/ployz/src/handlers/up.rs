//! `ployz up`: from the directory it runs in to a running app. It adds a Server
//! first when asked (the Organization's first founds its Cluster), creates a
//! Project named after the directory unless one is linked or named, links the
//! directory, gives an Environment without Services one (with a generated domain
//! on Cloud), then uploads the directory, deploys it and prints its URLs.

use std::path::Path;

use clap::{ArgMatches, Command, ValueHint};
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    AddDomain, Change, CreateProject, CreateService, DeploymentView, DomainName, DomainsQuery,
    Edit, EnvironmentId, EnvironmentRef, ProjectId, ProjectName, ServiceLineageId, ServicesQuery,
    SettingPath,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::deploy::{Request, open_events, say_view, upload_and_ship};
use super::store::{failed, mint, scoped, store};
use super::{Error, config_path, leaf_matches};
use crate::cli::{base, value};
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
    .arg(
        crate::cli::switch("reset", None)
            .requires("server")
            .help("Reset the --server if it already runs Ployz, before adding it"),
    )
    .arg(crate::cli::volume_acceptance())
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
        .read(&ServicesQuery { environment })
        .map_err(failed(matches, &["up"]))?;
    let linked = super::link::record_unless_linked(&config, listed.environment.clone())?;
    let environment = EnvironmentRef {
        project: Some(listed.environment.project.clone()),
        environment: Some(listed.environment.name.clone()),
    };
    if listed.services.is_empty() {
        add_service(matches, &store, &environment, name, &directory)?;
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
            accept: super::teardown::accepted(matches)?,
            message: None,
        },
        events,
    )?;
    let view = &shipped.view;
    // ponytail: the Deployment already ran, so a failed read only leaves the URLs out.
    let urls = store
        .read(&DomainsQuery {
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
                organization.slug, view.environment.project, view.environment.name
            )
        });
    let up = Up {
        directory: &linked,
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
    let mut args = vec![
        "ployz",
        "--ployz-config",
        &config,
        "--ssh-timeout",
        &timeout,
        "server",
        "add",
    ];
    // `up --reset` is its confirmation: `up` never prompts for one it can't take.
    if matches.get_flag("reset") {
        args.extend(["--reset", "--yes"]);
    }
    args.extend(["--", destination]);
    let add = crate::cli::command()
        .try_get_matches_from(args)
        .map_err(|error| Error::usage(error.render().to_string()).with_exit(USAGE_EXIT))?;
    let handler = super::handler_for("server add").expect("server add has a handler");
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
    let created = store.write(&create).map_err(|error| {
        Error::from(super::store::with_next(
            error,
            |refusal| refusal.code == RpcErrorCode::Conflict,
            || shell_words::join(["ployz", "up", "--project", name.as_str()]),
        ))
    })?;
    say!("Created Project {}.", created.project.name);
    Ok(EnvironmentRef {
        project: Some(created.project.name),
        environment,
    })
}

/// The Service the upload builds, named like the directory, and on Cloud a
/// generated domain for it. A root `Dockerfile` builds it, and its first `EXPOSE`d
/// port is the domain's.
fn add_service(
    matches: &ArgMatches,
    store: &super::store::Store,
    environment: &EnvironmentRef,
    name: ServiceName,
    directory: &Path,
) -> Result<(), Error> {
    store
        .write(&CreateService {
            id: ServiceLineageId::parse(mint())?,
            environment: environment.clone(),
            name: name.clone(),
            image: None,
        })
        .map_err(failed(matches, &["up"]))?;
    say!("Added Service {name}.");
    let dockerfile = std::fs::read_to_string(directory.join("Dockerfile")).ok();
    if dockerfile.is_some() {
        store
            .write(&Edit {
                environment: environment.clone(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse(&format!("{name}.buildMethod"))?,
                    value: json!("dockerfile"),
                }],
            })
            .map_err(failed(matches, &["up"]))?;
    }
    // The hidden local Store has no Cluster Domain to generate one under.
    if store.local().is_none() {
        store
            .write(&AddDomain {
                environment: environment.clone(),
                service: name,
                hostname: None,
                port: dockerfile.as_deref().and_then(exposed_port),
            })
            .map_err(failed(matches, &["up"]))?;
    }
    Ok(())
}

/// A Dockerfile's first `EXPOSE`d port.
fn exposed_port(dockerfile: &str) -> Option<u16> {
    dockerfile.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        if !words.next()?.eq_ignore_ascii_case("EXPOSE") {
            return None;
        }
        words.find_map(|port| port.split('/').next()?.parse().ok())
    })
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
    fn the_first_exposed_port_is_the_domains() {
        for (dockerfile, port) in [
            ("FROM nginx\nEXPOSE 80\nEXPOSE 443", Some(80)),
            ("FROM x\n  expose 3000/tcp 9000", Some(3000)),
            ("FROM x\nEXPOSE $PORT", None),
            ("FROM x", None),
        ] {
            assert_eq!(exposed_port(dockerfile), port, "{dockerfile}");
        }
    }

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
