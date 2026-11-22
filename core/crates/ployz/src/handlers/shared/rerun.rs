//! The commands a hint tells the reader to run. A rerun is rebuilt from what the command
//! line gave; a typed command is what was typed, as `--yes` confirms it.

use clap::ArgMatches;

/// `--ployz-config PATH` when the command line gave it. A default or `PLOYZ_CONFIG`
/// holds for the next command too, and a quoted `~` would not expand.
pub(crate) fn config_flag(matches: &ArgMatches) -> Option<[&str; 2]> {
    (matches.value_source("ployz-config") == Some(clap::parser::ValueSource::CommandLine))
        .then(|| matches.get_one::<String>("ployz-config"))
        .flatten()
        .map(|config| ["--ployz-config", config.as_str()])
}

/// What every rebuilt command starts with: `ployz`, the config the command line gave, and
/// the Cluster it connected through.
fn carried(matches: &ArgMatches) -> impl Iterator<Item = &str> + Clone {
    ["ployz"]
        .into_iter()
        .chain(config_flag(matches).into_iter().flatten())
        .chain(
            matches
                .get_one::<String>("connect")
                .into_iter()
                .flat_map(|connect| ["--connect", connect.as_str()]),
        )
}

/// `args` as the same Cluster would run them, under the context this command was given.
pub(in crate::handlers) fn rerun(matches: &ArgMatches, args: &[&str]) -> String {
    let context = matches.try_get_one::<String>("context").ok().flatten();
    shell_words::join(
        carried(matches).chain(args.iter().copied()).chain(
            context
                .map(|context| ["--context", context.as_str()])
                .into_iter()
                .flatten(),
        ),
    )
}

/// `command` under `context`, which this command never typed.
pub(in crate::handlers) fn recovery_command(
    matches: &ArgMatches,
    context: &str,
    command: &[&str],
) -> String {
    let args = carried(matches)
        .chain(command.iter().copied())
        .chain(["--context", context]);
    debug_assert!(
        crate::cli::command()
            .try_get_matches_from(args.clone())
            .is_ok_and(|matches| {
                matches.value_source("context") != Some(clap::parser::ValueSource::CommandLine)
                    || crate::handlers::takes_value(
                        crate::handlers::command_path(&matches).split(' '),
                        "context",
                    )
            }),
        "recovery hint does not parse or accept its context: {command:?}"
    );
    shell_words::join(args)
}

#[cfg(test)]
mod tests {
    use crate::handlers::{leaf_matches, recovery_command, rerun, typed_with};

    fn matches(args: &[&str]) -> clap::ArgMatches {
        crate::cli::command().try_get_matches_from(args).unwrap()
    }

    #[test]
    fn a_rerun_carries_what_the_command_line_gave_and_puts_context_last() {
        let typed = matches(&[
            "ployz",
            "--ployz-config",
            "/tmp/a config.yaml",
            "--connect",
            "ssh://root@entry",
            "server",
            "set",
            "edge",
            "--accepts-ingress=true",
            "--context",
            "staging",
        ]);
        assert_eq!(
            rerun(
                leaf_matches(&typed),
                &["server", "set", "edge", "--accepts-ingress=true"]
            ),
            "ployz --ployz-config '/tmp/a config.yaml' --connect ssh://root@entry \
             server set edge '--accepts-ingress=true' --context staging"
        );
    }

    #[test]
    fn a_rerun_drops_a_config_the_command_line_did_not_give() {
        let typed = matches(&["ployz", "server", "ls"]);
        assert_eq!(
            rerun(leaf_matches(&typed), &["server", "ls"]),
            "ployz server ls"
        );
    }

    #[test]
    fn a_recovery_names_the_context_it_was_given_after_the_command() {
        let typed = matches(&[
            "ployz",
            "--ployz-config",
            "/tmp/a config.yaml",
            "--connect",
            "ssh://root@entry",
            "server",
            "add",
            "--standalone",
            "root@host",
            "--context",
            "typed",
        ]);
        assert_eq!(
            recovery_command(
                leaf_matches(&typed),
                "staging",
                &["server", "set", "edge", "--accepts-ingress=true"]
            ),
            "ployz --ployz-config '/tmp/a config.yaml' --connect ssh://root@entry \
             server set edge '--accepts-ingress=true' --context staging"
        );
    }

    #[test]
    fn a_typed_command_keeps_what_was_typed_and_appends_its_extra() {
        let command = typed_with(&["--yes", "two words"]);
        assert!(command.starts_with("ployz "), "{command}");
        assert!(command.ends_with(" --yes 'two words'"), "{command}");
    }
}
