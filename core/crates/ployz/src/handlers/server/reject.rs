//! `server reject`: turn away the waiting servers a glob matches. Their check-in loop stops
//! and the same install never comes back.

use std::io::IsTerminal;

use clap::{ArgMatches, Command};
use globset::Glob;
use ployz_core::RpcErrorCode;
use serde_json::json;

use super::waiting::{self, Waiting};
use crate::cli::{base, env, positional, switch};
use crate::handlers::{Error, leaf_matches, runtime, typed_with};
use crate::ui::{self, Cell, Hint, Table};

pub(crate) fn command(command: Command) -> Command {
    base(command, "Reject servers waiting to join")
        .long_about(
            "Reject the servers waiting to join whose names match a glob, such as 'web-*'. \
             It lists the matches and asks before it rejects them; --yes skips the ask. \
             A rejected server stops checking in and the same install never comes back. \
             Outside a terminal without --yes it fails with confirmation_required.",
        )
        .arg(positional("pattern", true).help("A name or glob, such as 'web-*'"))
        .arg(switch("yes", Some('y')).env(env::AUTO_CONFIRM))
}

pub(crate) fn reject(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let pattern = matches
        .get_one::<String>("pattern")
        .ok_or_else(|| Error::usage("pattern is required"))?;
    let runtime = runtime()?;
    let credential = waiting::credential(&runtime, matches)?;
    let waiting = runtime.block_on(waiting::fetch(&credential))?;
    let matched = matching(&waiting, pattern)?;
    if matched.is_empty() {
        return Err(Error::not_found(format!(
            "No waiting server matches {pattern}. Nothing rejected."
        ))
        .hint(Hint::Next("ployz server ls --waiting".into())));
    }
    let names: Vec<&str> = matched.iter().map(|host| host.row.name.as_str()).collect();
    if !ui::json() {
        let mut table = Table::new(["NAME", "IP", "TOKEN"], "");
        for host in &matched {
            table.row([
                Cell::from(host.row.name.clone()),
                Cell::from(host.row.ip().to_owned()),
                Cell::from(host.row.fleet_token.clone()),
            ]);
        }
        let stderr = std::io::stderr();
        let _ = table.write(&mut anstream::stderr(), stderr.is_terminal());
    }
    if !matches.get_flag("yes") {
        let count = matched.len();
        ui::confirm(
            &format!("Reject {count} waiting server{}?", plural(count)),
            || {
                Error::detailed(
                    RpcErrorCode::ConfirmationRequired,
                    format!(
                        "Rejecting {count} waiting server{} needs confirmation. Nothing rejected.",
                        plural(count)
                    ),
                    json!({ "waiting_servers": names }),
                )
                .hint(Hint::Retry(typed_with(&["--yes"])))
            },
            "Nothing rejected.",
        )?;
    }
    let ids: Vec<&str> = matched.iter().map(|host| host.row.id.as_str()).collect();
    let rejected = runtime.block_on(waiting::reject(&credential, &ids))?;
    let sentence = match rejected {
        1 => "1 rejected. It won't come back.".to_owned(),
        count => format!("{count} rejected. They won't come back."),
    };
    ui::done(
        &json!({ "rejected": rejected, "waiting_servers": names }),
        sentence,
    )
}

/// The waiting servers whose name `pattern` matches, in the list's order.
fn matching<'a>(waiting: &'a [Waiting], pattern: &str) -> Result<Vec<&'a Waiting>, Error> {
    let glob = Glob::new(pattern)
        .map_err(|error| Error::usage(format!("{pattern} is not a valid glob: {}", error.kind())))?
        .compile_matcher();
    Ok(waiting
        .iter()
        .filter(|host| glob.is_match(&host.row.name))
        .collect())
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_takes_one_pattern_and_yes() {
        let parse = |args: &[&str]| {
            crate::cli::command()
                .try_get_matches_from(["ployz", "server", "reject"].iter().chain(args))
        };
        let matches = parse(&["web-*", "--yes"]).unwrap();
        let leaf = leaf_matches(&matches);
        assert_eq!(leaf.get_one::<String>("pattern").unwrap(), "web-*");
        assert!(leaf.get_flag("yes"));
        assert!(parse(&[]).is_err());
    }
}
