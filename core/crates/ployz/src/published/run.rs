//! Running one published command: the parsed arguments go back to Cloud as the
//! entry's input, and the reply prints as the entry's render says.

use std::ffi::OsString;
use std::path::Path;

use clap::ArgMatches;
use clap::parser::ValueSource;
use ployz_core::{RpcError, RpcErrorCode};
use reqwest::Method;
use serde_json::{Map, Value};

use super::wire::Ran;
use super::{ArgKind, PublishError, PublishedCommand, Render, Setup, Typed, UNKNOWN_COMMAND};
use crate::cloud_account::{self, StoreCallError};
use crate::failure::Failure;
use crate::ui::{self, Hint};

/// Run `command` with the arguments under `root`, on `setup`'s Cloud.
///
/// # Errors
///
/// Returns Cloud's refusal, a Cloud or sign-in failure, or the typed name declined.
pub(crate) fn run(
    root: &ArgMatches,
    command: &PublishedCommand,
    setup: &Setup,
) -> Result<(), Failure> {
    let leaf = leaf(root);
    let mut input = input(leaf, command);
    scope(&mut input, command, &setup.config)?;
    let approval = leaf
        .try_get_one::<String>("approval")
        .ok()
        .flatten()
        .cloned();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| Failure::command(PublishError::Runtime(source)))?;
    let credential = runtime
        .block_on(setup.credential())
        .map_err(Failure::from)?
        .ok_or_else(|| Failure::from(crate::cloud_login::LoginError::SignedOut))?;
    setup.acts_with(&credential).map_err(Failure::command)?;
    if command.typed == Some(Typed::Organization) {
        let needs_terminal = || {
            Failure::command(PublishError::NeedsTerminal {
                path: command.path(),
            })
            .hint(Hint::Next(format!("{}/", setup.cloud)))
        };
        // A command agents may run needs no word from Cloud to refuse for want of a terminal.
        // One kept for people asks first, so an agent hears `forbidden` in every mode.
        if command.agents && !ui::can_prompt() {
            return Err(needs_terminal());
        }
        let acting = runtime
            .block_on(cloud_account::acting_as(&credential))
            .map_err(Failure::from)?;
        if acting.agent && !command.agents {
            return Err(Failure::command(PublishError::AgentRefused {
                path: command.path(),
            })
            .hint(Hint::Next(format!("{}/", setup.cloud))));
        }
        ui::confirm_name(
            &acting.organization.slug,
            needs_terminal,
            "Cancelled. Nothing changed.",
        )?;
    }
    let mut approval = approval;
    let ran = loop {
        let key = uuid::Uuid::new_v4().to_string();
        let ran = runtime
            .block_on(run_on(
                &credential,
                cloud_account::Via::Cli,
                &command.name,
                Value::Object(input.clone()),
                approval.as_deref(),
                &key,
            ))
            .0;
        let asked = match &ran {
            Err(StoreCallError::Refused(refusal)) if !ui::json() => {
                crate::approval::Asked::of(refusal)
            }
            _ => None,
        };
        let Some(asked) = asked else {
            break ran.map_err(|error| refused(error, root, command, setup))?;
        };
        let retry = retry(root, command, &asked.id);
        approval = Some(
            crate::approval::settle(&runtime, &credential, asked.verb(), &asked, retry)
                .map_err(Failure::from)?,
        );
    };
    print(command, ran.output, ran.message)
}

/// Post `input` to Cloud's run route for the entry named `name`, under `key` so a
/// retry runs it once. The second half is the manifest revision the reply named.
pub(crate) async fn run_on(
    credential: &cloud_account::Credential,
    via: cloud_account::Via,
    name: &str,
    input: Value,
    approval: Option<&str>,
    key: &str,
) -> (Result<Ran, StoreCallError>, Option<String>) {
    cloud_account::keyed_reply(
        credential,
        via,
        Method::POST,
        &format!("run/{name}"),
        &serde_json::json!({ "input": input }),
        approval,
        Some(key),
    )
    .await
}

