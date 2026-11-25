//! The commands a failure or a stream line names next.

use clap::ArgMatches;
use ployz_store::{EnvironmentName, ProjectName};

/// `--project PROJECT --env ENVIRONMENT`, naming an Environment whatever scope this
/// command was given.
pub(crate) fn scope_args<'a>(
    project: &'a ProjectName,
    environment: &'a EnvironmentName,
) -> [&'a str; 4] {
    ["--project", project.as_str(), "--env", environment.as_str()]
}

/// `ployz WORDS…` in `environment` of `project`.
pub(crate) fn next_in(
    project: &ProjectName,
    environment: &EnvironmentName,
    words: &[&str],
) -> String {
    shell_words::join(
        std::iter::once("ployz")
            .chain(words.iter().copied())
            .chain(scope_args(project, environment)),
    )
}

/// `ployz WORDS…` in the same Project and Environment as this command.
pub(crate) fn next(matches: &ArgMatches, words: &[&str]) -> String {
    let mut next = vec!["ployz".to_owned()];
    next.extend(words.iter().map(|word| (*word).to_owned()));
    for flag in ["project", "env"] {
        // Not every command takes both flags (`env new` has no `--env`).
        if let Ok(Some(value)) = matches.try_get_one::<String>(flag) {
            next.extend([format!("--{flag}"), value.clone()]);
        }
    }
    shell_words::join(next)
}

/// The command that points `reference` at something there: a new value for its
/// variable, or a new copy of its Config file.
pub(crate) fn fix_broken(matches: &ArgMatches, reference: &ployz_store::BrokenReference) -> String {
    match &reference.at {
        ployz_store::ReferenceAt::Variable { service, variable } => next(
            matches,
            &["set", &format!("{service}.env.{variable}=<VALUE>")],
        ),
        ployz_store::ReferenceAt::File { config, file } => next(
            matches,
            &[
                "config",
                "put",
                config.as_str(),
                file.as_str(),
                "--from",
                "<PATH>",
            ],
        ),
    }
}

/// A broken reference and its fix as a stream line, the name escaped and the fix pasteable.
pub(crate) fn broken_line(
    matches: &ArgMatches,
    reference: &ployz_store::BrokenReference,
) -> String {
    format!(
        "{}; deploys skip it until you change it: {}",
        crate::ui::escaped(&reference.to_string()),
        crate::ui::pasteable(&fix_broken(matches, reference))
    )
}
