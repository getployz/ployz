//! The replay audit: every place the Client sends an RPC, with how it repeats it.
//!
//! `call_audit.tsv` is the oracle, recorded from the call sites as they were before the Client
//! had one call engine. Each row names a call site (file and function), the Op, the flavor the
//! site used then (`was`), how the site paces its attempts (`pace`), what bounds them (`time`),
//! whether it dials again between attempts (`redial`) and what may be repeated (`replay`). The
//! scan reads the sources as they are now, takes the same columns from the call each site
//! builds, and fails when a site appears, goes, or changes any of them. A site that moved to
//! another function or file is a change to review: edit its row by hand.

use std::{collections::BTreeMap, fs, path::Path};

const FIXTURE: &str = "src/cluster/call_audit.tsv";
const ROOTS: [&str; 4] = ["src", "tests", "../ployz-testkit/tests", "../ployz-sdk/src"];
/// The calls that send: the Client's, and the SDK session's two wrappers over `call`.
const SENDS: [&str; 5] = ["call", "call_shared", "call_raw", "unary", "unary_answered"];
const SDK_WRAPPERS: [&str; 2] = ["unary", "unary_answered"];
/// Files that are the engine or its own tests; they are not call sites.
const ENGINE: [&str; 3] = [
    "src/cluster/call.rs",
    "src/cluster/call_audit.rs",
    "src/cluster/retry.rs",
];
/// The SDK session's wrappers pass the caller's call through; the sites are their callers.
const DELEGATIONS: [(&str, &str); 2] = [("src/sdk.rs", "unary"), ("src/sdk.rs", "unary_answered")];

/// A call site as the scan finds it: what it sends, and the columns the audit compares.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Row {
    site: String,
    op: String,
    policy: String,
}