fn refused(
    error: StoreCallError,
    root: &ArgMatches,
    command: &PublishedCommand,
    setup: &Setup,
) -> Failure {
    match error {
        StoreCallError::Refused(RpcError { code, .. }) if code.as_str() == UNKNOWN_COMMAND => {
            setup.evict(&command.name);
            Failure::command(PublishError::Gone {
                path: command.path(),
            })
            .hint(Hint::Inspect("ployz --help".to_owned()))
        }
        StoreCallError::Refused(refusal)
            if refusal.code == RpcErrorCode::Ambiguous
                && refusal.details.get("projects").is_some() =>
        {
            Failure::from(refusal).hint(Hint::Next("ployz link --project PROJECT".to_owned()))
        }
        StoreCallError::Refused(refusal) => match crate::approval::Asked::of(&refusal) {
            Some(asked) => {
                let retry = retry(root, command, &asked.id);
                Failure::from(refusal).hint(Hint::Retry(retry))
            }
            None => StoreCallError::Refused(refusal).into(),
        },
        error @ (StoreCallError::Cloud(_) | StoreCallError::Stopped(_)) => error.into(),
    }
}

/// This command again as typed, carrying approval `id`, in the same output mode. A
/// value that starts with `-` still reads as a value: a flag's joins it with `=`,
/// and positionals move behind `--`.
fn retry(root: &ArgMatches, command: &PublishedCommand, id: &str) -> String {
    let leaf = leaf(root);
    let typed = |id: &str| root.value_source(id) == Some(ValueSource::CommandLine);
    let mut words = vec!["ployz".to_owned()];
    words.extend(
        crate::handlers::config_flag(root)
            .into_iter()
            .flatten()
            .map(ToOwned::to_owned),
    );
    if typed("json") {
        words.push("--json".to_owned());
    }
    if typed("color")
        && let Some(color) = root.get_one::<String>("color")
    {
        words.extend(["--color".to_owned(), color.clone()]);
    }
    words.extend(command.path.iter().cloned());
    let (mut positionals, mut named) = (Vec::new(), Vec::new());
    for arg in &command.args {
        for value in leaf.get_raw(&arg.name).into_iter().flatten() {
            let value = value.to_string_lossy().into_owned();
            if arg.positional {
                positionals.push(value);
            } else if value.starts_with('-') {
                named.push(format!("--{}={value}", arg.name));
            } else {
                named.extend([format!("--{}", arg.name), value]);
            }
        }
    }
    named.extend(["--approval".to_owned(), id.to_owned()]);
    if positionals.iter().any(|value| value.starts_with('-')) {
        words.extend(named);
        words.push("--".to_owned());
        words.extend(positionals);
    } else {
        words.extend(positionals);
        words.extend(named);
    }
    shell_words::join(words)
}

fn leaf(mut matches: &ArgMatches) -> &ArgMatches {
    while let Some((_, child)) = matches.subcommand() {
        matches = child;
    }
    matches
}

/// The entry's input from what was typed, as its schema shapes it. An argument left
/// out stays out, so Cloud applies its own default.
fn input(leaf: &ArgMatches, command: &PublishedCommand) -> Map<String, Value> {
    let mut input = Map::new();
    for arg in &command.args {
        let value = match &arg.kind {
            ArgKind::Text | ArgKind::Choice(_) => leaf
                .get_one::<String>(&arg.name)
                .map(|text| text.as_str().into()),
            ArgKind::Integer => leaf.get_one::<i64>(&arg.name).map(|n| (*n).into()),
            ArgKind::Boolean => leaf.get_one::<bool>(&arg.name).map(|b| (*b).into()),
            ArgKind::Texts => leaf
                .get_many::<String>(&arg.name)
                .map(|texts| Value::Array(texts.map(|text| Value::from(text.as_str())).collect())),
        };
        if let Some(value) = value {
            input.insert(arg.name.clone(), value);
        }
    }
    input
}

