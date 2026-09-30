//! Authored Services in the Config Store: add, list, inspect, rename and remove.
//! Every change is staged in Working State until a Deploy ships it.

use clap::{ArgMatches, Command};
use ployz_store::{
    CreateGitService, CreateService, RemoveService, RenameService, ServiceLineageId, ServiceQuery,
    ServiceStaged, ServicesQuery,
};

use super::super::store::{self, Next};
use super::super::{Error, leaf_matches};
use crate::cli::{positional, value};
use crate::output::{self, say};

pub(super) fn add_command() -> Command {
    store::scoped(Command::new("add").about("Add a Service; it is staged until a Deploy"))
        .arg(positional("name", true).help("Service name, also its Private DNS name"))
        .arg(
            value("image", None)
                .value_name("REF")
                .help("Container image to run [default: none, an empty Service]"),
        )
        .arg(
            value("repo", None)
                .value_name("OWNER/REPO[@BRANCH]")
                .conflicts_with("image")
                .help("GitHub repository to build; Ployz checks it and the branch (default: its default branch)"),
        )
}

pub(super) fn ls_command() -> Command {
    store::scoped(Command::new("ls").about("List Services and what the next Deploy does to them"))
}

pub(super) fn inspect_command() -> Command {
    store::scoped(Command::new("inspect").about("Show a Service, its Settings and staged changes"))
        .arg(positional("service", true))
}

pub(super) fn rename_command() -> Command {
    store::scoped(
        Command::new("rename")
            .about("Rename a Service; its Private DNS name and references stay the same"),
    )
    .arg(positional("service", true))
    .arg(positional("name", true).help("The new name"))
}

pub(super) fn rm_command() -> Command {
    store::scoped(
        Command::new("rm").about("Remove a Service; it keeps running until a Deploy removes it"),
    )
    .arg(positional("service", true))
}

/// Add an image, GitHub repository or empty Service to Working State.
pub(super) fn add(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let id = ServiceLineageId::parse(store::mint())?;
    let environment = store::environment(matches)?;
    let name = service_name(matches, "name")?;
    let store = store::store(root)?.args([name.as_str()]);
    let created = if let Some(repo) = matches.get_one::<String>("repo") {
        let (repository, branch) = match repo.split_once('@') {
            Some((repository, branch)) => {
                (repository, Some(ployz_store::BranchName::parse(branch)?))
            }
            None => (repo.as_str(), None),
        };
        store
            .args(["--repo", "OWNER/REPO"])
            .write(&CreateGitService {
                id,
                environment,
                name: name.clone(),
                repository: ployz_store::RepositoryName::parse(repository)?,
                branch,
            })?
    } else {
        let image = matches.get_one::<String>("image").cloned();
        let flag = image.as_ref().map(|_| ["--image", "REF"]);
        store
            .args(flag.into_iter().flatten())
            .write(&CreateService {
                id,
                environment,
                name: name.clone(),
                image,
            })?
    };
    staged(matches, &created, "Staged new Service")
}

/// List Working State's Services, and deployed ones a removal dropped from it.
pub(super) fn list(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let query = ServicesQuery {
        environment: store::environment(matches)?,
    };
    let view = store::store(root)?.read(&query)?;
    output::finish(&view, || {
        say!("SERVICE\tPRIVATE DNS\tSOURCE\tNEXT DEPLOY");
        for listing in &view.services {
            say!(
                "{}\t{}\t{}\t{}",
                listing.service.name,
                listing.service.private_dns,
                crate::handlers::store::word(&listing.source),
                listing
                    .change
                    .as_ref()
                    .map_or("-".to_owned(), crate::handlers::store::word)
            );
        }
    })
}

/// Show one Service: its identity, Settings and staged changes.
pub(super) fn inspect(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let query = ServiceQuery {
        environment: store::environment(matches)?,
        service: service_name(matches, "service")?,
    };
    let view = store::store(root)?
        .args([query.service.as_str()])
        .read(&query)?;
    output::finish(&view, || {
        let service = &view.service.service;
        say!("Service {} ({})", service.name, service.id);
        say!("Private DNS: {}", service.private_dns);
        say!(
            "Source: {}",
            crate::handlers::store::word(&view.service.source)
        );
        if let Some(change) = &view.service.change {
            say!("Next Deploy: {}", crate::handlers::store::word(change));
        }
        for (setting, value) in &view.values {
            say!("{setting}={value}");
        }
        for change in &view.changes {
            say!(
                "staged {}: {} -> {}",
                change.path,
                change.before,
                change.after
            );
        }
    })
}

/// Rename a Service in Working State.
pub(super) fn rename(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let rename = RenameService {
        environment: store::environment(matches)?,
        service: service_name(matches, "service")?,
        name: service_name(matches, "name")?,
    };
    let renamed = store::store(root)?
        .args([rename.service.as_str(), rename.name.as_str()])
        .write(&rename)?;
    staged(matches, &renamed, "Staged rename of Service")
}

/// Remove a Service from Working State; the next Deploy removes it from the Servers.
pub(super) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let remove = RemoveService {
        environment: store::environment(matches)?,
        service: service_name(matches, "service")?,
    };
    let removed = store::store(root)?
        .args([remove.service.as_str()])
        .write(&remove)?;
    staged(matches, &removed, "Staged removal of Service")
}

/// A staged Service change and, when it changed anything, `ployz diff` to review it.
fn staged(matches: &ArgMatches, result: &ServiceStaged, what: &str) -> Result<(), Error> {
    let hint = (!result.staged.is_empty()).then(|| store::next(matches, &["diff"]));
    output::finish(&Next::new(result, hint), || {
        say!(
            "{what} {} in {}/{} (revision {}).",
            result.service.name,
            result.environment.project,
            result.environment.name,
            result.environment.revision
        );
    })
}

use crate::handlers::store::service_name;
