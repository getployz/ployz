use std::{
    io::{self, IsTerminal, Write},
    path::Path,
};

use clap::ArgMatches;

use crate::context::{Config, RemovedContext, expand_home};

use super::{Error, leaf_matches, required};

fn config(matches: &ArgMatches) -> Result<Config, Error> {
    if matches
        .get_one::<String>("connect")
        .is_some_and(|value| !value.is_empty())
    {
        return Err(Error::usage(
            "context management is unavailable with a direct connection",
        ));
    }
    let path = matches
        .get_one::<String>("ployz-config")
        .map(Path::new)
        .map(expand_home)
        .ok_or_else(|| Error::usage("Ployz config path is required"))?;
    Ok(Config::load_or_empty(path)?)
}

pub(super) fn list(matches: &ArgMatches) -> Result<(), Error> {
    let config = config(matches)?;
    if config.contexts.is_empty() {
        println!("No contexts found");
        return Ok(());
    }
    println!("NAME\tCURRENT\tCONNECTIONS");
    for (name, context) in &config.contexts {
        let current = if Some(name.as_str()) == config.current_context() {
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
        println!("{}", config.current_context().unwrap_or(""));
    }
    Ok(())
}

pub(super) fn select(matches: &ArgMatches, requested: Option<&str>) -> Result<(), Error> {
    let mut config = config(matches)?;
    if config.contexts.is_empty() {
        return Err(Error::usage(format!(
            "no contexts found in Ployz config {}",
            config.path().display()
        )));
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
                    .position(|name| Some(name.as_str()) == config.current_context()),
            )?;
            names
                .get(index)
                .expect("selection came from the context list")
                .to_string()
        }
    };
    config.set_current_context(Some(selected.clone()))?;
    config.save()?;
    println!("Current context is now {}.", selected.escape_debug());
    Ok(())
}

pub(super) fn remove(matches: &ArgMatches) -> Result<(), Error> {
    let mut config = config(matches)?;
    let name = required(leaf_matches(matches), "context-name")?;
    let removed = config.remove_context(&name)?;
    config.save()?;
    println!("Removed context {}.", name.escape_debug());
    if removed == RemovedContext::Current {
        println!("Current context is now unset.");
    }
    Ok(())
}

pub(super) fn connection(matches: &ArgMatches, requested: Option<&str>) -> Result<(), Error> {
    let mut config = config(matches)?;
    let Some(name) = config.current_context().map(str::to_owned) else {
        return Err(Error::usage(format!(
            "current context is not set in Ployz config {}",
            config.path().display()
        )));
    };
    let context = config.contexts.get_mut(&name).ok_or_else(|| {
        Error::usage(format!("current context {} not found", name.escape_debug()))
    })?;
    if context.connections.is_empty() {
        return Err(Error::usage(format!(
            "no connections found in context {}",
            name.escape_debug()
        )));
    }
    let Some(requested) = requested else {
        println!(
            "{}",
            context
                .connections
                .first()
                .expect("the current context has a connection")
        );
        return Ok(());
    };
    let index = context
        .connections
        .iter()
        .position(|connection| connection.to_string() == requested)
        .ok_or_else(|| Error::usage(format!("connection {requested:?} not found")))?;
    context.select_connection(index);
    let selected = context
        .connections
        .first()
        .expect("a connection was selected")
        .to_string();
    config.save()?;
    println!(
        "Default connection for context {} is now {}.",
        name.escape_debug(),
        selected.escape_debug()
    );
    Ok(())
}

fn prompt<'a>(
    title: &str,
    choices: impl Iterator<Item = &'a str>,
    default: Option<usize>,
) -> Result<usize, Error> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Error::usage(format!(
            "cannot {title} interactively without a terminal"
        )));
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
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let index = if input.trim().is_empty() {
        default.ok_or_else(|| Error::usage("a selection is required"))?
    } else {
        input
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|number| number.checked_sub(1))
            .filter(|index| *index < choices.len())
            .ok_or_else(|| Error::usage("invalid selection"))?
    };
    Ok(index)
}
