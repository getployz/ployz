//! Authored Services in the Config Store: add, list, inspect, rename and remove.
//! Every change is staged in Working State until a Deploy ships it.

use clap::{ArgMatches, Command};
use ployz_core::ServiceName;
use ployz_store::{
    CreateGitService, CreateService, RemoveService, RenameService, ServiceId, ServiceQuery,
    ServiceStaged, ServicesQuery,
};

use super::super::store::{self, Next};
use super::super::{Error, leaf_matches, required};
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
    let id = ServiceId::parse(store::mint())?;
    let environment = store::environment(matches)?;
    let name = service_name(matches, "name")?;
    let store = store::store(root)?;
    let created = if let Some(repo) = matches.get_one::<String>("repo") {
        let (repository, branch) = match repo.split_once('@') {
            Some((repository, branch)) => {
                (repository, Some(ployz_store::BranchName::parse(branch)?))
            }
            None => (repo.as_str(), None),
        };
        let words = ["service", "add", name.as_str(), "--repo", "OWNER/REPO"];
        store
            .create_git_service(&CreateGitService {
                id,
                environment,
                name: name.clone(),
                repository: ployz_store::RepositoryName::parse(repository)?,
                branch,
            })
            .map_err(store::failed(matches, &words))?
    } else {
        let image = matches.get_one::<String>("image").cloned();
        let mut words = vec!["service", "add", name.as_str()];
        if image.is_some() {
            words.extend(["--image", "REF"]);
        }
        store
            .create_service(&CreateService {
                id,
                environment,
                name: name.clone(),
                image,
            })
            .map_err(store::failed(matches, &words))?
    };
    staged(matches, &created, "Staged new Service")
}

/// List Working State's Services, and deployed ones a removal dropped from it.
pub(super) fn list(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let query = ServicesQuery {
        environment: store::environment(matches)?,
    };
    let view = store::store(root)?
        .services(&query)
        .map_err(store::failed(matches, &["service", "ls"]))?;
    output::finish(&view, || {
        say!("SERVICE\tPRIVATE DNS\tSOURCE\tNEXT DEPLOY");
        for listing in &view.services {
            say!(
                "{}\t{}\t{}\t{}",
                listing.service.name,
                listing.service.private_dns,
                json_word(&listing.source),
                listing.change.as_ref().map_or("-".to_owned(), json_word)
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
    let view = store::store(root)?.service(&query).map_err(store::failed(
        matches,
        &["service", "inspect", query.service.as_str()],
    ))?;
    output::finish(&view, || {
        let service = &view.service.service;
        say!("Service {} ({})", service.name, service.id);
        say!("Private DNS: {}", service.private_dns);
        say!("Source: {}", json_word(&view.service.source));
        if let Some(change) = &view.service.change {
            say!("Next Deploy: {}", json_word(change));
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
    let words = [
        "service",
        "rename",
        rename.service.as_str(),
        rename.name.as_str(),
    ];
    let renamed = store::store(root)?
        .rename_service(&rename)
        .map_err(store::failed(matches, &words))?;
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
        .remove_service(&remove)
        .map_err(store::failed(
            matches,
            &["service", "rm", remove.service.as_str()],
        ))?;
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

fn service_name(matches: &ArgMatches, arg: &str) -> Result<ServiceName, Error> {
    // Core's name error quotes the value; a rejected value is never echoed.
    ServiceName::parse(required(matches, arg)?).map_err(|_| {
        Error::usage("Expected a Service name: lowercase letters, digits and -, like web")
    })
}

/// A unit enum variant as the word its JSON uses.
fn json_word(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}
