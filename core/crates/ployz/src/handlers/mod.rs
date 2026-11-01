use std::{future::Future, path::Path, pin::Pin};

use crate::cancellation::on_ctrl_c as cancellation_on_ctrl_c;
use clap::{ArgMatches, Command};
use clap_complete::Shell;
use clap_complete::env::Shells;

use crate::failure::Failure;
use ployz_core::MachineId;

pub(crate) mod account;
pub(crate) mod build;
pub(crate) mod catalog;
pub(crate) mod cloud;
mod cloud_read;
pub(crate) mod config;
pub(crate) mod config_item;
pub(crate) mod context;
mod data_loss;
#[cfg(feature = "verify-faults")]
pub(crate) mod debug;
pub(crate) mod deploy;
pub(crate) mod device;
pub(crate) mod docs;
pub(crate) mod domain;
pub(crate) mod env;
pub(crate) mod github;
pub(crate) mod link;
pub(crate) mod login;
pub(crate) mod mcp;
pub(crate) mod metrics;
pub(crate) mod operator;
pub(crate) mod project;
pub(crate) mod recovery;
pub(crate) mod review;
pub(crate) mod server;
pub(crate) mod service;
mod status_servers;
pub(crate) mod store;
mod teardown;
pub(crate) mod up;
pub(crate) mod verbs;
pub(crate) mod volume;

#[doc(hidden)]
pub use crate::provisioning::Installation;
#[doc(hidden)]
pub use cloud::enroll_with_installer as cloud_enroll_with_installer;

pub type Error = Failure;

/// The most bytes the CLI reads of a Config file, env file or secret, so a wrong source
/// such as `/dev/zero` can't fill memory. It bounds only the read: the Store and Cloud
/// judge what was read, as they would without it.
pub(crate) const MAX_CLI_INPUT_BYTES: usize = 32 * 1024 * 1024;

/// The refusal of an `input` past [`MAX_CLI_INPUT_BYTES`], such as `stdin` or a path.
pub(crate) fn input_too_long(input: &str, what: &str) -> Error {
    Error::detailed(
        ployz_core::RpcErrorCode::InvalidArgument,
        format!("{input}: the CLI reads at most 32 MiB of {what}; this one holds more"),
        serde_json::json!({ "input": input, "max_bytes": MAX_CLI_INPUT_BYTES }),
    )
}

/// All of `source`, or `None` when it holds more than `limit` bytes. Reads at most one
/// byte past `limit`, so an endless source such as `/dev/zero` is refused at once.
pub(crate) fn read_bounded(
    source: impl std::io::Read,
    limit: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    let past = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    std::io::Read::read_to_end(&mut source.take(past), &mut bytes)?;
    Ok((bytes.len() <= limit).then_some(bytes))
}

pub fn run() -> Result<(), Error> {
    let mut command = crate::cli::command();
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let before_parse = crate::ui::Mode::resolve(crate::ui::Surroundings::of_process(false));
    crate::ui::Color::requested(&command, &args).apply(before_parse);
    let matches = match parse(&command, &args) {
        Ok(matches) => matches,
        Err(error) => {
            // A word the tree lacks may be a command Cloud published since the
            // cache was written, or one never cached: one refresh, then parse again.
            let Some(published) = published_after_miss(&error, &command, &args)? else {
                return Err(usage_failure(error, &command));
            };
            command = crate::published::attach(crate::cli::compiled(), &published);
            parse(&command, &args).map_err(|error| usage_failure(error, &command))?
        }
    };
    let color = matches
        .get_one::<String>("color")
        .map_or("", String::as_str);
    crate::ui::init(matches.get_flag("json"), crate::ui::Color::named(color));
    let result = dispatch(&matches, &mut command);
    follow_cloud_revision();
    result
}

fn parse(command: &Command, args: &[std::ffi::OsString]) -> Result<ArgMatches, clap::Error> {
    fn without_context_env(command: Command) -> Command {
        command
            .mut_args(|arg| {
                if arg.get_id() == "context" {
                    arg.env(None::<&str>)
                } else {
                    arg
                }
            })
            .mut_subcommands(without_context_env)
    }

    let argv = || std::iter::once(std::ffi::OsString::from("ployz")).chain(args.iter().cloned());
    let matches = command.clone().try_get_matches_from(argv())?;
    if matches.value_source("context") != Some(clap::parser::ValueSource::EnvVariable)
        || verbs::find(&command_path(&matches))
            .is_some_and(|verb| takes_value(verb.path.split(' '), "context"))
    {
        return Ok(matches);
    }
    // Clap propagates group environment values through the root's global argument.
    without_context_env(command.clone()).try_get_matches_from(argv())
}

/// Every `/api/cli` reply names Cloud's manifest revision. One the cache lacks means
/// the next `--help` would be stale, so fetch it now, after the command's own output.
fn follow_cloud_revision() {
    let setup = crate::published::Setup::from_env();
    let Some(seen) = crate::cloud_account::revision_seen(&setup.cloud) else {
        return;
    };
    if setup
        .cached()
        .is_some_and(|published| published.revision == seen)
    {
        return;
    }
    let _ = setup.refresh_now();
}

