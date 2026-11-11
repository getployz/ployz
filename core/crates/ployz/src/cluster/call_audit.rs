//! The replay audit: every place the Client sends an RPC, with how it repeats it.
//!
//! `call_audit.tsv` is the oracle. Each row names a call site (file and function), the Op, the
//! flavor the site used before the Client had one call engine (`was`), how the site paces its
//! attempts (`pace`), what bounds them (`time`), whether it dials again between attempts
//! (`redial`) and what may be repeated (`replay`). The scan reads the sources and fails when a
//! site appears, goes, or changes any column but `replay`; `replay` is chosen by hand, and a
//! row whose `replay` disagrees with its `pace` fails too.
//!
//! Set `PLOYZ_BLESS_CALL_AUDIT=1` to rewrite the fixture after a deliberate change. Rows for
//! a new site get `?` as their `replay`, which fails until someone chooses.

use std::{collections::BTreeMap, fmt::Write as _, fs, path::Path};

const FIXTURE: &str = "src/cluster/call_audit.tsv";
const ROOTS: [&str; 4] = ["src", "tests", "../ployz-testkit/tests", "../ployz-sdk/src"];
const FLAVORS: [&str; 14] = [
    "call",
    "call_repeatable",
    "call_repeatable_for",
    "read_once",
    "call_unretried",
    "read",
    "try_read",
    "invoke",
    "invoke_answered",
    "invoke_entry",
    "invoke_raw",
    "unary",
    "unary_answered",
    "repeatable",
];
/// The SDK session's own wrappers over `call` and `read`, which its Op methods call.
const SDK_WRAPPERS: [&str; 3] = ["unary", "unary_answered", "repeatable"];
/// The flavor definitions delegate to each other; the sites are their callers.
const DELEGATIONS: [(&str, &str); 6] = [
    ("src/cluster.rs", "call_repeatable"),
    ("src/cluster.rs", "read"),
    ("src/cluster.rs", "invoke"),
    ("src/sdk.rs", "unary"),
    ("src/sdk.rs", "unary_answered"),
    ("src/sdk.rs", "repeatable"),
];

#[derive(Clone, Debug, Eq, PartialEq)]
struct Row {
    site: String,
    op: String,
    was: String,
    pace: String,
    time: String,
    redial: &'static str,
}

impl Row {
    fn key(&self) -> String {
        format!("{}\t{}\t{}", self.site, self.op, self.was)
    }

    fn policy(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            self.site, self.op, self.was, self.pace, self.time, self.redial
        )
    }
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

fn time_of(timeout: Option<&String>) -> String {
    match timeout.map(String::as_str) {
        None | Some("None") => "-".to_owned(),
        Some(other) => match other
            .strip_prefix("Some(")
            .and_then(|some| some.strip_suffix(')'))
        {
            Some(duration) => format!("target({duration})"),
            None => format!("target(?{other})"),
        },
    }
}

fn row(site: &str, flavor: &str, op: &str, args: &[String]) -> Row {
    let (pace, time, redial) = match flavor {
        "call" | "unary" | "unary_answered" => ("unary", "-".to_owned(), "redial"),
        "read" | "try_read" | "repeatable" => {
            ("unary", "budget(target_read_timeout)".to_owned(), "redial")
        }
        "call_repeatable" => ("setup", "wait(WAIT)".to_owned(), "redial"),
        "call_repeatable_for" => (
            "setup",
            format!("wait({})", args.get(4).map_or("?", String::as_str)),
            "redial",
        ),
        "read_once" => ("single", "setup-read(5s)".to_owned(), "-"),
        "call_unretried" => (
            "single",
            format!("reply({})", args.get(2).map_or("?", String::as_str)),
            "-",
        ),
        "invoke" | "invoke_answered" => ("single", time_of(args.get(2)), "-"),
        "invoke_entry" => ("single", time_of(args.get(1)), "-"),
        "invoke_raw" => ("single", time_of(args.get(3)), "-"),
        other => unreachable!("a flavor the audit does not know: {other}"),
    };
    Row {
        site: site.to_owned(),
        op: op.to_owned(),
        was: flavor.to_owned(),
        pace: pace.to_owned(),
        time,
        redial,
    }
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
        let Some(flavor) = FLAVORS.iter().find(|flavor| **flavor == word) else {
            continue;
        };
        if SDK_WRAPPERS.contains(flavor) && name != "src/sdk.rs" {
            continue;
        }
        let mut rest = rest.get(word.len()..).unwrap_or_default().trim_start();
        let op = if let Some(generic) = rest.strip_prefix("::")
            && let Some((op, after)) = matching(generic.trim_start())
        {
            rest = after.trim_start();
            op.trim().to_owned()
        } else if *flavor == "invoke_raw" {
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
        rows.push(row(
            &format!("{name}::{function}"),
            flavor,
            &op,
            &arguments(args),
        ));
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
            scan_file(&name.to_string_lossy().replace('\\', "/"), &text, rows);
        }
    }
}

