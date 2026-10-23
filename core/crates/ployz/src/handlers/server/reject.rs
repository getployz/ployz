//! `server reject`: turn away the waiting servers a glob matches, and the enrolling ones whose
//! join Cloud hasn't sent, cancelling their attempt. Their check-in loop stops and they won't
//! come back under the same Fleet Token.

use std::io::IsTerminal;

use clap::{ArgMatches, Command};
use globset::Glob;
use ployz_core::RpcErrorCode;
use serde_json::json;

use super::waiting::{self, Enrolling, Rejection, Stopped, Waiting};
use crate::cli::{base, positional};
use crate::handlers::{Error, leaf_matches, runtime, typed_with};
use crate::ui::{self, Cell, Hint, Table, printable};

pub(crate) fn command(command: Command) -> Command {
    base(command, "Reject servers waiting to join")
        .long_about(
            "Reject the servers waiting to join whose names match a glob, such as 'web-*', \
             or one by its id. \
             It also rejects enrolling servers and cancels their attempt, \
             unless Cloud already sent the join; remove those with ployz server rm once joined. \
             It lists the matches and asks before it rejects them; --yes skips the ask. \
             A rejected server stops checking in and won't come back under the same Fleet Token; \
             revoke the token to turn away every server that holds it. \
             Outside a terminal without --yes it fails with confirmation_required.",
        )
        .arg(
            positional("pattern", true)
                .help("A name, a glob such as 'web-*', or a waiting server's id"),
        )
        .arg(crate::cli::yes("Reject the matches without asking"))
}

pub(crate) fn reject(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let pattern = matches
        .get_one::<String>("pattern")
        .ok_or_else(|| Error::usage("pattern is required"))?;
    let runtime = runtime()?;
    let credential = waiting::credential(&runtime, matches)?;
    let (waiting, attempts) = runtime.block_on(async {
        tokio::try_join!(waiting::fetch(&credential), waiting::enrolling(&credential))
    })?;
    let matched = matching(&waiting, pattern)?;
    let (joining, enrolling): (Vec<_>, Vec<_>) = enrolling_matches(&attempts, pattern)?
        .into_iter()
        .partition(|attempt| attempt.joining());
    if matched.is_empty() && enrolling.is_empty() {
        if !joining.is_empty() {
            return Err(Error::detailed(
                RpcErrorCode::Conflict,
                format!(
                    "{}. Nothing rejected. {}",
                    sent(&joining),
                    remove_hint(&joining)
                ),
                json!({ "joining": attempts_json(&joining) }),
            ));
        }
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
    let listed_enrolling = attempts_json(&enrolling);
    let listed_joining = attempts_json(&joining);
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
        for attempt in &enrolling {
            table.row([
                Cell::from(printable(ui::short_id(&attempt.waiting_server_id))),
                Cell::from(printable(&attempt.name)),
                Cell::from("enrolling"),
                Cell::from("-"),
            ]);
        }
        let stderr = std::io::stderr();
        let _ = table.write(&mut anstream::stderr(), stderr.is_terminal());
        if !joining.is_empty() {
            let them = if joining.len() == 1 { "it" } else { "them" };
            ui::warn(format!(
                "{}. Not rejecting {them}. {}",
                sent(&joining),
                remove_hint(&joining)
            ));
        }
    }
    let described = described(matched.len(), enrolling.len());
    if !matches.get_flag("yes") {
        ui::confirm(
            &format!("Reject {described}?"),
            || {
                Error::detailed(
                    RpcErrorCode::ConfirmationRequired,
                    format!("Rejecting {described} needs confirmation. Nothing rejected."),
                    json!({ "waiting_servers": listed, "enrolling": listed_enrolling, "joining": listed_joining }),
                )
                .hint(Hint::Retry(typed_with(&["--yes"])))
            },
            "Nothing rejected.",
        )?;
    }
    let ids: Vec<&str> = matched
        .iter()
        .map(|host| host.row.id.as_str())
        .chain(
            enrolling
                .iter()
                .map(|attempt| attempt.waiting_server_id.as_str()),
        )
        .collect();
    let Rejection { rejected, stopped } = runtime.block_on(waiting::reject(&credential, &ids))?;
    let matched = ids.len();
    let Some(Stopped {
        error,
        uncertain,
        not_rejected,
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
            &json!({ "rejected": rejected, "matched": matched, "waiting_servers": listed, "enrolling": listed_enrolling, "joining": listed_joining }),
            sentence,
        );
    };
    ui::finish(
        &json!({
            "rejected": rejected,
            "matched": matched,
            "waiting_servers": listed,
            "enrolling": listed_enrolling,
            "joining": listed_joining,
            "uncertain": uncertain,
            "not_rejected": not_rejected,
            "error": error.json(),
        }),
        || {
            ui::stream(format_args!("{rejected} of {matched} rejected."));
            ui::warn_cause("Reject stopped", &error);
            if uncertain.is_empty() {
                ui::warn(format!(
                    "{} server{} not rejected.",
                    not_rejected.len(),
                    plural(not_rejected.len())
                ));
            } else {
                ui::warn(format!(
                    "{} server{} may or may not be rejected and {} not. Run the same reject again.",
                    uncertain.len(),
                    plural(uncertain.len()),
                    not_rejected.len()
                ));
            }
        },
    )?;
    Err(Error::partial())
}

