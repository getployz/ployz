//! Published entries as clap subcommands, so `ployz --help`, completion, and parsing
//! treat them like compiled ones.

use clap::builder::PossibleValuesParser;
use clap::{Arg, ArgAction, Command};

use super::{ArgKind, Published, PublishedArg, PublishedCommand};

/// Graft every published entry under its path, creating groups on the way. A path
/// that meets a compiled leaf, at its end or on the way, is dropped.
pub(crate) fn attach(root: Command, published: &Published) -> Command {
    published
        .commands
        .iter()
        .fold(root, |root, command| graft(root, &command.path, command))
}

fn graft(parent: Command, path: &[String], command: &PublishedCommand) -> Command {
    let [name, rest @ ..] = path else {
        return parent;
    };
    match parent.find_subcommand(name).map(Command::has_subcommands) {
        None if rest.is_empty() => parent.subcommand(leaf(name, command)),
        None => parent.subcommand(graft(Command::new(name.clone()), rest, command)),
        Some(true) if !rest.is_empty() => {
            parent.mut_subcommand(name.clone(), |group| graft(group, rest, command))
        }
        Some(_) => parent,
    }
}

fn leaf(name: &str, command: &PublishedCommand) -> Command {
    let mut leaf = Command::new(name.to_owned())
        .about(command.about.clone())
        .args(command.args.iter().map(arg));
    if command.asks() && !command.args.iter().any(|arg| arg.name == "approval") {
        leaf = leaf.arg(crate::cli::approval());
    }
    if !command.examples.is_empty() {
        let examples = command
            .examples
            .iter()
            .map(|example| format!("  {example}"))
            .collect::<Vec<_>>()
            .join("\n");
        leaf = leaf.after_help(format!("Examples:\n{examples}"));
    }
    leaf
}

fn arg(arg: &PublishedArg) -> Arg {
    let mut clap = Arg::new(arg.name.clone())
        .required(arg.required)
        .action(ArgAction::Set);
    if arg.positional {
        clap = clap.value_name(arg.name.to_uppercase());
    } else {
        clap = clap
            .long(arg.name.clone())
            .value_name(arg.name.to_uppercase());
    }
    if let Some(help) = &arg.help {
        clap = clap.help(help.clone());
    }
    match &arg.kind {
        ArgKind::Text => clap,
        ArgKind::Integer => clap.value_parser(clap::value_parser!(i64)),
        ArgKind::Boolean => clap
            .value_parser(clap::value_parser!(bool))
            .value_name("true|false"),
        ArgKind::Choice(choices) => clap.value_parser(PossibleValuesParser::new(choices.clone())),
        ArgKind::Texts if arg.positional => clap.num_args(1..).action(ArgAction::Append),
        ArgKind::Texts => clap.action(ArgAction::Append),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/published-manifest.json");

    fn published() -> Published {
        Published::parse(FIXTURE).unwrap()
    }

    fn leaf<'a>(root: &'a Command, path: &[String]) -> &'a Command {
        path.iter().fold(root, |command, name| {
            command
                .find_subcommand(name)
                .unwrap_or_else(|| panic!("{} is not under {}", name, command.get_name()))
        })
    }

    #[test]
    fn every_fixture_entry_becomes_a_leaf_with_one_argument_per_property() {
        let published = published();
        let mut root = attach(crate::cli::compiled(), &published);
        root.build();
        for command in &published.commands {
            let leaf = leaf(&root, &command.path);
            assert!(!leaf.has_subcommands(), "{} is a leaf", command.path());
            assert_eq!(
                leaf.get_about().map(ToString::to_string),
                Some(command.about.clone())
            );
            let own: Vec<&Arg> = leaf
                .get_arguments()
                .filter(|arg| !arg.is_global_set() && arg.get_long() != Some("help"))
                .collect();
            let properties = command
                .input
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .unwrap();
            assert_eq!(own.len(), properties.len(), "{}", command.path());
            for arg in own {
                let name = arg.get_id().as_str();
                assert!(
                    properties.contains_key(name),
                    "{} has no property {name}",
                    command.path()
                );
                let positional = command.args.iter().any(|a| a.name == name && a.positional);
                assert_eq!(arg.is_positional(), positional, "{} {name}", command.path());
            }
        }
    }

    #[test]
    fn a_published_leaf_that_a_compiled_one_already_owns_is_dropped() {
        let mut published = published();
        let first = published.commands.first_mut().unwrap();
        first.path = vec!["server".into(), "ls".into()];
        first.about = "published impostor".into();
        let root = attach(crate::cli::compiled(), &published);
        let compiled = crate::cli::compiled();
        let ls = |root: &Command| {
            root.find_subcommand("server")
                .unwrap()
                .find_subcommand("ls")
                .unwrap()
                .get_about()
                .map(ToString::to_string)
        };
        assert_eq!(ls(&root), ls(&compiled));
    }

    #[test]
    fn a_published_path_through_a_compiled_leaf_is_dropped() {
        let mut published = published();
        let first = published.commands.first_mut().unwrap();
        first.path = vec!["server".into(), "ls".into(), "extra".into()];
        let root = attach(crate::cli::compiled(), &published);
        let ls = root
            .find_subcommand("server")
            .unwrap()
            .find_subcommand("ls")
            .unwrap();
        assert!(!ls.has_subcommands(), "a compiled leaf takes no subcommand");
    }

    #[test]
    fn a_boolean_property_takes_a_value_and_never_acts_as_a_switch() {
        let published = published();
        let command = published
            .commands
            .iter()
            .find(|command| command.args.iter().any(|arg| arg.kind == ArgKind::Boolean))
            .expect("the fixture carries a boolean property");
        let flag = command
            .args
            .iter()
            .find(|arg| arg.kind == ArgKind::Boolean)
            .unwrap();
        let root = attach(crate::cli::compiled(), &published);
        let mut argv = vec!["ployz".to_owned()];
        argv.extend(command.path.iter().cloned());
        argv.push(format!("--{}", flag.name));
        assert!(
            root.clone().try_get_matches_from(&argv).is_err(),
            "a bare switch"
        );
        argv.push("false".to_owned());
        let matches = root.try_get_matches_from(&argv).unwrap();
        let leaf = command
            .path
            .iter()
            .fold(&matches, |m, name| m.subcommand_matches(name).unwrap());
        assert_eq!(leaf.get_one::<bool>(&flag.name), Some(&false));
    }
}