/// A word no command has may be one Cloud published since; a flag a published command
/// lacks may be one Cloud added since. A flag a compiled command lacks is a typo, and
/// costs no round trip. A bad `PLOYZ_PROJECT` or `PLOYZ_ENV` is refused before the
/// round trip.
fn published_after_miss(
    error: &clap::Error,
    tree: &Command,
    args: &[std::ffi::OsString],
) -> Result<Option<crate::published::Published>, Error> {
    use clap::error::ErrorKind;
    let setup = crate::published::Setup::from_env();
    if !setup.configured {
        return Ok(None);
    }
    let stale = error.kind() == ErrorKind::InvalidSubcommand
        || error.kind() == ErrorKind::UnknownArgument
            && setup
                .cached()
                .is_some_and(|published| published.find(&reached(tree, args)).is_some());
    if !stale {
        return Ok(None);
    }
    if let Err(failure) = crate::published::check_scope_env(args) {
        crate::ui::init(wants_json(args), crate::ui::Color::requested(tree, args));
        return Err(failure);
    }
    Ok(setup.refresh_now().ok())
}

/// The subcommand path `args` reach in `tree`, `server ls`: the command that rejected
/// whatever followed.
fn reached(tree: &Command, args: &[std::ffi::OsString]) -> String {
    let mut command = tree;
    let mut path = Vec::new();
    for word in args
        .iter()
        .take_while(|arg| *arg != "--")
        .filter_map(|arg| arg.to_str())
    {
        if let Some(child) = command.find_subcommand(word) {
            path.push(child.get_name());
            command = child;
        }
    }
    path.join(" ")
}

fn wants_json(args: &[std::ffi::OsString]) -> bool {
    args.iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--json")
}

/// A flag written before the command that takes it, `ployz --project web status`:
/// clap would offer the nearest top-level flag, so name the command's own instead.
/// A flag another command takes, `server show --project`, is one this command lacks,
/// not a typo of its nearest flag, unless that flag is [`close`]: `server ls --wait`
/// keeps clap's `--waiting`. One after `--` is an operand, and keeps clap's error.
fn misplaced_flag(error: clap::Error, tree: &Command, args: &[std::ffi::OsString]) -> clap::Error {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    if error.kind() != ErrorKind::UnknownArgument {
        return error;
    }
    let Some(ContextValue::String(invalid)) = error.get(ContextKind::InvalidArg) else {
        return error;
    };
    let Some(long) = invalid
        .strip_prefix("--")
        .and_then(|flag| flag.split('=').next())
    else {
        return error;
    };
    let named = |arg: &std::ffi::OsString| {
        arg.to_str()
            .and_then(|arg| arg.strip_prefix("--"))
            .and_then(|flag| flag.split('=').next())
            == Some(long)
    };
    if !args.iter().take_while(|arg| *arg != "--").any(named) {
        return error;
    }
    fn takes(command: &Command, long: &str) -> bool {
        command
            .get_arguments()
            .any(|arg| arg.get_long() == Some(long))
    }
    fn anywhere(command: &Command, long: &str) -> bool {
        takes(command, long) || command.get_subcommands().any(|sub| anywhere(sub, long))
    }
    let close_suggested = matches!(
        error.get(ContextKind::SuggestedArg),
        Some(ContextValue::String(suggested)) if suggested
            .strip_prefix("--")
            .is_some_and(|suggested| close(long, suggested))
    );
    let path = reached(tree, args);
    let Some(command) = (!path.is_empty())
        .then(|| {
            path.split(' ')
                .try_fold(tree, |command, name| command.find_subcommand(name))
        })
        .flatten()
    else {
        return error;
    };
    let tip = if takes(command, long) {
        format!("'{path} --{long}' exists; put it after the command")
    } else if !command.has_subcommands() && anywhere(tree, long) && !close_suggested {
        format!("'{path}' takes no --{long}")
    } else {
        return error;
    };
    clap::Error::raw(
        ErrorKind::UnknownArgument,
        format!("unexpected argument '{invalid}' found\n\n  tip: {tip}\n"),
    )
}

/// Whether clap's suggestion is a likely typo fix for `long`: one starts the other
/// (`--wait`, `--waiting`), or they are at most two edits apart (`--reset`, `--preset`).
fn close(long: &str, suggested: &str) -> bool {
    if suggested.starts_with(long) || long.starts_with(suggested) {
        return true;
    }
    // Levenshtein distance, one row at a time.
    let mut previous = (0..=suggested.chars().count()).collect::<Vec<_>>();
    for (i, a) in long.chars().enumerate() {
        let mut row = vec![i + 1];
        for ((b, diagonal), above) in suggested
            .chars()
            .zip(&previous)
            .zip(previous.iter().skip(1))
        {
            let left = row.last().copied().unwrap_or(i + 1);
            row.push(
                (diagonal + usize::from(a != b))
                    .min(above + 1)
                    .min(left + 1),
            );
        }
        previous = row;
    }
    previous.last().is_some_and(|distance| *distance <= 2)
}

