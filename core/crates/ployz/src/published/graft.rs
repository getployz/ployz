//! Published entries as clap subcommands, so `ployz --help`, completion, and parsing
//! treat them like compiled ones.

use std::collections::BTreeSet;

use clap::builder::PossibleValuesParser;
use clap::{Arg, ArgAction, Command};

use super::{ArgKind, Published, PublishedArg, PublishedCommand};

/// Graft every published entry under its path, creating groups on the way. A path
/// that meets a compiled leaf, at its end or on the way, is dropped.
pub(crate) fn attach(root: Command, published: &Published) -> Command {
    let groups = published
        .commands
        .iter()
        .flat_map(|command| {
            let groups = command
                .path
                .split_last()
                .map_or(&[][..], |(_, groups)| groups);
            groups.iter().scan(Vec::new(), |path, name| {
                path.push(name.clone());
                Some(path.clone())
            })
        })
        .filter(|path| find(&root, path).is_none())
        .collect::<BTreeSet<_>>();
    let root = published
        .commands
        .iter()
        .fold(root, |root, command| graft(root, &command.path, command));
    groups.iter().fold(root, |root, path| described(root, path))
}

fn find<'a>(root: &'a Command, path: &[String]) -> Option<&'a Command> {
    path.iter()
        .try_fold(root, |command, name| command.find_subcommand(name))
}

/// A group only Cloud published has no description of its own. The CLI describes top-level
/// `uptime`, and any other such group lists its commands.
fn described(parent: Command, path: &[String]) -> Command {
    let [name, rest @ ..] = path else {
        let commands = parent
            .get_subcommands()
            .map(|command| command.get_name().to_owned())
            .collect::<Vec<_>>()
            .join(", ");
        return parent.about(format!("Commands: {commands}"));
    };
    if name == "uptime" && rest.is_empty() {
        return parent.mut_subcommand(name.clone(), |group| {
            group.about("See whether visitors reach a Service, from every region")
        });
    }
    parent.mut_subcommand(name.clone(), |group| described(group, rest))
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
    let mut help = Vec::new();
    if !command.examples.is_empty() {
        let examples = command
            .examples
            .iter()
            .map(|example| format!("  {example}"))
            .collect::<Vec<_>>()
            .join("\n");
        help.push(format!("Examples:\n{examples}"));
    }
    if let Some(permission) = &command.requires {
        help.push(format!("Needs {permission}"));
    }
    if !help.is_empty() {
        leaf = leaf.after_help(help.join("\n\n"));
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
        // Cloud's `environment` input is `--env`, as on every compiled command.
        let long = if arg.name == "environment" {
            "env"
        } else {
            arg.name.as_str()
        };
        clap = clap
            .long(long.to_owned())
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
                .filter(|arg| !(command.asks() && arg.get_id() == "approval"))
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
    fn a_published_environment_is_the_env_flag() {
        let root = attach(crate::cli::compiled(), &published());
        let matches = root
            .try_get_matches_from(["ployz", "uptime", "show", "web", "--env", "staging"])
            .unwrap();
        let (_, uptime) = matches.subcommand().unwrap();
        let (_, show) = uptime.subcommand().unwrap();
        assert_eq!(
            show.get_one::<String>("environment").map(String::as_str),
            Some("staging")
        );
    }

    #[test]
    fn uptime_says_what_it_is_for() {
        let root = attach(crate::cli::compiled(), &published());
        let about = root
            .find_subcommand("uptime")
            .and_then(Command::get_about)
            .map(ToString::to_string);
        assert_eq!(
            about.as_deref(),
            Some("See whether visitors reach a Service, from every region")
        );
    }

    #[test]
    fn a_published_group_lists_its_commands() {
        let published = published();
        let root = attach(crate::cli::compiled(), &published);
        for command in &published.commands {
            let Some((_, groups)) = command.path.split_last() else {
                continue;
            };
            let mut group = &root;
            for name in groups {
                group = group.find_subcommand(name).unwrap();
                assert!(group.get_about().is_some(), "{}", command.path());
            }
            if let Some(about) = group.get_about().map(ToString::to_string)
                && about.starts_with("Commands: ")
            {
                let leaf = command.path.last().unwrap();
                assert!(about.contains(leaf.as_str()), "{about} lists {leaf}");
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
