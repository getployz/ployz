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
    let (waiting, mut attempts) = runtime.block_on(async {
        tokio::try_join!(waiting::fetch(&credential), waiting::enrolling(&credential))
    })?;
    // A host that began enrolling between the two reads is in both; keep it once, as waiting.
    attempts.retain(|attempt| {
        !waiting
            .iter()
            .any(|host| host.row.id == attempt.waiting_server_id)
    });
    let Matched {
        waiting: matched,
        attempts: open,
    } = matching(&waiting, &attempts, pattern)?;
    let (joining, enrolling): (Vec<_>, Vec<_>) =
        open.into_iter().partition(|attempt| attempt.joining());
    let joining: Vec<Left<'_>> = joining.into_iter().map(Left::from).collect();
    if matched.is_empty() && enrolling.is_empty() {
        if !joining.is_empty() {
            return Err(left_joining(&joining, "Nothing rejected"));
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
    let listed_joining = lefts_json(&joining);
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
    let Rejection {
        rejected,
        spared,
        stopped,
    } = runtime.block_on(waiting::reject(&credential, &ids))?;
    // Cloud is the judge of a sent join: it may have sent one the stage above doesn't show yet,
    // even for a host that was still waiting when listed.
    let (kept_enrolling, enrolling): (Vec<_>, Vec<_>) = enrolling
        .into_iter()
        .partition(|attempt| spared.contains(&attempt.waiting_server_id));
    let kept: Vec<Left<'_>> = matched
        .iter()
        .filter(|host| spared.contains(&host.row.id))
        .map(|host| Left {
            id: &host.row.id,
            enrollment_id: None,
            name: &host.row.name,
        })
        .chain(kept_enrolling.iter().map(|attempt| Left::from(*attempt)))
        .collect();
    let joining: Vec<_> = joining.into_iter().chain(kept.iter().copied()).collect();
    let listed_enrolling = attempts_json(&enrolling);
    let listed_joining = lefts_json(&joining);
    let rejected = rejected.saturating_sub(kept.len() as u64);
    let matched = ids.len() - kept.len();
    if matched == 0 && stopped.is_none() {
        return Err(left_joining(&joining, "Not cancelled"));
    }
    if !kept.is_empty() && !ui::json() {
        ui::warn(format!(
            "{}. Not cancelled. {}",
            sent(&kept),
            remove_hint(&kept)
        ));
    }
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

struct Matched<'a> {
    waiting: Vec<&'a Waiting>,
    attempts: Vec<&'a Enrolling>,
}

impl Matched<'_> {
    fn len(&self) -> usize {
        self.waiting.len() + self.attempts.len()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What `pattern` names across waiting servers and open attempts, in the order
/// [`waiting::named`] keeps: an exact id, else exact names, else the one id that starts with
/// it (eight characters or more), else the names the glob matches. An enrolling server goes
/// by its waiting server id, which the table shows, or its attempt id.
fn matching<'a>(
    waiting: &'a [Waiting],
    attempts: &'a [Enrolling],
    pattern: &str,
) -> Result<Matched<'a>, Error> {
    let pick = |host: &dyn Fn(&Waiting) -> bool, attempt: &dyn Fn(&Enrolling) -> bool| Matched {
        waiting: waiting.iter().filter(|row| host(row)).collect(),
        attempts: attempts.iter().filter(|row| attempt(row)).collect(),
    };
    let exact = pick(&|host| host.row.id == pattern, &|attempt| {
        attempt.id == pattern || attempt.waiting_server_id == pattern
    });
    if !exact.is_empty() {
        return Ok(exact);
    }
    let named = pick(&|host| host.row.name == pattern, &|attempt| {
        attempt.name == pattern
    });
    if !named.is_empty() {
        return Ok(named);
    }
    if pattern.chars().count() >= waiting::ID_PREFIX_MIN {
        let started = pick(&|host| host.row.id.starts_with(pattern), &|attempt| {
            attempt.waiting_server_id.starts_with(pattern)
        });
        match started.len() {
            0 => {}
            1 => return Ok(started),
            _ => return Err(ambiguous(&started, pattern)),
        }
    }
    let glob = glob(pattern)?;
    Ok(pick(&|host| glob.is_match(&host.row.name), &|attempt| {
        glob.is_match(&attempt.name)
    }))
}