/// Fill the Project and Environment the entry takes and the command line left out
/// from `PLOYZ_PROJECT`, `PLOYZ_ENV` and the directory link, as a compiled command
/// fills them.
fn scope(
    input: &mut Map<String, Value>,
    command: &PublishedCommand,
    config: &Path,
) -> Result<(), Failure> {
    let takes = |name: &str| command.args.iter().any(|arg| arg.name == name);
    if !takes("project") {
        return Ok(());
    }
    let given = |name| input.get(name).and_then(Value::as_str);
    let at =
        crate::handlers::link::scope_given(config, given("project"), given("environment"))?.at();
    if let Some(project) = at.project {
        input.insert("project".to_owned(), project.as_str().into());
    }
    if let Some(environment) = at.environment.filter(|_| takes("environment")) {
        input.insert("environment".to_owned(), environment.as_str().into());
    }
    Ok(())
}

/// Refuse a bad `PLOYZ_PROJECT` or `PLOYZ_ENV` before fetching the manifest that a
/// command line the cache lacks needs, as [`scope`] would once the command is known.
/// A typed `--project` or `--environment` still wins over its variable.
pub(crate) fn check_scope_env(args: &[OsString]) -> Result<(), Failure> {
    let typed = |flag: &str| {
        args.iter()
            .take_while(|arg| *arg != "--")
            .filter_map(|arg| arg.to_str())
            .any(|arg| {
                arg.strip_prefix(flag)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('='))
            })
    };
    crate::handlers::link::check_env(typed("--project"), typed("--environment"))
}

fn print(
    command: &PublishedCommand,
    output: Value,
    message: Option<String>,
) -> Result<(), Failure> {
    match command.render {
        Render::Message => ui::done(&output, message.unwrap_or_default()),
        Render::Record => ui::fields(&output, &record(&command.output, &output)),
        Render::Table => {
            let columns = properties(items_of(&command.output));
            let mut table = ui::Table::new(
                columns
                    .iter()
                    .map(|column| column.replace('_', " ").to_uppercase()),
                format!("Nothing from ployz {} yet.", command.path()),
            );
            let items = items_of(&command.output);
            for row in output.as_array().into_iter().flatten() {
                table.row(
                    columns
                        .iter()
                        .map(|column| cell(row.get(column), property(items, column))),
                );
            }
            ui::list(&output, &table)
        }
    }
}

/// One field per property, in schema order. A list of records under one property
/// takes one field per record, the way `volume show` repeats `mounted by`.
fn record(schema: &Value, output: &Value) -> ui::Fields {
    let mut fields = ui::Fields::new();
    for name in properties(schema) {
        let under = property(schema, &name);
        match output.get(&name) {
            Some(Value::Array(items)) if items.iter().any(Value::is_object) => {
                for item in items {
                    fields.push(label(&name), inline(item, items_of(under)));
                }
            }
            value => fields.push(label(&name), cell(value, under)),
        }
    }
    fields
}

/// Property names as the schema lists them. Cloud's `required` keeps that order; a
/// property outside it follows, in name order.
fn properties(schema: &Value) -> Vec<String> {
    let schema = shaped(schema);
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut names: Vec<String> = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|name| properties.contains_key(*name))
        .map(ToOwned::to_owned)
        .collect();
    let rest: Vec<String> = properties
        .keys()
        .filter(|name| !names.contains(name))
        .cloned()
        .collect();
    names.extend(rest);
    names
}

/// The branch of `schema` that has a shape: itself, or the first `anyOf` branch that
/// is not `null`.
fn shaped(schema: &Value) -> &Value {
    schema
        .get("anyOf")
        .and_then(Value::as_array)
        .and_then(|branches| {
            branches
                .iter()
                .find(|branch| branch.get("type") != Some(&Value::from("null")))
        })
        .unwrap_or(schema)
}

