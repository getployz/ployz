//! `ployz up`: from the directory it runs in to a running app. It adds a Server
//! first when asked (the Organization's first founds its Cluster), creates a
//! Project named after the directory unless one is linked or named, links the
//! directory, gives an Environment with no Service to build the upload one (with a
//! generated domain on Cloud), then uploads the directory, deploys it and prints its URLs.

use std::path::Path;

use clap::{ArgMatches, Command, ValueHint};
use ployz_core::{DockerfileFacts, RpcErrorCode, ServiceName};
use ployz_store::{
    AddDomain, Change, CreateProject, CreateService, DeploymentView, DomainName, DomainsQuery,
    Edit, EnvironmentId, EnvironmentRef, ProjectId, ProjectName, ServiceLineageId, ServiceListing,
    ServicesQuery, SettingPath, SourceKind,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::deploy::{Request, open_events, say_view, upload_and_ship};
use super::store::{mint, scoped, store};
use super::{Error, config_path, leaf_matches};
use crate::cli::{base, value};

pub(super) fn command(command: Command) -> Command {
    super::deploy::following(scoped(base(command, "Deploy this directory: create and link its Project if needed, upload, build and deploy it",
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
    .arg(crate::cli::reviewed_version())
    .arg(crate::cli::approval())
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
    let directory = std::env::current_dir()?.canonicalize()?;
    refuse_empty(&directory)?;
    let events = open_events(matches)?;
    let server = matches
        .get_one::<String>("server")
        .map(|destination| add_server(matches, &config, destination))
        .transpose()?
        .flatten();
    let store = store(root)?.args(super::deploy::following_args(matches));
    let scope = super::link::scope(matches)?.at();
    let name = directory_name(&directory);
    let environment = match scope.project {
        Some(_) => scope,
        None => found_project(&store, &name, scope.environment)?,
    };
    let listed = store.read(&ServicesQuery { environment })?;
    // Refuse before linking, so a refused directory stays as it was.
    let added = service_for_upload(&listed.services, name)?;
    let linked = super::link::record_unless_linked(&config, listed.environment.clone())?;
    let environment = EnvironmentRef {
        project: Some(listed.environment.project.clone()),
        environment: Some(listed.environment.name.clone()),
    };
    if let Some(name) = added {
        add_service(&store, &environment, name, &directory)?;
    }
    let identity = super::link::identity(&store)?;
    let shipped = upload_and_ship(
        matches,
        &store,
        Request {
            environment: environment.clone(),
            services: Vec::new(),
            version: matches.get_one::<String>("expect-version").cloned(),
            source: Some(directory),
            accept: super::teardown::accepted(matches)?,
            message: None,
            no_prune: false,
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
    crate::ui::finish(&up, || {
        if !shipped.presented {
            say_view(view);
        }
        for url in opens(&up.urls, &shipped.ran) {
            crate::ui::stream(format_args!("Open {url}"));
        }
        if let Some(dashboard) = &up.dashboard {
            crate::ui::stream(format_args!("Dashboard: {dashboard}"));
        }
    })?;
    shipped.ran
}

/// The URLs to say `Open` for: none when the Deployment failed, since it left the app as
/// it was, or not running at all.
fn opens<'u>(urls: &'u [String], ran: &Result<(), Error>) -> &'u [String] {
    if ran.is_ok() { urls } else { &[] }
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
        .map_err(|error| super::clap_usage(&error))?;
    let (added, server) = crate::ui::captured(|| (super::verbs::SERVER_ADD.run)(&add));
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
    let created = store.try_write(&create).map_err(|error| {
        store.fail(super::store::with_next(
            error,
            |refusal| refusal.code == RpcErrorCode::Conflict,
            || shell_words::join(["ployz", "up", "--project", name.as_str()]),
        ))
    })?;
    crate::ui::note(format_args!("Created Project {}.", created.project.name));
    Ok(EnvironmentRef {
        project: Some(created.project.name),
        environment,
    })
}

/// The Service the upload builds, named like the directory, and on Cloud a
/// generated domain for it. A root `Dockerfile` builds it, and its first `EXPOSE`d
/// port is the domain's.
fn add_service(
    store: &super::store::Store,
    environment: &EnvironmentRef,
    name: ServiceName,
    directory: &Path,
) -> Result<(), Error> {
    store.write(&CreateService {
        id: ServiceLineageId::parse(mint())?,
        environment: environment.clone(),
        name: name.clone(),
        image: None,
        template: None,
    })?;
    crate::ui::note(format_args!("Added Service {name}."));
    let dockerfile = std::fs::read_to_string(directory.join("Dockerfile")).ok();
    if dockerfile.is_some() {
        store.write(&Edit {
            environment: environment.clone(),
            expect: None,
            changes: vec![Change::Set {
                path: SettingPath::parse(&format!("{name}.buildMethod"))?,
                value: json!("dockerfile"),
            }],
        })?;
    }
    // The hidden local Store has no Cluster Domain to generate one under.
    if store.local().is_none() {
        store.write(&AddDomain {
            environment: environment.clone(),
            service: name,
            hostname: None,
            prefix: None,
            port: dockerfile
                .as_deref()
                .and_then(|dockerfile| DockerfileFacts::parse(dockerfile).ports.first().copied()),
        })?;
    }
    Ok(())
}

/// The Service to add so the upload builds: none when one already builds from an
/// upload or has no source yet, else `name`. An Environment with a Git Service, or a
/// Service already named `name`, is refused: the upload would build nothing, and a new
/// Service would deploy a second copy of an app Git already ships.
fn service_for_upload(
    services: &[ServiceListing],
    name: ServiceName,
) -> Result<Option<ServiceName>, Error> {
    if services
        .iter()
        .any(|listed| matches!(listed.source, SourceKind::Uploaded | SourceKind::Empty))
    {
        return Ok(None);
    }
    if let Some(git) = services
        .iter()
        .find(|listed| listed.source == SourceKind::Git)
    {
        return Err(Error::usage(format!(
            "Nothing would build this directory: Service {} builds from Git, and no Service builds from an upload. Run ployz deploy to ship from Git, or add a Service with `ployz service add <name>` and no --image, then run ployz up again.",
            git.service.name
        )));
    }
    if services.iter().any(|listed| listed.service.name == name) {
        return Err(Error::usage(format!(
            "Nothing would build this directory: Service {name} pulls an image, and no Service builds from an upload. Add one with `ployz service add <name>` and no --image, then run ployz up again."
        )));
    }
    Ok(Some(name))
}

/// Refuse a directory with nothing to upload before anything is created.
fn refuse_empty(directory: &Path) -> Result<(), Error> {
    let listing = crate::build::UploadListing::read(directory)
        .map_err(super::deploy::unreadable(directory))?;
    if listing.is_empty() {
        return Err(Error::usage(format!(
            "Nothing to deploy: {} is empty. Run ployz up in your app's directory.",
            directory.display()
        )));
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
    fn a_failed_deployment_says_to_open_nothing() {
        let urls = vec!["https://web.example.com".to_owned()];
        assert_eq!(opens(&urls, &Ok(())), urls.as_slice());
        assert!(opens(&urls, &Err(Error::cancelled())).is_empty());
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

    fn listed(name: &str, source: SourceKind) -> ServiceListing {
        serde_json::from_value(json!({
            "id": "00000000-0000-4000-8000-000000000003",
            "name": name,
            "private_dns": name,
            "row": "00000000-0000-4000-8000-000000000003:node",
            "source": source,
            "change": null,
            "template": null,
        }))
        .unwrap()
    }

    #[test]
    fn an_existing_project_without_an_uploaded_service_gets_one() {
        // #2863: a Project holding only an image Service built nothing and said "Nothing changed".
        let name = || ServiceName::parse("my-app").unwrap();
        assert_eq!(service_for_upload(&[], name()).unwrap(), Some(name()));
        assert_eq!(
            service_for_upload(&[listed("db", SourceKind::Image)], name()).unwrap(),
            Some(name())
        );
        // A Git Service already ships this app; a second, uploaded copy is refused.
        assert!(service_for_upload(&[listed("api", SourceKind::Git)], name()).is_err());
        for source in [SourceKind::Uploaded, SourceKind::Empty] {
            let services = [listed("db", SourceKind::Image), listed("web", source)];
            assert_eq!(service_for_upload(&services, name()).unwrap(), None);
        }
    }

    #[test]
    fn a_taken_name_that_builds_nothing_from_the_upload_is_refused() {
        let error = service_for_upload(
            &[listed("my-app", SourceKind::Image)],
            ServiceName::parse("my-app").unwrap(),
        )
        .unwrap_err();
        assert_eq!(error.report().code, RpcErrorCode::InvalidArgument);
        assert!(error.to_string().contains("ployz service add"), "{error}");
    }

    #[test]
    fn an_empty_directory_is_refused_before_anything_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let error = refuse_empty(dir.path()).unwrap_err();
        assert_eq!(error.report().code, RpcErrorCode::InvalidArgument);
        assert!(
            error.to_string().starts_with("Nothing to deploy: "),
            "{error}"
        );
        std::fs::write(dir.path().join("index.html"), "hi").unwrap();
        refuse_empty(dir.path()).unwrap();
    }
}