/// `text`, which opens with a bracket: the text inside it, and what follows its match.
fn matching(text: &str) -> Option<(&str, &str)> {
    let mut chars = text.char_indices();
    let (_, opener) = chars.next()?;
    let closer = match opener {
        '(' => ')',
        '<' => '>',
        _ => return None,
    };
    let mut depth = 1usize;
    while let Some((index, c)) = chars.next() {
        match c {
            '"' => {
                while let Some((_, inside)) = chars.next() {
                    match inside {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            // The arrow of a closure's return type is not a closing bracket.
            '-' if opener == '<' && text.get(index + 1..)?.starts_with('>') => {
                chars.next();
            }
            c if c == opener => depth += 1,
            c if c == closer => {
                depth -= 1;
                if depth == 0 {
                    return Some((
                        text.get(opener.len_utf8()..index)?,
                        text.get(index + closer.len_utf8()..)?,
                    ));
                }
            }
            _ => {}
        }
    }
    None
}

/// `args` split at the commas that sit outside every bracket and string, whitespace folded.
fn arguments(args: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    let mut chars = args.char_indices();
    while let Some((index, c)) = chars.next() {
        match c {
            '"' => {
                while let Some((_, inside)) = chars.next() {
                    match inside {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.extend(args.get(start..index));
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.extend(args.get(start..));
    parts
        .into_iter()
        .map(|part| part.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|part| !part.is_empty())
        .collect()
}

/// The function a position sits in: the nearest `fn` before it.
fn enclosing(before: &str) -> String {
    let Some((_, after)) = before.rsplit_once("fn ") else {
        return "-".to_owned();
    };
    after
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

/// The text of the arguments of the first `.method(...)` in `chain`.
fn method<'a>(chain: &'a str, method: &str) -> Option<&'a str> {
    let (_, after) = chain.split_once(&format!(".{method}"))?;
    matching(after).map(|(args, _)| args)
}

/// The one argument of a call, without the comma rustfmt leaves after it.
fn only(args: &str) -> String {
    arguments(args).into_iter().next().unwrap_or_default()
}

/// The `wait` of a `Wait` as the call writes it.
fn wait_of(wait: &str) -> String {
    let wait = wait.trim();
    if wait.contains("Wait::standard(") {
        return "WAIT".to_owned();
    }
    if let Some((_, of)) = wait.split_once("Wait::of")
        && let Some((duration, _)) = matching(of)
    {
        return only(duration);
    }
    if let Some(fields) = wait
        .strip_prefix("Wait")
        .and_then(|rest| rest.trim_start().strip_prefix('{'))
    {
        for field in arguments(fields.trim_end().trim_end_matches('}')) {
            if field == "wait" {
                return "wait".to_owned();
            }
            if let Some(value) = field.strip_prefix("wait:") {
                return value.trim().to_owned();
            }
        }
    }
    format!("?{wait}")
}

/// A deadline as the call writes it, in the audit's words.
fn deadline_of(deadline: &str) -> String {
    let deadline = deadline.trim();
    if deadline == "Deadline::SetupRead" {
        return "setup-read(5s)".to_owned();
    }
    for (name, words) in [("Target", "target"), ("MutationReply", "reply")] {
        if let Some(duration) = deadline
            .strip_prefix("Deadline::")
            .and_then(|rest| rest.strip_prefix(name))
            .and_then(|rest| matching(rest).map(|(inside, _)| inside))
        {
            return format!("{words}({})", only(duration));
        }
    }
    format!("?{deadline}")
}

/// The pace, time, redial and replay of the call a site builds, in the fixture's columns.
fn policy_of(chain: &str) -> String {
    let replay = ["read", "keyed", "once"]
        .into_iter()
        .find(|replay| chain.contains(&format!("Call::{replay}()")))
        .unwrap_or("?");
    let (pace, time, redial) = if replay == "once" {
        let time = method(chain, "within").map_or_else(|| "-".to_owned(), deadline_of);
        ("single", time, "-")
    } else if let Some(wait) = method(chain, "waiting") {
        ("setup", format!("wait({})", wait_of(wait)), "redial")
    } else if let Some(budget) = method(chain, "budget") {
        let time = if budget.contains("target_read_timeout") {
            "budget(target_read_timeout)".to_owned()
        } else {
            format!("budget({})", only(budget))
        };
        ("unary", time, "redial")
    } else {
        ("unary", "-".to_owned(), "redial")
    };
    format!("{pace}\t{time}\t{redial}\t{replay}")
}

fn scan_file(name: &str, text: &str, rows: &mut Vec<Row>) {
    for (dot, _) in text.match_indices('.') {
        let Some((before, rest)) = text.split_at_checked(dot) else {
            continue;
        };
        let line = before.rsplit('\n').next().unwrap_or_default();
        if line.trim_start().starts_with("//") {
            continue;
        }
        let rest = rest.get(1..).unwrap_or_default();
        let word: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let Some(send) = SENDS.iter().find(|send| **send == word) else {
            continue;
        };
        if SDK_WRAPPERS.contains(send) && name != "src/sdk.rs" {
            continue;
        }
        let mut rest = rest.get(word.len()..).unwrap_or_default().trim_start();
        let op = if let Some(generic) = rest.strip_prefix("::")
            && let Some((op, after)) = matching(generic.trim_start())
        {
            rest = after.trim_start();
            op.trim().to_owned()
        } else if *send == "call_raw" {
            "-".to_owned()
        } else {
            // Without a turbofish it is another type's method, such as the Config Store's.
            continue;
        };
        let Some((args, _)) = matching(rest) else {
            continue;
        };
        let function = enclosing(before);
        if DELEGATIONS.contains(&(name, function.as_str())) {
            continue;
        }
        let args = arguments(args);
        let chain = args
            .get(if *send == "call_raw" { 2 } else { 1 })
            .map_or("", String::as_str);
        rows.push(Row {
            site: format!("{name}::{function}"),
            op,
            policy: policy_of(chain),
        });
    }
}

fn walk(base: &Path, dir: &Path, rows: &mut Vec<Row>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            walk(base, &path, rows);
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && let Ok(text) = fs::read_to_string(&path)
        {
            let name = path.strip_prefix(base).unwrap_or(&path);
            let name = name.to_string_lossy().replace('\\', "/");
            if !ENGINE.contains(&name.as_str()) {
                scan_file(&name, &text, rows);
            }
        }
    }
}

fn scan() -> Vec<Row> {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut rows = Vec::new();
    for root in ROOTS {
        walk(base, &base.join(root), &mut rows);
    }
    rows
}

/// What each site and Op sends, as the columns the audit compares, in order.
fn by_site(rows: impl Iterator<Item = (String, String, String)>) -> BTreeMap<String, Vec<String>> {
    let mut sites: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (site, op, policy) in rows {
        sites
            .entry(format!("{site}\t{op}"))
            .or_default()
            .push(policy);
    }
    for policies in sites.values_mut() {
        policies.sort();
    }
    sites
}

/// The fixture's rows, each as (site, op, the columns the audit compares).
fn recorded() -> Vec<(String, String, String)> {
    let text =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)).unwrap_or_default();
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let columns: Vec<&str> = line.split('\t').collect();
            let [site, op, _was, pace, time, redial, replay] = columns[..] else {
                panic!("a call audit row has seven columns: {line}");
            };
            (
                site.to_owned(),
                op.to_owned(),
                [pace, time, redial, replay].join("\t"),
            )
        })
        .collect()
}

/// What a `replay` may be for a pace: a single attempt is `once`, a repeating one a `read` or a
/// `keyed` mutation.
fn replay_fits(policy: &str) -> bool {
    let columns: Vec<&str> = policy.split('\t').collect();
    match columns[..] {
        ["single", _, _, replay] => replay == "once",
        [_, _, _, replay] => matches!(replay, "read" | "keyed"),
        _ => false,
    }
}

#[test]
fn every_call_site_keeps_the_replay_policy_the_audit_records() {
    let found = by_site(scan().into_iter().map(|row| (row.site, row.op, row.policy)));
    let expected_rows = recorded();
    let mut problems: Vec<String> = expected_rows
        .iter()
        .filter(|(_, _, policy)| !replay_fits(policy))
        .map(|(site, op, policy)| format!("replay does not fit pace: {site} {op} {policy}"))
        .collect();
    let expected = by_site(expected_rows.into_iter());
    for (site, policies) in &found {
        match expected.get(site) {
            None => problems.push(format!("new site, not in the audit: {site} {policies:?}")),
            Some(recorded) if recorded != policies => problems.push(format!(
                "changed policy at {site}: was {recorded:?}, now {policies:?}"
            )),
            Some(_) => {}
        }
    }
    for site in expected.keys().filter(|site| !found.contains_key(*site)) {
        problems.push(format!("site gone or changed: {site}"));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