fn ambiguous(started: &Matched<'_>, pattern: &str) -> Error {
    if started.attempts.is_empty() {
        return waiting::ambiguous(
            format!(
                "{} waiting servers have an id starting {}. Use more of the id",
                started.waiting.len(),
                printable(pattern)
            ),
            &started.waiting,
        );
    }
    let ids = started
        .waiting
        .iter()
        .map(|host| printable(&host.row.id).to_string())
        .chain(
            started
                .attempts
                .iter()
                .map(|attempt| printable(&attempt.waiting_server_id).to_string()),
        )
        .collect::<Vec<_>>()
        .join(", ");
    Error::detailed(
        RpcErrorCode::Ambiguous,
        format!(
            "{} servers have an id starting {}. Use more of the id: {ids}",
            started.len(),
            printable(pattern)
        ),
        json!({
            "waiting_servers": started
                .waiting
                .iter()
                .map(|host| json!({ "id": host.row.id, "ip": host.row.ip() }))
                .collect::<Vec<_>>(),
            "enrolling": attempts_json(&started.attempts),
        }),
    )
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

/// A server whose attempt runs on because Cloud may have sent its join.
#[derive(Clone, Copy)]
struct Left<'a> {
    id: &'a str,
    enrollment_id: Option<&'a str>,
    name: &'a str,
}

impl<'a> From<&'a Enrolling> for Left<'a> {
    fn from(attempt: &'a Enrolling) -> Self {
        Self {
            id: &attempt.waiting_server_id,
            enrollment_id: Some(&attempt.id),
            name: &attempt.name,
        }
    }
}

fn lefts_json(joining: &[Left<'_>]) -> Vec<serde_json::Value> {
    joining
        .iter()
        .map(
            |left| json!({ "id": left.id, "enrollment_id": left.enrollment_id, "name": left.name }),
        )
        .collect()
}

/// The refusal for attempts left running because Cloud may have sent their join.
fn left_joining(joining: &[Left<'_>], outcome: &str) -> Error {
    let refusal = Error::detailed(
        RpcErrorCode::Conflict,
        format!("{}. {outcome}. {}", sent(joining), remove_hint(joining)),
        json!({ "joining": lefts_json(joining) }),
    );
    match joining {
        [one] => refusal.hint(Hint::Next(format!(
            "ployz server rm {}",
            printable(one.name)
        ))),
        _ => refusal,
    }
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

fn names(joining: &[Left<'_>]) -> String {
    joining
        .iter()
        .map(|left| printable(left.name).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn sent(joining: &[Left<'_>]) -> String {
    match joining {
        [one] => format!(
            "{} may already be in the Cluster: Cloud sent its join",
            printable(one.name)
        ),
        many => format!(
            "{} may already be in the Cluster: Cloud sent their joins",
            names(many)
        ),
    }
}

fn remove_hint(joining: &[Left<'_>]) -> String {
    match joining {
        [one] => format!(
            "Once it has joined, remove it with ployz server rm {}.",
            printable(one.name)
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
            matching(&waiting, &[], pattern)
                .unwrap()
                .waiting
                .into_iter()
                .map(|host| host.row.name.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(names("db[1]"), ["db[1]"]);
        assert_eq!(names("db[2]"), Vec::<&str>::new());
        assert_eq!(names("db*"), ["db1", "db[1]"]);
        assert_eq!(names("b"), ["db[1]"]);
    }

    fn enrolling(waiting_server_id: &str, name: &str) -> Enrolling {
        serde_json::from_value(json!({
            "id": format!("attempt-{waiting_server_id}"),
            "waiting_server_id": waiting_server_id,
            "name": name,
            "state": "running",
        }))
        .unwrap()
    }

    #[test]
    fn a_literal_name_in_either_list_beats_the_glob_it_spells() {
        let mut host = super::super::waiting::tests::waiting("waiting", None);
        host.row.id = "a".into();
        host.row.name = "db[1]".into();
        let waiting = [host];
        let attempts = [enrolling("b", "db1"), enrolling("c", "web[")];
        let picked = |pattern| {
            let matched = matching(&waiting, &attempts, pattern).unwrap();
            (
                matched
                    .waiting
                    .iter()
                    .map(|host| host.row.id.as_str())
                    .collect::<Vec<_>>(),
                matched
                    .attempts
                    .iter()
                    .map(|attempt| attempt.waiting_server_id.as_str())
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(picked("db[1]"), (vec!["a"], vec![]));
        assert_eq!(picked("web["), (vec![], vec!["c"]));
        assert_eq!(picked("db*"), (vec!["a"], vec!["b"]));
    }

    #[test]
    fn an_enrolling_server_takes_a_unique_id_prefix() {
        let mut host = super::super::waiting::tests::waiting("waiting", None);
        host.row.id = "11111111-aaaa".into();
        let waiting = [host];
        let attempts = [
            enrolling("22222222-aaaa", "web-1"),
            enrolling("11111111-bbbb", "web-2"),
        ];
        let matched = matching(&waiting, &attempts, "22222222").unwrap();
        assert_eq!(matched.attempts.len(), 1);
        assert!(matched.waiting.is_empty());
        let error = matching(&waiting, &attempts, "11111111")
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.starts_with("2 servers have an id starting 11111111"),
            "{error}"
        );
    }
}
