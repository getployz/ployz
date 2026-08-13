use std::{
    io::{self, IsTerminal, Write},
    path::Path,
};

use clap::ArgMatches;

use crate::context::{Config, expand_home};

use super::Error;

fn config(matches: &ArgMatches) -> Result<Config, Error> {
    if matches
        .get_one::<String>("connect")
        .is_some_and(|value| !value.is_empty())
    {
        return Err("context management is unavailable with a direct connection".into());
    }
    let path = matches
        .get_one::<String>("ployz-config")
        .map(Path::new)
        .map(expand_home)
        .ok_or_else(|| "Ployz config path is required".to_owned())?;
    Config::load_or_empty(path).map_err(|error| error.to_string())
}

pub(super) fn list(matches: &ArgMatches) -> Result<(), Error> {
    let config = config(matches)?;
    if config.contexts.is_empty() {
        println!("No contexts found");
        return Ok(());
    }
    println!("NAME\tCURRENT\tCONNECTIONS");
    for (name, context) in &config.contexts {
        let current = if name == &config.current_context {
            "*"
        } else {
            ""
        };
        println!("{name}\t{current}\t{}", context.connections.len());
    }
    Ok(())
}

pub(super) fn show(matches: &ArgMatches) -> Result<(), Error> {
    let config = config(matches)?;
    if !config.contexts.is_empty() {
        println!("{}", config.current_context);
    }
    Ok(())
}

pub(super) fn select(matches: &ArgMatches, requested: Option<&str>) -> Result<(), Error> {
    let mut config = config(matches)?;
    if config.contexts.is_empty() {
        return Err(format!(
            "no contexts found in Ployz config {}",
            config.path().display()
        ));
    }
    let selected = match requested {
        Some(name) => name.to_owned(),
        None => {
            let names = config.contexts.keys().collect::<Vec<_>>();
            let index = prompt(
                "Select a context",
                names.iter().map(|name| name.as_str()),
                names
                    .iter()
                    .position(|name| name.as_str() == config.current_context),
            )?;
            names
                .get(index)
                .expect("selection came from the context list")
                .to_string()
        }
    };
    if !config.contexts.contains_key(&selected) {
        return Err(format!("context {selected:?} not found"));
    }
    config.current_context = selected.clone();
    config.save().map_err(|error| error.to_string())?;
    println!("Current context is now {selected:?}.");
    Ok(())
}

pub(super) fn select_connection(matches: &ArgMatches) -> Result<(), Error> {
    let mut config = config(matches)?;
    let name = config.current_context.clone();
    let context = config
        .contexts
        .get_mut(&name)
        .ok_or_else(|| format!("current context {name:?} not found"))?;
    if context.connections.is_empty() {
        return Err(format!("no connections found in context {name:?}"));
    }
    let labels = context
        .connections
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let index = prompt(
        "Select a default connection",
        labels.iter().map(String::as_str),
        Some(0),
    )?;
    context.select_connection(index);
    let selected = context
        .connections
        .first()
        .expect("a connection was selected")
        .to_string();
    config.save().map_err(|error| error.to_string())?;
    println!("Default connection for context {name:?} is now {selected:?}.");
    Ok(())
}

fn prompt<'a>(
    title: &str,
    choices: impl Iterator<Item = &'a str>,
    default: Option<usize>,
) -> Result<usize, Error> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(format!("cannot {title} interactively without a terminal"));
    }
    let choices = choices.collect::<Vec<_>>();
    println!("{title}:");
    for (index, choice) in choices.iter().enumerate() {
        let marker = if Some(index) == default {
            " (current)"
        } else {
            ""
        };
        println!("  {}. {choice}{marker}", index + 1);
    }
    print!("> ");
    io::stdout().flush().map_err(|error| error.to_string())?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|error| error.to_string())?;
    let index = if input.trim().is_empty() {
        default.ok_or_else(|| "a selection is required".to_owned())?
    } else {
        input
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|number| number.checked_sub(1))
            .filter(|index| *index < choices.len())
            .ok_or_else(|| "invalid selection".to_owned())?
    };
    Ok(index)
}
