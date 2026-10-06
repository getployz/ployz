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
use crate::ui::{Cell, Fields, Hint, Table, Tone};

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
    let store = store::store(root)?;
    let created = if let Some(repo) = matches.get_one::<String>("repo") {
        let (repository, branch) = match repo.split_once('@') {
            Some((repository, branch)) => {
                (repository, Some(ployz_store::BranchName::parse(branch)?))
            }
            None => (repo.as_str(), None),
        };
        store.write(&CreateGitService {
            id,
            environment,
            name: name.clone(),
            repository: ployz_store::RepositoryName::parse(repository)?,
            branch,
        })?
    } else {
        let image = matches.get_one::<String>("image").cloned();
        store.write(&CreateService {
            id,
            environment,
            name: name.clone(),
            image,
            template: None,
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
    let mut table = Table::new(
        ["SERVICE", "PRIVATE DNS", "SOURCE", "NEXT DEPLOY"],
        format!("No Services in {} yet.", view.environment.name),
    );
    for listing in &view.services {
        table.row([
            Cell::from(listing.service.name.to_string()),
            Cell::from(listing.service.private_dns.to_string()),
            Cell::from(store::word(&listing.source)),
            listing.change.as_ref().map_or_else(
                || Cell::from(""),
                |change| Cell::status(store::word(change), Tone::Change),
            ),
        ]);
    }
    crate::ui::list(&view, &table)?;
    if view.services.is_empty() {
        crate::ui::hint(&Hint::Next(store::next(
            matches,
            &["service", "add", "NAME", "--image", "REF"],
        )));
    }
    Ok(())
}

/// Show one Service: its identity, Settings and staged changes.
pub(super) fn inspect(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let query = ServiceQuery {
        environment: store::environment(matches)?,
        service: service_name(matches, "service")?,
    };
    let view = store::store(root)?.read(&query)?;
    let service = &view.service.service;
    let mut record = Fields::new()
        .field("service", &service.name)
        .field("private dns", &service.private_dns)
        .field("source", store::word(&view.service.source));
    if let Some(change) = &view.service.change {
        record.push("next deploy", store::word(change));
    }
    for (setting, value) in &view.values {
        record.push(setting.to_string(), value);
    }
    for change in &view.changes {
        record.push(
            "staged",
            format_args!(
                "{}: {} -> {}",
                change.path,
                store::shown(&change.before),
                store::shown(&change.after)
            ),
        );
    }
    crate::ui::fields(&view, &record)
}

/// Rename a Service in Working State.
pub(super) fn rename(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let rename = RenameService {
        environment: store::environment(matches)?,
        service: service_name(matches, "service")?,
        name: service_name(matches, "name")?,
    };
    let renamed = store::store(root)?.write(&rename)?;
    staged(matches, &renamed, "Staged rename of Service")
}

/// Remove a Service from Working State; the next Deploy removes it from the Servers.
pub(super) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let remove = RemoveService {
        environment: store::environment(matches)?,
        service: service_name(matches, "service")?,
    };
    let removed = store::store(root)?.write(&remove)?;
    staged(matches, &removed, "Staged removal of Service")
}

/// A staged Service change and, when it changed anything, `ployz deploy` to ship it.
fn staged(matches: &ArgMatches, result: &ServiceStaged, what: &str) -> Result<(), Error> {
    let hint = (!result.staged.is_empty()).then(|| store::next(matches, &["deploy"]));
    crate::ui::finish(&Next::new(result, hint.clone()), || {
        // A mutation always says what it did, nothing included.
        if result.staged.is_empty() {
            crate::ui::stream(format_args!(
                "Nothing changed in {}/{}; {} already has that name.",
                result.environment.project, result.environment.name, result.service.name
            ));
            return;
        }
        crate::ui::stream(format_args!(
            "{what} {} in {}/{} (revision {}).",
            result.service.name,
            result.environment.project,
            result.environment.name,
            result.environment.revision
        ));
        if let Some(hint) = hint.clone() {
            crate::ui::hint(&Hint::Next(hint));
        }
    })
}

use crate::handlers::store::service_name;