/// Report a rejected command line: clap's own rendering, or one JSON error under `--json`.
fn usage_failure(error: clap::Error, tree: &Command) -> Error {
    use clap::error::ErrorKind;
    use std::io::Write as _;
    let wants_json = wants_json(&std::env::args_os().collect::<Vec<_>>());
    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        // clap's own `exit` swallows a closed stdout instead of dying of it, and
        // a full one: `ui::exit` fails a write stdout could not take.
        let mut stdout = crate::ui::stdout();
        let _ = write!(stdout, "{}", error.render().ansi());
        let _ = stdout.flush();
        return Error::exit(0);
    }
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let error = misplaced_flag(operator::container_column_asked(error, &args), tree, &args);
    if !wants_json {
        error.exit();
    }
    crate::ui::init(true, crate::ui::Color::Auto);
    clap_usage(&error)
}

fn clap_usage(error: &clap::Error) -> Error {
    let message = error.render().to_string();
    Error::usage(message.trim().trim_start_matches("error: ").to_owned())
}

fn dispatch(matches: &ArgMatches, command: &mut Command) -> Result<(), Error> {
    if matches.get_flag("version") {
        return crate::ui::finish(
            &serde_json::json!({ "version": env!("CARGO_PKG_VERSION") }),
            || crate::ui::stream(format_args!("{}", env!("CARGO_PKG_VERSION"))),
        );
    }
    if matches.subcommand().is_none() {
        if matches.get_flag("json") {
            return Err(Error::usage("a command is required"));
        }
        crate::ui::stream(command.render_help().ansi());
        return Ok(());
    }
    let path = command_path(matches);
    // Every compiled leaf is a verb, so a missing one is a published leaf or a
    // group without its subcommand: its help, as a bare `ployz` shows the top level's.
    let Some(verb) = verbs::find(&path) else {
        let setup = crate::published::Setup::beside(
            &config_path(leaf_matches(matches))?,
            std::env::var(crate::cli::env::CLOUD_URL).ok(),
        );
        if let Some(cached) = setup.cached()
            && let Some(published) = cached.find(&path)
        {
            if matches.value_source("context") == Some(clap::parser::ValueSource::CommandLine) {
                return Err(Error::usage(format!("ployz {path} doesn't use a context")));
            }
            return crate::published::run(matches, published, &setup);
        }
        if matches.get_flag("json") {
            return Err(Error::usage(format!("ployz {path} requires a subcommand")));
        }
        command.build();
        let group = path
            .split(' ')
            .try_fold(command, |command, name| command.find_subcommand_mut(name))
            .ok_or_else(|| Error::usage(format!("ployz {path} requires a subcommand")))?;
        crate::ui::stream(group.render_help().ansi());
        return Ok(());
    };
    if verb.json == verbs::Json::Refused && matches.get_flag("json") {
        return Err(Error::usage(format!(
            "ployz {path} does not support --json"
        )));
    }
    if matches.value_source("context") == Some(clap::parser::ValueSource::CommandLine)
        && !takes_value(path.split(' '), "context")
    {
        // Store commands address a Project and Environment, never a context.
        let takes = |id| takes_value(path.split(' '), id);
        return Err(Error::usage(if takes("project") {
            format!("ployz {path} doesn't use a context; it takes --project and --env")
        } else {
            format!("ployz {path} doesn't use a context")
        }));
    }
    (verb.run)(matches)
        .map_err(|failure| context::with_context_next(failure, leaf_matches(matches)))
}