fn property<'a>(schema: &'a Value, name: &str) -> &'a Value {
    shaped(schema)
        .get("properties")
        .and_then(|properties| properties.get(name))
        .unwrap_or(&Value::Null)
}

fn items_of(schema: &Value) -> &Value {
    shaped(schema).get("items").unwrap_or(&Value::Null)
}

fn label(name: &str) -> String {
    let mut label = name.replace('_', " ");
    if let Some(first) = label.get(..1) {
        let first = first.to_uppercase();
        label.replace_range(..1, &first);
    }
    label
}

fn cell(value: Option<&Value>, schema: &Value) -> String {
    value.map_or_else(String::new, |value| inline(value, schema))
}

/// A value on one line: a word for a scalar, a list as its items, and a record as
/// `key value` pairs in schema order, with a nested record or list in parentheses.
/// A key with nothing under it is left out.
fn inline(value: &Value, schema: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(true) => "yes".to_owned(),
        Value::Bool(false) => "no".to_owned(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Array(items) => {
            let between = if items.iter().any(Value::is_object) {
                "; "
            } else {
                ", "
            };
            items
                .iter()
                .map(|item| inline(item, items_of(schema)))
                .collect::<Vec<_>>()
                .join(between)
        }
        Value::Object(pairs) => {
            let mut keys = properties(schema);
            let rest: Vec<String> = pairs
                .keys()
                .filter(|key| !keys.contains(key))
                .cloned()
                .collect();
            keys.extend(rest);
            keys.iter()
                .filter_map(|key| pairs.get(key).map(|value| (key, value)))
                .filter(|(_, value)| !empty(value))
                .map(|(key, value)| {
                    let text = inline(value, property(schema, key));
                    let key = key.replace('_', " ");
                    if matches!(value, Value::Object(_) | Value::Array(_)) {
                        format!("{key} ({text})")
                    } else {
                        format!("{key} {text}")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

fn empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(pairs) => pairs.is_empty(),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

#[cfg(test)]
#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
mod tests {
    use super::*;
    use serde_json::json;

    /// A record under `schema` with every property filled, two items per list. With
    /// `hollow`, each property that may be null is.
    fn sample(schema: &Value, name: &str, hollow: bool) -> Value {
        if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
            if hollow && branches.iter().any(|branch| branch["type"] == "null") {
                return Value::Null;
            }
            return sample(&branches[0], name, hollow);
        }
        let kinds: Vec<&str> = match &schema["type"] {
            Value::String(kind) => vec![kind],
            Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).collect(),
            other @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_)) => {
                panic!("{name} has no type: {other}")
            }
        };
        if hollow && kinds.contains(&"null") {
            return Value::Null;
        }
        match kinds.iter().find(|kind| **kind != "null").unwrap() {
            &"string" => schema["enum"]
                .as_array()
                .map_or_else(|| json!(format!("<{name}>")), |choices| choices[0].clone()),
            &"number" | &"integer" => json!(12),
            &"boolean" => json!(true),
            &"array" => json!([
                sample(&schema["items"], name, false),
                sample(&schema["items"], name, true),
            ]),
            &"object" => Value::Object(
                schema["properties"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(key, schema)| (key.clone(), sample(schema, key, hollow)))
                    .collect(),
            ),
            kind => panic!("{name} is a {kind}"),
        }
    }

    /// The fixture manifest, with `server upgrade-policy set` and `org approval deny`
    /// behind approval so a gated entry takes named flags too.
    fn gated() -> crate::published::Published {
        let mut published = crate::published::Published::parse(include_bytes!(
            "../../tests/fixtures/published-manifest.json"
        ))
        .unwrap();
        for command in &mut published.commands {
            if ["server upgrade-policy set", "org approval deny"].contains(&command.path().as_str())
            {
                command.effect = crate::published::Effect::Write {
                    ask: true,
                    removes: false,
                };
            }
        }
        published
    }

    fn parsed(published: &crate::published::Published, argv: &[&str]) -> ArgMatches {
        crate::published::attach(crate::cli::compiled(), published)
            .try_get_matches_from(argv)
            .unwrap_or_else(|error| panic!("{argv:?}: {error}"))
    }

    #[test]
    fn an_approval_retry_parses_with_every_argument_typed() {
        let published = gated();
        for (path, argv) in [
            (
                "org member role",
                vec!["ployz", "org", "member", "role", "Linus Torvalds", "admin"],
            ),
            (
                "server upgrade-policy set",
                vec![
                    "ployz",
                    "--ployz-config",
                    "/tmp/ployz config.toml",
                    "server",
                    "upgrade-policy",
                    "set",
                    "--channel",
                    "beta",
                    "--automatic",
                    "false",
                ],
            ),
            (
                "org member role",
                vec![
                    "ployz",
                    "--json",
                    "org",
                    "member",
                    "role",
                    "Linus Torvalds",
                    "admin",
                ],
            ),
            (
                "org member role",
                vec![
                    "ployz", "org", "member", "role", "Linus", "admin", "--color", "always",
                ],
            ),
            (
                "org member rm",
                vec!["ployz", "org", "member", "rm", "--", "--json"],
            ),
            (
                "org approval deny",
                vec![
                    "ployz",
                    "org",
                    "approval",
                    "deny",
                    "--reason=-no",
                    "--",
                    "-a7",
                ],
            ),
        ] {
            let command = published.find(path).unwrap();
            let root = parsed(&published, &argv);
            let again = retry(&root, command, "a7e572f3");
            let words = shell_words::split(&again).unwrap();
            let words: Vec<&str> = words.iter().map(String::as_str).collect();
            let again = parsed(&published, &words);
            assert_eq!(
                input(leaf(&again), command),
                input(leaf(&root), command),
                "{path}"
            );
            assert_eq!(
                leaf(&again)
                    .get_one::<String>("approval")
                    .map(String::as_str),
                Some("a7e572f3")
            );
            for flag in ["ployz-config", "json", "color"] {
                assert_eq!(again.value_source(flag), root.value_source(flag), "{flag}");
                assert_eq!(
                    again.get_raw(flag).map(Iterator::collect::<Vec<_>>),
                    root.get_raw(flag).map(Iterator::collect::<Vec<_>>),
                    "{flag}"
                );
            }
        }
    }

    fn written(fields: &ui::Fields, aligned: bool) -> String {
        let mut out = anstream::StripStream::new(Vec::new());
        fields.write(&mut out, aligned).unwrap();
        String::from_utf8(out.into_inner()).unwrap()
    }

    #[test]
    fn a_record_with_nested_lists_reads_as_one_field_per_item_and_never_as_json() {
        let schema: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/nested-record-output.json"
        ))
        .unwrap();
        let output = sample(&schema, "output", false);
        let record = record(&schema, &output);
        let expected = include_str!("../../tests/fixtures/nested-record-output.txt");
        assert_eq!(written(&record, false), expected);
        let aligned = written(&record, true);
        assert_eq!(aligned.lines().count(), expected.lines().count());
        for text in [&aligned, expected] {
            assert!(!text.contains('{') && !text.contains('"'), "{text}");
        }
    }

    #[test]
    fn a_table_cell_holding_a_record_reads_as_its_pairs_in_schema_order() {
        let schema = json!({
            "type": "object",
            "properties": {
                "ms": { "type": "number" },
                "at": { "type": "string" },
                "note": { "type": ["string", "null"] }
            },
            "required": ["ms", "at", "note"]
        });
        let row = json!({ "name": "web", "latest": { "at": "noon", "ms": 12, "note": null } });
        assert_eq!(cell(row.get("latest"), &schema), "ms 12, at noon");
        assert_eq!(cell(row.get("missing"), &schema), "");
        assert_eq!(
            inline(&json!([true, "a", 1.5]), &Value::Null),
            "yes, a, 1.5"
        );
    }
}
