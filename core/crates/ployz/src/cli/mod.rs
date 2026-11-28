use clap::{Arg, ArgAction, Command, ValueHint};
use ployz_store::ReviewVersion;

use crate::{env, handlers, operator::input::TIME_FORMATS};

mod rerun;
pub(crate) mod shown;

#[cfg(test)]
pub(crate) use rerun::retry_hint;
pub(crate) use rerun::{Rerun, Withheld, withheld};
pub(crate) use shown::{ExistingPath, References, Shown};

#[must_use]
/// The whole tree: every compiled command plus the ones the signed-in Cloud publishes,
/// from the cache beside the config. Completion and `--help` read this; a refresh
/// happens only when a command line misses it.
pub fn command() -> Command {
    let setup = crate::published::Setup::from_env();
    match setup.cached() {
        Some(published) => crate::published::attach(compiled(), &published),
        None => compiled(),
    }
}

/// The commands this binary carries on its own.
pub(crate) fn compiled() -> Command {
    let root = base(
        Command::new("ployz"),
        "Run Services on your Servers: deploy, inspect and manage them",
    )
    .mut_arg("context", |arg| arg.env(None::<&str>).global(true))
    .styles(crate::ui::clap_styles())
    .after_help(handlers::catalog::SIGNPOST)
    .arg(switch("version", Some('V')).help("Print version"))
    .arg(
        switch("json", None)
            .global(true)
            .help("Print the result as one JSON object on stdout; streams print one per line"),
    )
    .arg(
        value("color", None)
            .global(true)
            .value_name("WHEN")
            .value_parser(["auto", "always", "never"])
            .default_value("auto")
            .help("When to color output; overrides NO_COLOR and CLICOLOR"),
    )
    .arg(
        value("why", None)
            .global(true)
            .value_name("REASON")
            .help("Why you are running this: the goal the command serves. Required when a coding agent runs ployz"),
    );
    handlers::verbs::tree(root)
}

pub(crate) fn base(command: Command, about: &'static str) -> Command {
    command.about(about).args(connection_args())
}

fn connection_args() -> [Arg; 4] {
    [
        value("connect", None)
            .env(env::CONNECT)
            .hide_env_values(true)
            .value_parser(withheld(crate::context::shown))
            .help("Reach the Servers through this connection instead of Cloud or a context")
            .global(true),
        value("ssh-timeout", None)
            .value_name("SECONDS")
            .help("SSH setup timeout in seconds (provisioning: network connection only)")
            .long_help("SSH setup timeout in seconds. Management commands use noninteractive authentication. During provisioning, only the network connection is timed; SSH/sudo authentication and installer execution have no deadline.")
            .value_parser(clap::value_parser!(u32).range(1..))
            // A fresh SSH login to a distant Server takes 4–5s on its own (Bali → Sydney measured 4.1–4.7s).
            .default_value(crate::connect::DEFAULT_SSH_TIMEOUT.as_secs().to_string())
            .global(true),
        value("ployz-config", None)
            .env(env::CONFIG)
            .hide_env_values(true)
            .value_parser(config_path_arg)
            .default_value(crate::context::default_config_path())
            .value_hint(ValueHint::FilePath)
            // PLOYZ_CONFIG is the way to point elsewhere; the flag stays for tests.
            .hide(true)
            .global(true),
        value("context", Some('c'))
            .env(env::CONTEXT)
            .hide_env_values(true)
            .help("Act through this local context instead of the current one"),
    ]
}

/// A Ployz config path, handed on as typed: any text but a URL ([`ConfigPath::reads_as_url`]).
fn config_path_arg(typed: &str) -> Result<String, &'static str> {
    match shown::ConfigPath::reads_as_url(typed) {
        true => Err("PLOYZ_CONFIG takes a file path, not a URL"),
        false => Ok(typed.to_owned()),
    }
}

/// `--cloud-url`, or `PLOYZ_CLOUD_URL`: output shows its scheme, host and port alone, and
/// `--help` the variable's name without its value.
pub(crate) fn cloud_url() -> Arg {
    value("cloud-url", None)
        .env(env::CLOUD_URL)
        .hide_env_values(true)
        .value_parser(withheld(|typed| {
            crate::cloud::origin::CloudOrigin::checked(typed)
                .ok()?
                .whole()
        }))
}