fn matching<'a>(waiting: &'a [Waiting], pattern: &str) -> Result<Vec<&'a Waiting>, Error> {
    let named = waiting::named(waiting, pattern)?;
    if !named.is_empty() {
        return Ok(named);
    }
    let glob = glob(pattern)?;
    Ok(waiting
        .iter()
        .filter(|host| glob.is_match(&host.row.name))
        .collect())
}

/// The open attempts `pattern` names by attempt id, waiting server id or name, else those
/// whose name the glob matches.
fn enrolling_matches<'a>(
    attempts: &'a [Enrolling],
    pattern: &str,
) -> Result<Vec<&'a Enrolling>, Error> {
    let named: Vec<_> = attempts
        .iter()
        .filter(|attempt| {
            attempt.id == pattern || attempt.waiting_server_id == pattern || attempt.name == pattern
        })
        .collect();
    if !named.is_empty() {
        return Ok(named);
    }
    let glob = glob(pattern)?;
    Ok(attempts
        .iter()
        .filter(|attempt| glob.is_match(&attempt.name))
        .collect())
}

fn glob(pattern: &str) -> Result<globset::GlobMatcher, Error> {
    Ok(Glob::new(pattern)
        .map_err(|error| {
            Error::usage(format!(
                "{} is not a valid glob: {}",
                printable(pattern),
                error.kind()
            ))
        })?
        .compile_matcher())
}

fn attempts_json(attempts: &[&Enrolling]) -> Vec<serde_json::Value> {
    attempts
        .iter()
        .map(|attempt| {
            json!({
                "id": attempt.waiting_server_id,
                "enrollment_id": attempt.id,
                "name": attempt.name,
            })
        })
        .collect()
}

fn names(attempts: &[&Enrolling]) -> String {
    attempts
        .iter()
        .map(|attempt| printable(&attempt.name).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn sent(joining: &[&Enrolling]) -> String {
    match joining {
        [one] => format!(
            "{} may already be in the Cluster: Cloud sent its join",
            printable(&one.name)
        ),
        many => format!(
            "{} may already be in the Cluster: Cloud sent their joins",
            names(many)
        ),
    }
}

fn remove_hint(joining: &[&Enrolling]) -> String {
    match joining {
        [one] => format!(
            "Once it has joined, remove it with ployz server rm {}.",
            printable(&one.name)
        ),
        _ => "Once they have joined, remove them with ployz server rm.".to_owned(),
    }
}

fn described(waiting: usize, enrolling: usize) -> String {
    let count = |count: usize, kind: &str| format!("{count} {kind} server{}", plural(count));
    match (waiting, enrolling) {
        (_, 0) => count(waiting, "waiting"),
        (0, _) => count(enrolling, "enrolling"),
        _ => format!(
            "{} and {}",
            count(waiting, "waiting"),
            count(enrolling, "enrolling")
        ),
    }
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