fn scan() -> Vec<Row> {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut rows = Vec::new();
    for root in ROOTS {
        walk(base, &base.join(root), &mut rows);
    }
    rows.sort_by_key(Row::policy);
    rows
}

/// A row as the fixture holds it: the policy the scan finds, and the hand-chosen replay.
#[derive(Debug)]
struct Recorded {
    policy: String,
    pace: String,
    replay: String,
}

/// Rows are told apart by their site, Op and flavor, and by order when those repeat.
fn numbered<'a>(keys: impl Iterator<Item = (String, &'a str)>) -> Vec<(String, &'a str)> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    keys.map(|(key, value)| {
        let count = seen.entry(key.clone()).or_default();
        let numbered = format!("{key}\t#{count}");
        *count += 1;
        (numbered, value)
    })
    .collect()
}

fn recorded() -> BTreeMap<String, Recorded> {
    let text =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|line| !line.starts_with('#')).collect();
    let keys = lines.iter().map(|line| {
        let key = line.splitn(4, '\t').take(3).collect::<Vec<_>>().join("\t");
        (key, *line)
    });
    numbered(keys)
        .into_iter()
        .map(|(key, line)| {
            let columns: Vec<&str> = line.split('\t').collect();
            let [site, op, was, pace, time, redial, replay] = columns[..] else {
                panic!("a call audit row has seven columns: {line}");
            };
            (
                key,
                Recorded {
                    policy: [site, op, was, pace, time, redial].join("\t"),
                    pace: pace.to_owned(),
                    replay: replay.to_owned(),
                },
            )
        })
        .collect()
}

fn bless(rows: &[Row]) {
    let replays: BTreeMap<String, String> = recorded()
        .into_iter()
        .map(|(key, recorded)| (key, recorded.replay))
        .collect();
    let mut out = String::from(
        "# site\top\twas\tpace\ttime\tredial\treplay\n\
         # pace: unary = UNARY_RETRY_DELAYS (500 ms, 1500 ms, 4 s); setup = 5 s attempts, 1 s apart, until `time`; single = one attempt.\n\
         # replay: read | keyed repeat; once never does.\n",
    );
    let policies: Vec<String> = rows.iter().map(Row::policy).collect();
    let keys = rows
        .iter()
        .zip(&policies)
        .map(|(row, policy)| (row.key(), policy.as_str()));
    for (key, policy) in numbered(keys) {
        let replay = replays.get(&key).map_or("?", String::as_str);
        let _ = writeln!(out, "{policy}\t{replay}");
    }
    fs::write(Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE), out)
        .expect("the call audit fixture is writable");
}

/// What a `replay` may be for a pace: a single attempt is `once`, a repeating one a `read` or a
/// `keyed` mutation.
fn replay_fits(pace: &str, replay: &str) -> bool {
    match pace {
        "single" => replay == "once",
        _ => matches!(replay, "read" | "keyed"),
    }
}

#[test]
fn every_call_site_keeps_the_replay_policy_the_audit_records() {
    let rows = scan();
    if std::env::var_os("PLOYZ_BLESS_CALL_AUDIT").is_some() {
        bless(&rows);
    }
    let expected = recorded();
    let policies: Vec<String> = rows.iter().map(Row::policy).collect();
    let keys = rows
        .iter()
        .zip(&policies)
        .map(|(row, policy)| (row.key(), policy.as_str()));
    let found: BTreeMap<String, &str> = numbered(keys).into_iter().collect();
    let mut problems = Vec::new();
    for (key, policy) in &found {
        match expected.get(key) {
            None => problems.push(format!("new site, not in the audit: {policy}")),
            Some(recorded) if recorded.policy != *policy => problems.push(format!(
                "changed policy: was `{}`, now `{policy}`",
                recorded.policy
            )),
            Some(_) => {}
        }
    }
    for (key, recorded) in &expected {
        if !found.contains_key(key) {
            problems.push(format!("site gone or changed: {}", recorded.policy));
        }
        if !replay_fits(&recorded.pace, &recorded.replay) {
            problems.push(format!(
                "replay `{}` does not fit pace `{}`: {}",
                recorded.replay, recorded.pace, recorded.policy
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