/// The Cloud `--cloud-url` or `PLOYZ_CLOUD_URL` names, as typed, refused when Ployz would
/// not store it as given ([`CloudOrigin::checked`]). The refusal names which gave it.
///
/// [`CloudOrigin::checked`]: crate::cloud::origin::CloudOrigin::checked
pub(crate) fn cloud_url_of(
    matches: &clap::ArgMatches,
) -> Result<Option<&str>, crate::failure::Failure> {
    let Some(typed) = matches
        .try_get_one::<Withheld>("cloud-url")
        .ok()
        .flatten()
        .map(Withheld::typed)
    else {
        return Ok(None);
    };
    let named = match matches.value_source("cloud-url") {
        Some(clap::parser::ValueSource::EnvVariable) => env::CLOUD_URL,
        Some(_) | None => "--cloud-url",
    };
    crate::cloud::origin::CloudOrigin::checked(typed)
        .map(|_| Some(typed))
        .map_err(|refused| crate::failure::Failure::usage(refused.sentence(named)))
}

/// The connection `--connect` or `PLOYZ_CONNECT` names, as typed.
pub(crate) fn connection(matches: &clap::ArgMatches) -> Option<&str> {
    matches
        .try_get_one::<Withheld>("connect")
        .ok()
        .flatten()
        .map(Withheld::typed)
}

/// The subcommand words of `matches`, `server add`.
pub(crate) fn command_path(mut matches: &clap::ArgMatches) -> String {
    let mut parts = Vec::new();
    while let Some((name, child)) = matches.subcommand() {
        parts.push(name);
        matches = child;
    }
    parts.join(" ")
}

pub(crate) fn leaf_matches(mut matches: &clap::ArgMatches) -> &clap::ArgMatches {
    while let Some((_, child)) = matches.subcommand() {
        matches = child;
    }
    matches
}

pub(crate) fn config_path(
    matches: &clap::ArgMatches,
) -> Result<std::path::PathBuf, crate::failure::Failure> {
    matches
        .get_one::<String>("ployz-config")
        .map(std::path::Path::new)
        .map(crate::context::expand_home)
        .ok_or_else(|| crate::failure::Failure::usage("Ployz config path is required"))
}

/// Whether the compiled command at `words` takes a value for the option `id`; a
/// positional of the same name is not the option. Read from the command tree: `ArgMatches` refuses unknown ids only in debug builds. The tree is
/// unbuilt, so a global argument still sits on the ancestor that declares it.
pub(crate) fn takes_value<'a>(words: impl IntoIterator<Item = &'a str>, id: &str) -> bool {
    let root = compiled();
    let takes = |arg: &clap::Arg| {
        arg.get_id() == id && arg.get_long().is_some() && arg.get_action().takes_values()
    };
    let mut command = &root;
    let mut inherited = false;
    for word in words {
        let Some(child) = command.find_subcommand(word) else {
            break;
        };
        inherited |= id != "context"
            && command
                .get_arguments()
                .any(|arg| arg.is_global_set() && takes(arg));
        command = child;
    }
    inherited || command.get_arguments().any(takes)
}

/// `args` as the same Cluster would run them, under this command line's own arguments:
/// the line a `next:` or `inspect:` hint names.
pub(crate) fn rerun(matches: &clap::ArgMatches, args: &[&str]) -> String {
    Rerun::then(matches, args).line()
}

/// The values given for `id` on the command line or from the environment, not its default.
pub(crate) fn string_values(matches: &clap::ArgMatches, id: &str) -> Vec<String> {
    if matches.try_contains_id(id).ok() != Some(true) {
        return Vec::new();
    }
    if matches.value_source(id) == Some(clap::parser::ValueSource::DefaultValue) {
        return Vec::new();
    }
    matches
        .try_get_many::<String>(id)
        .ok()
        .flatten()
        .map(|values| values.cloned().collect())
        .unwrap_or_default()
}

/// SSH setup budget shared by every CLI connection path.
pub(crate) fn ssh_timeout(matches: &clap::ArgMatches) -> std::time::Duration {
    std::time::Duration::from_secs(u64::from(
        *matches
            .get_one::<u32>("ssh-timeout")
            .expect("ssh-timeout has a default"),
    ))
}

/// What a command dials Servers with, under its `--ssh-timeout`.
pub(crate) fn connector(
    matches: &clap::ArgMatches,
) -> std::sync::Arc<crate::connect::SystemConnector> {
    std::sync::Arc::new(
        crate::connect::SystemConnector::default().with_ssh_timeout(ssh_timeout(matches)),
    )
}

pub(crate) fn value(name: &'static str, short: Option<char>) -> Arg {
    let arg = Arg::new(name).long(name).action(ArgAction::Set);
    match short {
        Some(short) => arg.short(short),
        None => arg,
    }
}