/// Whether the compiled command at `words` takes a value for the option `id`; a
/// positional of the same name is not the option. Read from the command tree: `ArgMatches` refuses unknown ids only in debug builds. The tree is
/// unbuilt, so a global argument still sits on the ancestor that declares it.
fn takes_value<'a>(words: impl IntoIterator<Item = &'a str>, id: &str) -> bool {
    let root = crate::cli::compiled();
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

/// Print the shell hook that asks `ployz` itself for completions, so Setting paths
/// and Service names complete from the catalog and the Store.
pub(super) fn completion(root: &ArgMatches) -> Result<(), Error> {
    let shell = leaf_matches(root)
        .get_one::<Shell>("shell")
        .copied()
        .ok_or_else(|| Error::usage("completion shell is required"))?;
    let shells = Shells::builtins();
    let completer = shells
        .completer(&shell.to_string())
        .ok_or_else(|| Error::usage("unsupported completion shell"))?;
    completer.write_registration(
        crate::cli::env::COMPLETE,
        "ployz",
        "ployz",
        "ployz",
        &mut crate::ui::stdout_bytes(),
    )?;
    Ok(())
}

fn command_path(mut matches: &ArgMatches) -> String {
    let mut parts = Vec::new();
    while let Some((name, child)) = matches.subcommand() {
        parts.push(name);
        matches = child;
    }
    parts.join(" ")
}

/// Items as one line of text: `a, b, c`.
pub(crate) fn joined<T: ToString>(items: &[T]) -> String {
    items
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn leaf_matches(mut matches: &ArgMatches) -> &ArgMatches {
    while let Some((_, child)) = matches.subcommand() {
        matches = child;
    }
    matches
}

fn string_values(matches: &ArgMatches, id: &str) -> Vec<String> {
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

fn required(matches: &ArgMatches, name: &str) -> Result<String, Error> {
    matches
        .get_one::<String>(name)
        .cloned()
        .ok_or_else(|| Error::usage(format!("{name} is required")))
}

fn runtime() -> Result<tokio::runtime::Runtime, Error> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

pub(super) fn config_path(matches: &ArgMatches) -> Result<std::path::PathBuf, Error> {
    matches
        .get_one::<String>("ployz-config")
        .map(Path::new)
        .map(crate::context::expand_home)
        .ok_or_else(|| Error::usage("Ployz config path is required"))
}

/// The explicit connection, the selected context, or the local daemon; never Cloud.
async fn connect_context(
    matches: &ArgMatches,
    context: Option<&str>,
) -> Result<crate::connect::Client, Error> {
    Ok(crate::connect::connect_with_ssh_timeout(
        &config_path(matches)?,
        matches.get_one::<String>("connect").map(String::as_str),
        context,
        crate::cli::ssh_timeout(matches),
    )
    .await?)
}

/// Re-establish a management connection after setup has already started.
async fn reconnect_client(
    matches: &ArgMatches,
    context: Option<&str>,
) -> Result<crate::connect::Client, Error> {
    let config = config_path(matches)?;
    let connect = matches.get_one::<String>("connect").map(String::as_str);
    crate::setup_retry::run(
        &mut (),
        "Reconnecting to the Cluster",
        crate::setup_retry::WAIT,
        crate::connect::ConnectError::is_setup_retryable,
        async |_| {
            crate::connect::connect_with_ssh_timeout(
                &config,
                connect,
                context,
                crate::cli::ssh_timeout(matches),
            )
            .await
        },
    )
    .await
    .map_err(Into::into)
}

/// `--ployz-config PATH` when the command line gave it. A default or `PLOYZ_CONFIG`
/// holds for the next command too, and a quoted `~` would not expand.
pub(super) fn config_flag(matches: &ArgMatches) -> Option<[&str; 2]> {
    (matches.value_source("ployz-config") == Some(clap::parser::ValueSource::CommandLine))
        .then(|| matches.get_one::<String>("ployz-config"))
        .flatten()
        .map(|config| ["--ployz-config", config.as_str()])
}

pub(crate) fn typed_with(extra: &[&str]) -> String {
    std::iter::once("ployz".to_owned())
        .chain(std::env::args().skip(1))
        .chain(extra.iter().map(|arg| (*arg).to_owned()))
        .map(|arg| crate::ui::shell_word(&arg).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn rerun(matches: &ArgMatches, args: &[&str]) -> String {
    let connect = matches.get_one::<String>("connect");
    let context = matches.try_get_one::<String>("context").ok().flatten();
    let args = ["ployz"]
        .into_iter()
        .chain(config_flag(matches).into_iter().flatten())
        .chain(
            connect
                .map(|connect| ["--connect", connect.as_str()])
                .into_iter()
                .flatten(),
        )
        .chain(args.iter().copied())
        .chain(
            context
                .map(|context| ["--context", context.as_str()])
                .into_iter()
                .flatten(),
        );
    shell_words::join(args)
}

fn recovery_command(matches: &ArgMatches, context: &str, command: &[&str]) -> String {
    let connect = matches
        .get_one::<String>("connect")
        .into_iter()
        .flat_map(|connect| ["--connect", connect.as_str()]);
    let args = ["ployz"]
        .into_iter()
        .chain(config_flag(matches).into_iter().flatten())
        .chain(connect)
        .chain(command.iter().copied())
        .chain(["--context", context]);
    debug_assert!(
        crate::cli::command()
            .try_get_matches_from(args.clone())
            .is_ok_and(|matches| {
                matches.value_source("context") != Some(clap::parser::ValueSource::CommandLine)
                    || takes_value(command_path(&matches).split(' '), "context")
            }),
        "recovery hint does not parse or accept its context: {command:?}"
    );
    shell_words::join(args)
}

/// The headline for a committed step whose Ingress Proxy deploy failed. A failed deploy's
/// own sentence names what its Container did, so it stays on the headline; the one
/// `cause:` line is then the Container's last line.
pub(crate) fn ingress_headline(stem: &str, error: &Error) -> String {
    match error.diagnostic() {
        Some(_) => format!("{stem}: {error}"),
        None => format!("{stem}."),
    }
}

/// Direct ingress diagnostics acquire their command scope before committed output is serialized.
/// Each Server is selected by Machine ID, since a Machine Name may be ambiguous.
pub(crate) fn ingress_hints(error: Error, command: impl Fn(&[&str]) -> String) -> Error {
    let retry = error
        .diagnostic()
        .and_then(|diagnostic| diagnostic.failed_machine)
        .map(|machine| {
            crate::ui::Hint::Retry(command(&[
                "server",
                "set",
                &machine.to_string(),
                "--accepts-ingress=true",
            ]))
        });
    ingress_log_hints(error, command, ToString::to_string).hint(retry)
}

/// [`ingress_hints`] without the retry, for a failure that has the reader do something else
/// first. `server` selects each Server in the hints.
pub(crate) fn ingress_log_hints(
    mut error: Error,
    command: impl Fn(&[&str]) -> String,
    server: impl Fn(&MachineId) -> String,
) -> Error {
    let Some(diagnostic) = error.diagnostic() else {
        return error;
    };
    let hints = diagnostic
        .logs
        .iter()
        .map(|tail| {
            crate::ui::Hint::Inspect(command(&[
                "logs",
                &tail.service.to_string(),
                "--machine",
                &server(&tail.machine),
            ]))
        })
        .collect::<Vec<_>>();
    for hint in hints {
        error = error.hint(hint);
    }
    error
}

fn with_client<F>(root: &ArgMatches, work: F) -> Result<(), Error>
where
    F: for<'a> FnOnce(
        &'a mut crate::connect::Client,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + 'a>>,
{
    let leaf = leaf_matches(root);
    let context = leaf.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        let mut client = server::connect(leaf, context).await?;
        work(&mut client).await
    })
}

fn with_full_client<F>(root: &ArgMatches, work: F) -> Result<(), Error>
where
    F: for<'a> FnOnce(
        &'a mut crate::connect::Client,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + 'a>>,
{
    let leaf = leaf_matches(root);
    let context = leaf.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        let mut client = server::connect_full(leaf, context).await?;
        work(&mut client).await
    })
}

/// [`with_client`] for a read Cloud also answers: `work` runs against the Servers, or the
/// credential comes back when Cloud refuses it Server access, to ask Cloud instead.
fn with_reader<F>(
    root: &ArgMatches,
    work: F,
) -> Result<Option<crate::cloud_account::Credential>, Error>
where
    F: for<'a> FnOnce(
        &'a mut crate::connect::Client,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + 'a>>,
{
    with_lister(root, || Err(server::no_servers()), work)
}

/// [`with_reader`] for a list: an Organization with no Servers yet runs `empty` instead.
fn with_lister<F>(
    root: &ArgMatches,
    empty: impl FnOnce() -> Result<(), Error>,
    work: F,
) -> Result<Option<crate::cloud_account::Credential>, Error>
where
    F: for<'a> FnOnce(
        &'a mut crate::connect::Client,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + 'a>>,
{
    let leaf = leaf_matches(root);
    let context = leaf.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        match server::reach(leaf, context).await? {
            Some(server::Reach::Dial(mut client)) => work(&mut client).await.map(|()| None),
            Some(server::Reach::Ask(credential)) => Ok(Some(credential)),
            None => empty().map(|()| None),
        }
    })
}

