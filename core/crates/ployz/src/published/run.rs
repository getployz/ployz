//! Running one published command: the parsed arguments go back to Cloud as the
//! entry's input, and the reply prints as the entry's render says.

use clap::ArgMatches;
use ployz_core::RpcError;
use reqwest::Method;
use serde_json::{Map, Value};

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
    let input = input(leaf, command);
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
    if command.typed == Some(Typed::Organization) {
        if !ui::can_prompt() {
            return Err(Failure::command(PublishError::NeedsTerminal {
                path: command.path(),
            })
            .hint(Hint::Next(format!("{}/", setup.cloud))));
        }
        let organization = runtime
            .block_on(cloud_account::acting_in(&credential))
            .map_err(Failure::from)?;
        ui::confirm_name(
            &organization.slug,
            || {
                Failure::command(PublishError::NeedsTerminal {
                    path: command.path(),
                })
            },
            "Cancelled. Nothing changed.",
        )?;
    }
    let key = uuid::Uuid::new_v4().to_string();
    let body = serde_json::json!({ "input": input });
    let ran: super::wire::Ran = runtime
        .block_on(cloud_account::keyed_answer(
            &credential,
            Method::POST,
            &format!("run/{}", command.name),
            &body,
            approval.as_deref(),
            Some(&key),
        ))
        .map_err(|error| refused(error, command, setup))?;
    print(command, ran.output, ran.message)
}

fn refused(error: StoreCallError, command: &PublishedCommand, setup: &Setup) -> Failure {
    match error {
        StoreCallError::Refused(RpcError { code, .. }) if code.as_str() == UNKNOWN_COMMAND => {
            setup.evict(&command.name);
            Failure::command(PublishError::Gone {
                path: command.path(),
            })
            .hint(Hint::Inspect("ployz --help".to_owned()))
        }
        StoreCallError::Refused(refusal) if crate::approval::Asked::of(&refusal).is_some() => {
            let asked = crate::approval::Asked::of(&refusal).expect("checked");
            let retry = format!("ployz {} --approval {}", command.path(), asked.id);
            Failure::from(refusal).hint(Hint::Retry(retry))
        }
        error @ (StoreCallError::Refused(_)
        | StoreCallError::Cloud(_)
        | StoreCallError::Stopped(_)) => error.into(),
    }
}

fn leaf(mut matches: &ArgMatches) -> &ArgMatches {
    while let Some((_, child)) = matches.subcommand() {
        matches = child;
    }
    matches
}

/// The entry's input from what was typed, as its schema shapes it. An argument left
/// out stays out, so Cloud applies its own default.
fn input(leaf: &ArgMatches, command: &PublishedCommand) -> Value {
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
    Value::Object(input)
}

fn print(
    command: &PublishedCommand,
    output: Value,
    message: Option<String>,
) -> Result<(), Failure> {
    match command.render {
        Render::Message => ui::done(&output, message.unwrap_or_default()),
        Render::Record => {
            let mut fields = ui::Fields::new();
            for name in properties(&command.output) {
                fields.push(label(&name), cell(output.get(&name)));
            }
            ui::fields(&output, &fields)
        }
        Render::Table => {
            let columns = properties(command.output.get("items").unwrap_or(&Value::Null));
            let mut table = ui::Table::new(
                columns
                    .iter()
                    .map(|column| column.replace('_', " ").to_uppercase()),
                format!("Nothing from ployz {} yet.", command.path()),
            );
            for row in output.as_array().into_iter().flatten() {
                table.row(columns.iter().map(|column| cell(row.get(column))));
            }
            ui::list(&output, &table)
        }
    }
}

/// Property names in the order the schema lists them.
fn properties(schema: &Value) -> Vec<String> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default()
}

fn label(name: &str) -> String {
    let mut label = name.replace('_', " ");
    if let Some(first) = label.get(..1) {
        let first = first.to_uppercase();
        label.replace_range(..1, &first);
    }
    label
}

fn cell(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| cell(Some(item)))
            .collect::<Vec<_>>()
            .join(", "),
        Some(other) => other.to_string(),
    }
}
