//! `ployz docs`: search the user docs built into this binary, or print a page.

use clap::{Arg, ArgAction, ArgMatches, Command};
use ployz_store::docs::{self, Lookup};

use super::{Error, leaf_matches};
use crate::ui::{self, Hint};

pub(super) fn command(command: Command) -> Command {
    command.about("Search the Ployz docs, or print a page").arg(
        Arg::new("query")
            .required(true)
            .num_args(1..)
            .action(ArgAction::Append)
            .help("A question, or a page path ending in .md, for example services/domains.md"),
    )
}

pub(super) fn run(root: &ArgMatches) -> Result<(), Error> {
    let query = leaf_matches(root)
        .get_many::<String>("query")
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    match docs::lookup(&query) {
        Lookup::Search(found) => Ok(ui::finish(&found, || {
            if found.results.is_empty() {
                ui::note(format_args!("No docs match {query}."));
            }
            for (at, hit) in found.results.iter().enumerate() {
                if at > 0 {
                    ui::stream("");
                }
                ui::stream(hit.heading);
                ui::stream(format_args!("  {}#{}", hit.path, hit.anchor));
                ui::stream(format_args!("  {}", hit.excerpt));
            }
        })?),
        Lookup::Page(page) => Ok(ui::finish(&page, || ui::stream(&page.content))?),
        Lookup::Missing { path, closest } => Err(Error::not_found(format!("No docs page {path}"))
            .hint(closest.map(|page| Hint::Closest(page.to_owned())))
            .hint(Hint::valid(docs::pages()))),
    }
}