pub(crate) fn many(name: &'static str, short: Option<char>) -> Arg {
    value(name, short)
        .action(ArgAction::Append)
        .value_delimiter(',')
}

pub(crate) fn repeated(name: &'static str) -> Arg {
    value(name, None).action(ArgAction::Append)
}

pub(crate) fn switch(name: &'static str, short: Option<char>) -> Arg {
    let arg = Arg::new(name).long(name).action(ArgAction::SetTrue);
    match short {
        Some(short) => arg.short(short),
        None => arg,
    }
}

/// `--yes`/`-y`, which `PLOYZ_AUTO_CONFIRM` also answers, as 1/0, yes/no,
/// true/false or on/off.
pub(crate) fn yes(help: &'static str) -> Arg {
    switch("yes", Some('y'))
        .value_parser(clap::builder::BoolishValueParser::new())
        .env(env::AUTO_CONFIRM)
        .hide_env_values(true)
        .help(help)
}

/// A flag that takes one of `table`'s words and reads back as the value beside it:
/// `matches.get_one::<T>(id)`.
pub(crate) const fn choice<T>(table: &'static [(&'static str, T)]) -> Choice<T> {
    Choice(table)
}

#[derive(Clone)]
pub(crate) struct Choice<T: 'static>(&'static [(&'static str, T)]);

impl<T: Clone + Send + Sync + 'static> clap::builder::TypedValueParser for Choice<T> {
    type Value = T;

    fn parse_ref(
        &self,
        command: &Command,
        arg: Option<&Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<T, clap::Error> {
        // The words' own parser, so a wrong word is refused in clap's usual words.
        let word = clap::builder::PossibleValuesParser::new(self.0.iter().map(|(word, _)| *word))
            .parse_ref(command, arg, value)?;
        Ok(self
            .0
            .iter()
            .find(|(known, _)| *known == word)
            .map(|(_, chosen)| chosen.clone())
            .expect("the words' parser admits only the table's words"))
    }

    fn possible_values(
        &self,
    ) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
        Some(Box::new(
            self.0
                .iter()
                .map(|(word, _)| clap::builder::PossibleValue::new(*word)),
        ))
    }
}

pub(crate) fn positional(name: &'static str, required: bool) -> Arg {
    Arg::new(name).required(required).action(ArgAction::Set)
}

pub(crate) fn trailing(name: &'static str) -> Arg {
    Arg::new(name)
        .num_args(0..)
        .action(ArgAction::Append)
        .allow_hyphen_values(true)
        .trailing_var_arg(true)
}

pub(crate) fn log_flags(command: Command) -> Command {
    command
        .arg(switch("follow", Some('f')).help("Keep streaming new lines"))
        .arg(many("machine", Some('m')).help("Only these Servers"))
        .arg(value("since", None).help(format!("Lines after this time: {TIME_FORMATS}")))
        .arg(
            value("tail", Some('n'))
                .default_value("100")
                .help("Lines from the end of the log; all for everything"),
        )
        .arg(value("until", None).help(format!("Lines before this time: {TIME_FORMATS}")))
        .arg(switch("utc", None).help("Show times in UTC"))
}

pub(crate) fn machine_policy_flags(command: Command) -> Command {
    command
        .arg(
            many("label-add", None)
                .value_name("KEY=VALUE")
                .help("Set the label KEY to VALUE; repeat for more"),
        )
        .args(
            [
                ("accepts-builds", "Whether it runs Builds"),
                ("accepts-services", "Whether it runs Services' Containers"),
                (
                    "accepts-ingress",
                    "Whether it serves web traffic through the Ingress Proxy",
                ),
            ]
            .map(|(name, help)| {
                value(name, None)
                    .value_parser(clap::value_parser!(bool))
                    .help(help)
            }),
        )
}

/// `--expect-version`: a data-loss refusal's version, which its `--accept-volume-loss` binds to.
pub(crate) fn reviewed_version() -> Arg {
    value("expect-version", None)
        .value_name("VERSION")
        .value_parser(review_version)
        .help("Refuse unless this is still the reviewed version; a data-loss refusal names it")
}

/// A review's version, handed on as it was typed; the grammar is [`ReviewVersion`]'s.
fn review_version(version: &str) -> Result<String, &'static str> {
    version
        .parse::<ReviewVersion>()
        .map(|version| version.to_string())
        .map_err(|_| MALFORMED_VERSION)
}

const MALFORMED_VERSION: &str =
    "Use the version ployz diff --json or a refusal gave, such as 3:1:0.1";

pub(crate) fn approval() -> Arg {
    value("approval", None)
        .value_name("DIGEST")
        .help("Go ahead with what you said yes to; the refusal that asks names it")
}

