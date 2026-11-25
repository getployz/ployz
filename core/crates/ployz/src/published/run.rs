//! Running one published command: the parsed arguments go back to Cloud as the
//! entry's input, and the reply prints as the entry's render says.

use std::ffi::OsString;
use std::path::Path;

use clap::ArgMatches;
use ployz_core::{Hint, RpcError, RpcErrorCode};
use reqwest::Method;
use serde_json::{Map, Value};

use super::wire::Ran;
use super::{ArgKind, PublishError, PublishedCommand, Render, Setup, Typed, UNKNOWN_COMMAND};
use crate::cli::{Rerun, leaf_matches};
use crate::cloud;
use crate::cloud::account::StoreCallError;
use crate::failure::Failure;
use crate::ui;

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
    let leaf = leaf_matches(root);
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
        .ok_or_else(|| Failure::from(crate::cloud::login::LoginError::SignedOut))?;
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
            .block_on(cloud::account::acting_as(&credential))
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
    let mut said_yes = approval.is_some();
    // One key for the command, so the resend after a yes is the same request.
    let key = uuid::Uuid::new_v4().to_string();
    let ran = loop {
        let ran = runtime
            .block_on(run_on(
                &credential,
                cloud::http::Via::Cli,
                &command.name,
                Value::Object(input.clone()),
                approval.as_deref(),
                &key,
            ))
            .0;
        let (asked, refusal) = match ran {
            Err(StoreCallError::Refused(refusal)) if !ui::json() => {
                match crate::approval::Asked::of(&refusal) {
                    Some(asked) => (asked, refusal),
                    None => {
                        return Err(refused(
                            StoreCallError::Refused(refusal),
                            root,
                            command,
                            setup,
                        ));
                    }
                }
            }
            ran => break ran.map_err(|error| refused(error, root, command, setup))?,
        };
        if said_yes {
            return Err(crate::approval::moved(asked.verb(None), refusal));
        }
        let retry = retry(root, setup, asked.digest());
        approval =
            Some(crate::approval::settle(asked.verb(None), &asked, retry).map_err(Failure::from)?);
        said_yes = true;
    };
    print(command, ran.output, ran.message)
}

/// Post `input` to Cloud's run route for the entry named `name`, under `key` so a
/// retry runs it once. The second half is the manifest revision the reply named.
pub(crate) async fn run_on(
    credential: &cloud::account::Credential,
    via: cloud::http::Via,
    name: &str,
    input: Value,
    approval: Option<&str>,
    key: &str,
) -> (Result<Ran, StoreCallError>, Option<String>) {
    cloud::account::keyed_reply(
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
            Some(asked) => Failure::from(refusal).hint(retry(root, setup, asked.digest())),
            None => StoreCallError::Refused(refusal).into(),
        },
        error @ (StoreCallError::Cloud(_) | StoreCallError::Stopped(_)) => error.into(),
    }
}

/// This command again as typed, carrying `id`, the digest of the yes it needs.
fn retry(root: &ArgMatches, setup: &Setup, id: &str) -> Rerun {
    let compiled = crate::cli::compiled();
    let tree = match setup.cached() {
        Some(published) => super::attach(compiled, &published),
        None => compiled,
    };
    again(tree, root, id)
}

/// The command line `tree` parsed into `root` again, carrying `id`.
fn again(tree: clap::Command, root: &ArgMatches, id: &str) -> Rerun {
    Rerun::again_in(tree, root).set("approval", id)
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
    let at = crate::scope::Scope::given(given("project"), given("environment"))?
        .linked_here(config)?
        .at();
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
/// A typed `--project` or `--env` still wins over its variable.
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
    crate::scope::check_env(typed("--project"), typed("--env"))
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
    name.replace('_', " ")
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

    /// The fixture manifest, with `org member invite`, `org approval deny` and
    /// `uptime show` behind approval so a gated entry takes named flags too, `--env`
    /// among them.
    fn gated() -> crate::published::Published {
        let mut published = crate::published::Published::parse(include_bytes!(
            "../../tests/fixtures/published-manifest.json"
        ))
        .unwrap();
        for command in &mut published.commands {
            if ["org member invite", "org approval deny", "uptime show"]
                .contains(&command.path().as_str())
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

    /// An approval's retry is the command again with its yes: the flags of the run, each
    /// choice, and each value that parses as its kind, as typed; anything else, such as a
    /// member or a reason, as its placeholder.
    #[test]
    fn an_approval_retry_keeps_what_parses_and_withholds_free_text() {
        let published = gated();
        for (argv, line) in [
            (
                &["ployz", "org", "member", "role", "Linus Torvalds", "admin"][..],
                "ployz org member role '<MEMBER>' admin --approval a7e572f3",
            ),
            (
                &[
                    "ployz",
                    "--ployz-config",
                    "/tmp/ployz config.toml",
                    "org",
                    "member",
                    "invite",
                    "linus@example.com",
                    "--role",
                    "admin",
                ],
                "ployz --ployz-config '<PLOYZ_CONFIG>' org member invite '<EMAIL>' --role admin \
                 --approval a7e572f3",
            ),
            (
                &["ployz", "--json", "org", "member", "role", "Linus", "admin"],
                "ployz --json org member role '<MEMBER>' admin --approval a7e572f3",
            ),
            (
                &[
                    "ployz", "org", "member", "role", "Linus", "admin", "--color", "always",
                ],
                "ployz --color always org member role '<MEMBER>' admin --approval a7e572f3",
            ),
            (
                &["ployz", "org", "member", "rm", "--", "--json"],
                "ployz org member rm '<MEMBER>' --approval a7e572f3",
            ),
            (
                &["ployz", "uptime", "show", "web", "--env", "staging"],
                "ployz uptime show web --env staging --approval a7e572f3",
            ),
            (
                &[
                    "ployz",
                    "org",
                    "approval",
                    "deny",
                    "--reason=-no",
                    "--",
                    "-a7",
                ],
                "ployz org approval deny --reason '<REASON>' --approval a7e572f3 -- -a7",
            ),
        ] {
            let root = parsed(&published, argv);
            let tree = crate::published::attach(crate::cli::compiled(), &published);
            let again = again(tree, &root, "a7e572f3").line();
            assert_eq!(again, line, "{argv:?}");
            let words = shell_words::split(&again).unwrap();
            let words: Vec<&str> = words.iter().map(String::as_str).collect();
            parsed(&published, &words);
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
