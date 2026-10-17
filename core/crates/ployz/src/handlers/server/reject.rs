//! `server reject`: turn away the waiting servers a glob matches. Their check-in loop stops
//! and they won't come back under the same Fleet Token.

use std::io::IsTerminal;

use clap::{ArgMatches, Command};
use globset::Glob;
use ployz_core::RpcErrorCode;
use serde_json::json;

use super::waiting::{self, Rejection, Stopped, Waiting};
use crate::cli::{base, env, positional, switch};
use crate::handlers::{Error, leaf_matches, runtime, typed_with};
use crate::ui::{self, Cell, Hint, Table, printable};

pub(crate) fn command(command: Command) -> Command {
    base(command, "Reject servers waiting to join")
        .long_about(
            "Reject the servers waiting to join whose names match a glob, such as 'web-*', \
             or one by its id. \
             It lists the matches and asks before it rejects them; --yes skips the ask. \
             A rejected server stops checking in and won't come back under the same Fleet Token; \
             revoke the token to turn away every server that holds it. \
             Outside a terminal without --yes it fails with confirmation_required.",
        )
        .arg(
            positional("pattern", true)
                .help("A name, a glob such as 'web-*', or a waiting server's id"),
        )
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
            "No waiting server matches {}. Nothing rejected.",
            printable(pattern)
        ))
        .hint(Hint::Next("ployz server ls --waiting".into()))
        .hint(waiting::copied_hint(pattern)));
    }
    let listed: Vec<_> = matched
        .iter()
        .map(|host| {
            json!({
                "id": host.row.id,
                "name": host.row.name,
                "ip": host.row.ip(),
                "fleet_token": host.row.fleet_token,
            })
        })
        .collect();
    if !ui::json() {
        let mut table = Table::new(["ID", "NAME", "IP", "TOKEN"], "");
        for host in &matched {
            table.row([
                Cell::from(printable(ui::short_id(&host.row.id))),
                Cell::from(printable(&host.row.name)),
                Cell::from(printable(host.row.ip())),
                Cell::from(printable(&host.row.fleet_token)),
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
                    json!({ "waiting_servers": listed }),
                )
                .hint(Hint::Retry(typed_with(&["--yes"])))
            },
            "Nothing rejected.",
        )?;
    }
    let ids: Vec<&str> = matched.iter().map(|host| host.row.id.as_str()).collect();
    let Rejection { rejected, stopped } = runtime.block_on(waiting::reject(&credential, &ids))?;
    let matched = ids.len();
    let Some(Stopped {
        error,
        uncertain,
        unattempted,
    }) = stopped
    else {
        let sentence = match (rejected, matched) {
            (1, 1) => "1 rejected. It won't come back under the same Fleet Token.".to_owned(),
            (count, _) if count == matched as u64 => {
                format!("{count} rejected. They won't come back under the same Fleet Token.")
            }
            (count, _) => {
                format!("{count} of {matched} rejected; the rest had already left the list.")
            }
        };
        return ui::done(
            &json!({ "rejected": rejected, "matched": matched, "waiting_servers": listed }),
            sentence,
        );
    };
    ui::finish(
        &json!({
            "rejected": rejected,
            "matched": matched,
            "waiting_servers": listed,
            "uncertain": uncertain,
            "unattempted": unattempted,
            "error": error.json(),
        }),
        || {
            ui::stream(format_args!("{rejected} of {matched} rejected."));
            ui::warn_cause("Reject stopped", &error);
            ui::warn(format!(
                "{} may or may not be rejected and {} were not sent. Run the same reject again.",
                uncertain.len(),
                unattempted.len()
            ));
        },
    )?;
    Err(Error::partial())
}

fn matching<'a>(waiting: &'a [Waiting], pattern: &str) -> Result<Vec<&'a Waiting>, Error> {
    let named = waiting::named(waiting, pattern)?;
    if !named.is_empty() {
        return Ok(named);
    }
    let glob = Glob::new(pattern)
        .map_err(|error| {
            Error::usage(format!(
                "{} is not a valid glob: {}",
                printable(pattern),
                error.kind()
            ))
        })?
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

    #[test]
    fn a_literal_name_beats_the_glob_it_spells() {
        let host = |id: &str, name: &str| {
            let mut host = super::super::waiting::tests::waiting("waiting", None);
            host.row.id = id.into();
            host.row.name = name.into();
            host
        };
        let waiting = [host("a", "db1"), host("b", "db[1]")];
        let names = |pattern| {
            matching(&waiting, pattern)
                .unwrap()
                .into_iter()
                .map(|host| host.row.name.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(names("db[1]"), ["db[1]"]);
        assert_eq!(names("db[2]"), Vec::<&str>::new());
        assert_eq!(names("db*"), ["db1", "db[1]"]);
        assert_eq!(names("b"), ["db[1]"]);
    }
}