pub(crate) const VOLUME_ACCEPTANCE: &str =
    "Accept permanent deletion: repeat once per exact volume name in the full deletion list";

pub(crate) fn volume_acceptance() -> Arg {
    repeated("accept-volume-loss")
        .num_args(1)
        .value_name("name")
        .help(VOLUME_ACCEPTANCE)
}

pub(crate) fn completion(command: Command) -> Command {
    base(command, "Generate shell completion").arg(
        positional("shell", true)
            .value_parser(clap::value_parser!(clap_complete::Shell))
            .help("The shell to generate it for"),
    )
}

#[cfg(test)]
mod tests {
    /// Every command and argument `--help` shows, without its one-line description.
    fn undescribed(command: &clap::Command, path: &str, blank: &mut Vec<String>) {
        if command.get_about().is_none() {
            blank.push(path.to_owned());
        }
        for arg in command.get_arguments() {
            if !arg.is_hide_set() && arg.get_help().is_none() {
                blank.push(format!("{path} {}", arg.get_id()));
            }
        }
        for sub in command.get_subcommands().filter(|sub| !sub.is_hide_set()) {
            undescribed(sub, &format!("{path} {}", sub.get_name()), blank);
        }
    }

    #[test]
    fn every_command_and_argument_has_a_description() {
        let mut blank = Vec::new();
        let published = crate::published::Published::parse(include_bytes!(
            "../../tests/fixtures/published-manifest.json"
        ))
        .unwrap();
        undescribed(
            &crate::published::attach(super::compiled(), &published),
            "ployz",
            &mut blank,
        );
        assert_eq!(blank, Vec::<String>::new());
    }

    /// Every argument a variable sets, by its command path.
    fn showing_env_values(command: &clap::Command, path: &str, shown: &mut Vec<String>) {
        for arg in command.get_arguments() {
            if arg.get_env().is_some() && !arg.is_hide_env_values_set() {
                shown.push(format!("{path} {}", arg.get_id()));
            }
        }
        for sub in command.get_subcommands() {
            showing_env_values(sub, &format!("{path} {}", sub.get_name()), shown);
        }
    }

    /// `--help` names the variable an argument reads and never its value, which may be a
    /// credential set for another command or pasted into the wrong one.
    #[test]
    fn help_shows_no_variables_value() {
        let mut shown = Vec::new();
        let published = crate::published::Published::parse(include_bytes!(
            "../../tests/fixtures/published-manifest.json"
        ))
        .unwrap();
        showing_env_values(
            &crate::published::attach(super::compiled(), &published),
            "ployz",
            &mut shown,
        );
        assert_eq!(shown, Vec::<String>::new());
    }

    #[test]
    fn expect_version_takes_a_review_or_refusal_version_only() {
        let project = "9a79b1a9-3c26-40dc-8cd0-3e5567c6daca:3764cac4355a8215";
        for good in [
            "3:1:0.1",
            "3:0:0.12:ab12cd",
            project,
            "5:1:0.1#loss-fzfjky",
            "3:0:0.12:ab12cd#loss-0",
        ] {
            assert_eq!(super::review_version(good).as_deref(), Ok(good));
        }
        for bad in [
            "garbage",
            "3:1",
            "3:1:",
            "x:1:0.1",
            "3:1:0.1:",
            "shop:ab12cd",
            "16:1:0.0XYZ",
            "16:1:0.0; rm -rf /",
            "16:1:0.0 trailing",
            "16:1:0.0/extra",
            "16:1:0",
            "3:0:0.12:ab12cd!",
            "5:1:0.1#loss-",
            "5:1:0.1#loss-FZ",
            "5:1:0.1#loss-fz; rm -rf /",
            "5:1:0.1#other-fzfjky",
            "5:1:0.1#loss-a#loss-b",
            "#loss-fzfjky",
            "9a79b1a9-3c26-40dc-8cd0-3e5567c6daca:3764cac4355a8215#loss-fzfjky",
        ] {
            assert!(super::review_version(bad).is_err(), "{bad}");
        }
    }

    /// The grammar takes numbers as Rust parses them and hex digests in either case, and
    /// hands the text on as it was typed.
    #[test]
    fn expect_version_hands_on_what_it_accepted_as_typed() {
        for good in [
            "+3:1:0.1",
            "003:01:0.01",
            "3:0:0.12:ab12cd:FF00",
            "3:1:0.1:ABCDEF#loss-0z",
            "18446744073709551615:0:0.0",
        ] {
            assert_eq!(super::review_version(good).as_deref(), Ok(good));
        }
        for bad in [
            "18446744073709551616:0:0.0",
            "3:1:0.1:xyz",
            "3:1:+0.1.2",
            "3:1:0.1:ab:",
            "3:1:0.1 ",
            "3:1:0.1#loss-a b",
        ] {
            assert!(super::review_version(bad).is_err(), "{bad}");
        }
    }