pub(crate) type Handler = fn(&ArgMatches) -> Result<(), Error>;

#[cfg(test)]
mod tests {
    use super::*;

    /// An endless source that counts the bytes it serves.
    struct Endless(usize);

    impl std::io::Read for &mut Endless {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            buf.fill(b'a');
            self.0 += buf.len();
            Ok(buf.len())
        }
    }

    #[test]
    fn a_bounded_read_takes_at_most_one_byte_past_its_limit() {
        for limit in [0, 1, 4096, 256 * 1024] {
            let mut source = Endless(0);
            assert_eq!(read_bounded(&mut source, limit).unwrap(), None);
            assert!(source.0 <= limit + 1, "{limit}: read {}", source.0);
        }
        let exact = read_bounded(&[b'a'; 10][..], 10).unwrap();
        assert_eq!(exact.as_deref(), Some(&[b'a'; 10][..]));
    }

    #[test]
    fn an_explicit_context_names_where_a_cloud_command_takes_its_scope() {
        let mut command = command();
        for (args, expected) in [
            (
                &["config", "ls"][..],
                "ployz config ls doesn't use a context; it takes --project and --env",
            ),
            (&["org", "ls"], "ployz org ls doesn't use a context"),
            // A positional named `project` is not the --project option.
            (
                &["project", "rename", "old", "new"],
                "ployz project rename doesn't use a context",
            ),
            // `history` hands its global --project and --env down to its actions.
            (
                &["history", "restore", "1"],
                "ployz history restore doesn't use a context; it takes --project and --env",
            ),
            (
                &["history", "undo", "1"],
                "ployz history undo doesn't use a context; it takes --project and --env",
            ),
        ] {
            let matches = command
                .clone()
                .try_get_matches_from(["ployz", "-c", "prod"].iter().chain(args))
                .unwrap();
            assert_eq!(
                dispatch(&matches, &mut command).unwrap_err().to_string(),
                expected,
            );
        }
    }

    #[test]
    fn a_flag_typed_before_its_command_names_the_command() {
        let tip = |args: &[&str]| {
            let tree = crate::cli::compiled();
            let args = args
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>();
            let error = tree
                .clone()
                .try_get_matches_from(std::iter::once("ployz".into()).chain(args.clone()))
                .unwrap_err();
            misplaced_flag(error, &tree, &args).to_string()
        };
        let error = tip(&["--project", "zs1", "status"]);
        assert!(error.contains("tip: 'status --project' exists"), "{error}");
        assert!(!error.contains("--connect"), "{error}");
        let error = tip(&["--project=zs1", "service", "ls"]);
        assert!(
            error.contains("tip: 'service ls --project' exists"),
            "{error}"
        );
        let error = tip(&["--nonsense", "status"]);
        assert!(!error.contains("exists; put it after"), "{error}");
        let error = tip(&["get", "--", "web.image", "--project", "p"]);
        assert!(!error.contains("put it after"), "{error}");
        let error = tip(&["server", "show", "web-1", "--project", "p"]);
        assert!(
            error.contains("tip: 'server show' takes no --project"),
            "{error}"
        );
        assert!(!error.contains("--connect"), "{error}");
        let error = tip(&["server", "show", "web-1", "--conect", "p"]);
        assert!(error.contains("--connect"), "{error}");
    }

    #[test]
    fn a_flag_of_another_command_keeps_clap_suggestion_of_a_close_one() {
        let rejected = |args: &[&str]| {
            let tree = crate::cli::compiled();
            let args = args
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>();
            let error = tree
                .clone()
                .try_get_matches_from(std::iter::once("ployz".into()).chain(args.clone()))
                .unwrap_err();
            misplaced_flag(error, &tree, &args).to_string()
        };
        let error = rejected(&["server", "ls", "--wait"]);
        assert!(
            error.contains("tip: a similar argument exists: '--waiting'"),
            "{error}"
        );
        assert!(!error.contains("takes no"), "{error}");
        let error = rejected(&["server", "token", "create", "--expires-in", "7d"]);
        assert!(
            error.contains("tip: a similar argument exists: '--expires'"),
            "{error}"
        );
        assert!(!error.contains("takes no"), "{error}");
        let error = rejected(&["token", "new", "ci", "--reset", "read"]);
        assert!(
            error.contains("tip: a similar argument exists: '--preset'"),
            "{error}"
        );
        assert!(!error.contains("takes no"), "{error}");
        let error = rejected(&["server", "show", "web-1", "--project", "p"]);
        assert!(
            error.contains("tip: 'server show' takes no --project"),
            "{error}"
        );
        assert!(!error.contains("--connect"), "{error}");
    }

    fn command() -> Command {
        fn isolate(command: Command) -> Command {
            command
                .mut_args(|arg| {
                    if arg.get_id() == "ployz-config" {
                        arg.env(None::<&str>)
                            .default_value("/tmp/ployz-handler-tests/config.yaml")
                    } else {
                        arg
                    }
                })
                .mut_subcommands(isolate)
        }

        isolate(crate::cli::command())
    }

    #[test]
    fn setup_recovery_preserves_config_and_context_and_parses_as_a_command() {
        let matches = command()
            .try_get_matches_from([
                "ployz",
                "--ployz-config",
                "/tmp/a config.yaml",
                "server",
                "add",
                "--standalone",
                "root@host",
                "--context",
                "staging",
            ])
            .unwrap();
        let recovery = recovery_command(
            leaf_matches(&matches),
            "staging",
            &["server", "set", "edge", "--accepts-ingress=true"],
        );
        let args = shell_words::split(&recovery).unwrap();
        let parsed = command().try_get_matches_from(args).unwrap();
        let leaf = leaf_matches(&parsed);
        assert_eq!(
            leaf.get_one::<String>("context").map(String::as_str),
            Some("staging")
        );
        assert_eq!(
            leaf.get_one::<String>("ployz-config").map(String::as_str),
            Some("/tmp/a config.yaml")
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "recovery hint does not parse")]
    fn setup_recovery_refuses_a_command_outside_the_tree() {
        let matches = command()
            .try_get_matches_from(["ployz", "server", "add", "--standalone", "root@host"])
            .unwrap();
        recovery_command(leaf_matches(&matches), "staging", &["no-such", "command"]);
    }

    #[test]
    fn context_config_environment_reaches_only_context_aware_leaves() {
        if std::env::var_os("PLOYZ_CONTEXT").as_deref() != Some(std::ffi::OsStr::new("b")) {
            let home = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "handlers::tests::context_config_environment_reaches_only_context_aware_leaves",
                    "--nocapture",
                ])
                .env_clear()
                .env("HOME", home.path())
                .env("PLOYZ_CONTEXT", "b")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        for (args, context) in [
            (&["volume", "add", "data"][..], None),
            (&["volume", "rm", "data"], None),
            (&["service", "rm", "web"], None),
            (&["set", "web.image=web:2"], None),
            (&["org", "ls"], None),
            (&["ctx", "use", "other"], None),
            (&["ctx"], Some("b")),
            (&["server", "ls"], Some("b")),
            (&["ps"], Some("b")),
            (&["deploy"], Some("b")),
            (&["device", "request"], None),
            (&["device", "ls"], Some("b")),
            (&["device", "rm", "mmg42klgyq"], Some("b")),
            (&["device", "approve", "mmg42klgyq"], Some("b")),
        ] {
            let argv = args
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>();
            let root = parse(&crate::cli::command(), &argv).unwrap();
            let mut leaf = &root;
            while let Some((_, child)) = leaf.subcommand() {
                leaf = child;
            }
            assert_eq!(
                leaf.ids().any(|id| id == "context"),
                context.is_some(),
                "{args:?}"
            );
            assert_eq!(rerun(leaf, args).contains("--context"), context.is_some());
            if context.is_some() {
                assert_eq!(
                    leaf.get_one::<String>("context").map(String::as_str),
                    context
                );
            }
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "recovery hint does not parse")]
    fn context_config_recovery_asserts_dispatch_accepts_the_context() {
        let matches = command()
            .try_get_matches_from(["ployz", "server", "ls"])
            .unwrap();
        recovery_command(leaf_matches(&matches), "b", &["org", "ls"]);
    }

    #[test]
    fn direct_recovery_uses_failed_identity_and_actual_scope_before_committed_json() {
        use crate::ui::progress::{Diagnostic, LogTail};
        use ployz_core::{MachineId, QualifiedService};
        let failed = MachineId::random();
        let original = MachineId::random();
        let matches = command()
            .try_get_matches_from([
                "ployz",
                "--ployz-config",
                "/tmp/a config.yaml",
                "--connect",
                "ssh://root@entry",
                "server",
                "set",
                original.as_str(),
                "--accepts-ingress=true",
                "--context",
                "staging",
            ])
            .unwrap();
        let error = Error::unavailable("failed to start")
            .with_diagnostic(Diagnostic {
                failed_machine: Some(failed),
                logs: vec![LogTail {
                    service: QualifiedService::system_ingress(),
                    name: QualifiedService::system_ingress().name,
                    machine: failed,
                    server: "same-name".into(),
                    log: None,
                }],
                ..Default::default()
            })
            .interrupted();
        let error = ingress_hints(error, |args| rerun(leaf_matches(&matches), args))
            .context("Server updated; the Ingress Proxy did not follow.");
        assert!(error.is_interrupted());
        let mode = crate::ui::test_mode(true);
        let (result, captured) = crate::ui::captured(|| {
            crate::ui::emit_committed(
                serde_json::json!({"server": {"machine": {"id": original}}}),
                Err(error),
            )
        });
        drop(mode);
        assert!(result.unwrap_err().is_interrupted());
        let captured = captured.unwrap();
        assert_eq!(
            captured.pointer("/server/machine/id").unwrap(),
            original.as_str()
        );
        let json = captured.get("follow_up_error").unwrap();
        let retry = json.pointer("/details/retry").unwrap().as_str().unwrap();
        let inspect = json
            .pointer("/details/inspect/0")
            .unwrap()
            .as_str()
            .unwrap();
        for hint in [retry, inspect] {
            let parsed = command()
                .try_get_matches_from(shell_words::split(hint).unwrap())
                .unwrap();
            let leaf = leaf_matches(&parsed);
            assert_eq!(
                leaf.get_one::<String>("ployz-config").map(String::as_str),
                Some("/tmp/a config.yaml")
            );
            assert_eq!(
                leaf.get_one::<String>("connect").map(String::as_str),
                Some("ssh://root@entry")
            );
            assert_eq!(
                leaf.get_one::<String>("context").map(String::as_str),
                Some("staging")
            );
            assert!(hint.contains(failed.as_str()));
            assert!(!hint.contains(original.as_str()));
        }
        assert!(inspect.contains("ployz-system/ingress"));
        assert!(
            json.get("message")
                .unwrap()
                .as_str()
                .unwrap()
                .starts_with("Server updated")
        );
    }

    #[test]
    fn logs_since_rejects_garbage_before_connecting() {
        let mut command = command();
        let cases = [
            (
                "since",
                "notatime",
                r#"invalid time "notatime" for --since: expected a relative duration, RFC 3339 date, or Unix timestamp"#,
            ),
            (
                "until",
                "notatime",
                r#"invalid time "notatime" for --until: expected a relative duration, RFC 3339 date, or Unix timestamp"#,
            ),
            (
                "tail",
                "abc",
                r#"invalid log tail "abc": expected a non-negative integer or all"#,
            ),
        ];
        for (flag, value, expected) in cases {
            let matches = command
                .clone()
                .try_get_matches_from([
                    "ployz",
                    "--connect",
                    "tcp://127.0.0.1:1",
                    "logs",
                    "api",
                    &format!("--{flag}"),
                    value,
                ])
                .unwrap();
            assert_eq!(
                dispatch(&matches, &mut command).unwrap_err().to_string(),
                expected,
                "{flag}",
            );
        }
    }

    #[test]
    fn server_set_rejects_an_invalid_server_name_before_connecting() {
        let mut command = command();
        let matches = command
            .clone()
            .try_get_matches_from(["ployz", "server", "set", "vultr1", "--name", "BAD NAME"])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command).unwrap_err().to_string(),
            ployz_core::MachineName::parse("BAD NAME")
                .unwrap_err()
                .to_string(),
        );
    }

    #[test]
    fn local_machine_init_requires_root_to_install() {
        if crate::provisioning::process_is_root() {
            return;
        }
        let mut command = command();
        let matches = command
            .clone()
            .try_get_matches_from([
                "ployz",
                "server",
                "add",
                "--standalone",
                "--yes",
                "--storage",
                "none",
                "--accepts-ingress=false",
                "--context",
                "local-init",
            ])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command).unwrap_err().to_string(),
            "run this command with sudo",
        );
    }

    #[test]
    fn local_machine_init_without_install_dials_the_unix_socket() {
        if std::path::Path::new(crate::connect::DEFAULT_LOCAL_SOCKET).exists() {
            return;
        }
        let mut command = command();
        let matches = command
            .clone()
            .try_get_matches_from([
                "ployz",
                "server",
                "add",
                "--standalone",
                "--no-install",
                "--yes",
                "--storage",
                "none",
                "--accepts-ingress=false",
                "--context",
                "local-init-no-install",
            ])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command)
                .unwrap_err()
                .causes()
                .get(..2),
            Some(
                &[
                    "Couldn't open a connection to the Server.",
                    "transport error"
                ]
                .map(String::from)[..]
            ),
        );
    }

    #[test]
    fn server_add_accepts_only_supported_storage_choices() {
        for storage in ["none", "zfs", "zfs:/dev/vdb"] {
            let parsed = command()
                .try_get_matches_from([
                    "ployz",
                    "server",
                    "add",
                    "root@example.test",
                    "--storage",
                    storage,
                ])
                .unwrap();
            assert_eq!(
                leaf_matches(&parsed)
                    .get_one::<ployz_core::StoragePreparation>("storage")
                    .map(ToString::to_string),
                Some(storage.to_owned()),
            );
        }
        assert!(
            command()
                .try_get_matches_from([
                    "ployz",
                    "server",
                    "add",
                    "root@example.test",
                    "--storage",
                    "other",
                ])
                .is_err()
        );
    }

    #[test]
    fn server_add_takes_a_token_or_prints_a_command() {
        assert!(command().try_get_matches_from(["ployz", "cloud"]).is_ok());
        let parsed = command()
            .try_get_matches_from(["ployz", "server", "add", "--token-stdin"])
            .unwrap();
        let add = leaf_matches(&parsed);
        assert!(add.get_flag("token-stdin"));
        assert_eq!(
            add.get_one::<ipnet::Ipv4Net>("network").copied(),
            Some("10.210.0.0/16".parse().unwrap())
        );
        for conflicting in [
            ["ployz", "server", "add", "--token", "pmet_x"].as_slice(),
            ["ployz", "server", "add", "--token-stdin", "--command"].as_slice(),
            [
                "ployz",
                "server",
                "add",
                "--token-file",
                "t",
                "--wait",
                "id",
            ]
            .as_slice(),
            [
                "ployz",
                "server",
                "add",
                "--token-stdin",
                "--token-file",
                "t",
            ]
            .as_slice(),
            ["ployz", "server", "add", "--command", "root@host"].as_slice(),
            ["ployz", "server", "add", "--wait", "id", "root@host"].as_slice(),
            ["ployz", "server", "add", "--standalone", "--token-stdin"].as_slice(),
            ["ployz", "server", "add", "--network", "not-a-cidr"].as_slice(),
        ] {
            assert!(
                command().try_get_matches_from(conflicting).is_err(),
                "{conflicting:?}"
            );
        }
        let flags = command()
            .try_get_matches_from([
                "ployz",
                "server",
                "add",
                "root@host",
                "--token-file",
                "token",
                "--name",
                "edge",
                "--network",
                "10.220.0.0/16",
                "--storage",
                "zfs",
                "--accepts-ingress=false",
                "--reset",
                "--yes",
                "--wg-mtu",
                "1400",
                "--cloud-url",
                "example.test",
            ])
            .unwrap();
        let add = leaf_matches(&flags);
        assert_eq!(add.get_one::<String>("destination").unwrap(), "root@host");
        assert_eq!(add.get_one::<String>("name").unwrap(), "edge");
        assert_eq!(
            add.get_one::<ployz_core::StoragePreparation>("storage"),
            Some(&ployz_core::StoragePreparation::Zfs { device: None })
        );
        assert_eq!(add.get_one::<bool>("accepts-ingress"), Some(&false));
        assert!(add.get_flag("reset"));
        assert_eq!(add.get_one::<u32>("wg-mtu").copied(), Some(1400));
        assert_eq!(
            add.get_one::<String>("cloud-url").map(String::as_str),
            Some("example.test")
        );
    }

    #[test]
    fn server_add_with_a_token_and_no_daemon_requires_sudo() {
        if crate::provisioning::process_is_root() {
            return;
        }
        let token = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(token.path(), "pmet_test\n").unwrap();
        let mut command = command();
        let matches = command
            .clone()
            .try_get_matches_from([
                "ployz",
                "server",
                "add",
                "--token-file",
                token.path().to_str().unwrap(),
                "--cloud-url",
                "http://127.0.0.1:9",
            ])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command).unwrap_err().to_string(),
            "run this command with sudo",
        );
    }

    #[test]
    fn cloud_reset_fails_closed_with_the_command_to_confirm() {
        let mut command = command();
        let matches = command
            .clone()
            .try_get_matches_from(["ployz", "cloud", "reset"])
            .unwrap();
        let error = dispatch(&matches, &mut command).unwrap_err().report();
        assert_eq!(
            error
                .details
                .get("retry")
                .and_then(serde_json::Value::as_str),
            Some("ployz cloud reset --yes")
        );
    }

    #[test]
    fn founding_commands_reject_the_removed_ingress_backend_option() {
        for arguments in [
            [
                "ployz",
                "server",
                "add",
                "--standalone",
                "--ingress-backend",
                "caddy",
            ]
            .as_slice(),
            [
                "ployz",
                "server",
                "add",
                "--token-stdin",
                "--ingress-backend",
                "caddy",
            ]
            .as_slice(),
        ] {
            assert!(command().try_get_matches_from(arguments).is_err());
        }
    }

    #[test]
    fn retired_daemon_channels_are_rejected() {
        for channel in ["latest", "nightly"] {
            let error = command()
                .try_get_matches_from([
                    "ployz",
                    "server",
                    "add",
                    "root@example.com",
                    "--version",
                    channel,
                ])
                .unwrap_err();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{channel}"
            );
        }
    }
}