    fn again(words: &[&str]) -> (clap::ArgMatches, String) {
        let root = super::command()
            .try_get_matches_from(std::iter::once("ployz").chain(words.iter().copied()))
            .unwrap();
        let line = super::Rerun::again(&root).line();
        (root, line)
    }

    /// A retry shows a connection only when it reaches the same place as typed and every
    /// part of it shows: a TCP address, or a Unix socket that exists. Anything else, an SSH
    /// user that may be a session token among it, makes the whole of it `<CONNECT>`, which
    /// doesn't run. The command still reads it as typed.
    #[test]
    fn a_retry_never_repeats_a_connections_secret() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("ployz.sock");
        std::fs::write(&socket, "").unwrap();
        let socket = format!("unix://{}", socket.display());
        for (typed, shown) in [
            ("ployz1:SENTINEL-4564", "'<CONNECT>'"),
            ("management:SENTINEL-4564", "'<CONNECT>'"),
            ("ssh://root:SENTINEL-4564@203.0.113.7:22", "'<CONNECT>'"),
            ("root:SENTINEL-4564@203.0.113.7", "'<CONNECT>'"),
            ("SENTINEL-4564@203.0.113.7", "'<CONNECT>'"),
            ("ssh://SENTINEL-4564@203.0.113.7", "'<CONNECT>'"),
            (
                "tcp://203.0.113.7:7/SENTINEL-4564?t=SENTINEL-4564#SENTINEL-4564",
                "'<CONNECT>'",
            ),
            ("SENTINEL-4564://203.0.113.7", "'<CONNECT>'"),
            ("unix:///SENTINEL-4564", "'<CONNECT>'"),
            ("tcp://203.0.113.7:7", "tcp://203.0.113.7:7"),
            (&socket, &socket),
        ] {
            let (root, line) = again(&["--connect", typed, "status"]);
            assert_eq!(line, format!("ployz --connect {shown} status"), "{typed}");
            assert_eq!(super::connection(super::leaf_matches(&root)), Some(typed));
        }
    }

    /// A flag that takes its value only after `=` gets it that way, or clap reads the value
    /// as a word of its own.
    #[test]
    fn a_retry_joins_a_value_to_a_flag_that_requires_equals() {
        let (_, line) = again(&["volume", "set", "data", "--shared-writes=false"]);
        assert_eq!(line, "ployz volume set data '--shared-writes=false'");
    }

    #[test]
    fn root_version_flags_are_accepted() {
        for flag in ["--version", "-V"] {
            let matches = super::command()
                .try_get_matches_from(["ployz", flag])
                .unwrap();
            assert!(matches.get_flag("version"), "{flag}");
        }
    }

    #[test]
    fn machine_provisioning_defaults_to_cli_version_and_accepts_overrides() {
        for command in ["add"] {
            for version in [None, Some("stable"), Some("beta"), Some("1.2.3")] {
                let mut args = vec!["ployz", "server", command, "root@example.com"];
                if let Some(version) = version {
                    args.extend(["--version", version]);
                }
                let matches = super::command().try_get_matches_from(args).unwrap();
                let matches = matches
                    .subcommand_matches("server")
                    .unwrap()
                    .subcommand_matches(command)
                    .unwrap();
                assert_eq!(
                    matches
                        .get_one::<ployz_core::MachineRelease>("version")
                        .map(ToString::to_string)
                        .as_deref(),
                    Some(version.unwrap_or(env!("CARGO_PKG_VERSION")))
                );
            }
        }
    }

    #[test]
    fn removal_acceptance_requires_explicit_repeatable_volume_flags() {
        assert!(
            super::command()
                .try_get_matches_from([
                    "ployz",
                    "server",
                    "rm",
                    "worker",
                    "--accept-volume-loss",
                    "data",
                ])
                .is_ok()
        );
        assert!(
            super::command()
                .try_get_matches_from([
                    "ployz",
                    "server",
                    "forget",
                    "worker",
                    "--accept-volume-loss",
                    "data",
                ])
                .is_ok()
        );
        assert!(
            super::command()
                .try_get_matches_from(["ployz", "server", "rm", "worker", "--accept-volume-loss"])
                .is_err()
        );
    }
}
