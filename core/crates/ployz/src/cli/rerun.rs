//! The commands a hint tells the reader to run, rebuilt from the command line clap parsed.
//! A rerun keeps every argument the command line gave, in the order given, spelled as the
//! command tree spells it; a hint adds, replaces or drops arguments by their id, never by
//! text. Each value shows as a [`Shown`]: whole when its argument's kind is never a secret
//! (a choice, a number, a path, or one of the [`NAMED`] arguments), as its [`Withheld`]
//! view, or else as the placeholder its argument declares, so no hint repeats a secret.

use std::any::TypeId;
use std::collections::BTreeSet;
use std::path::PathBuf;

use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Command};
use ployz_core::{Hint, RetryCommand};

use super::Shown;

/// A command line to run: this one again, or another one that keeps this one's connection.
#[derive(Clone, Debug)]
pub(crate) struct Rerun {
    tree: Command,
    /// The subcommand words, or every word when they didn't parse.
    path: Vec<String>,
    given: Vec<Given>,
}

#[derive(Clone, Debug)]
struct Given {
    id: String,
    values: Values,
}

#[derive(Clone, Debug)]
enum Values {
    Switch,
    Text(Vec<Shown>),
}

impl Rerun {
    /// This command line again, as the compiled and published command tree parsed it.
    pub(crate) fn again(root: &ArgMatches) -> Self {
        Self::again_in(super::command(), root)
    }

    /// This command line again, as `tree` parsed it into `root`.
    pub(crate) fn again_in(tree: Command, root: &ArgMatches) -> Self {
        debug_assert!(
            root.subcommand().is_some(),
            "a rerun starts from the root matches, which name the command"
        );
        let (path, given) = given(&tree, root);
        Self { tree, path, given }
    }

    /// `words`, a command line without `ployz`, run with this command line's Ployz config,
    /// connection and context, each one the command at `words` takes.
    pub(crate) fn then(matches: &ArgMatches, words: &[&str]) -> Self {
        let tree = super::command();
        let leaf = super::leaf_matches(matches);
        let carried: Vec<Given> = tree
            .get_arguments()
            .filter(|arg| CARRIED.contains(&arg.get_id().as_str()))
            .filter_map(|arg| Given::read(arg, leaf))
            .collect();
        let mut rerun = Self::parsed(tree, words);
        for carried in carried {
            let taken = matches!(carried.values, Values::Switch)
                || super::takes_value(rerun.path.iter().map(String::as_str), &carried.id);
            if taken && !rerun.given.iter().any(|given| given.id == carried.id) {
                rerun.given.push(carried);
            }
        }
        rerun
    }

    /// `words` with `config`, on this host's daemon at `connect`, as the check-in loop
    /// names a command: it has no command line of its own to carry.
    pub(crate) fn for_this_host(config: &std::path::Path, connect: &str, words: &[&str]) -> Self {
        let config = config.to_string_lossy();
        let carried = ["--ployz-config", config.as_ref(), "--connect", connect];
        Self::parsed(super::command(), &[&carried[..], words].concat())
    }

    /// `words` alone, as a command line with nothing of its own to carry.
    #[cfg(test)]
    pub(crate) fn plain(words: &[&str]) -> Self {
        Self::parsed(super::command(), words)
    }

    fn parsed(tree: Command, words: &[&str]) -> Self {
        let argv = std::iter::once("ployz").chain(words.iter().copied());
        match tree.clone().try_get_matches_from(argv) {
            Ok(parsed) => {
                let (path, given) = given(&tree, &parsed);
                Self { tree, path, given }
            }
            Err(error) => {
                debug_assert!(false, "a rerun's words don't parse: {words:?}: {error}");
                Self {
                    tree,
                    path: words.iter().map(|word| (*word).to_owned()).collect(),
                    given: Vec::new(),
                }
            }
        }
    }

    /// With `value` as the one value of `id`, in place of any it was given, shown as the
    /// command line would show it.
    #[must_use]
    pub(crate) fn set(self, id: &str, value: &str) -> Self {
        self.set_all(id, [value])
    }

    /// With `values` as the values of `id`, in place of any it was given; none drops it.
    #[must_use]
    pub(crate) fn set_all<'a>(self, id: &str, values: impl IntoIterator<Item = &'a str>) -> Self {
        let values: Vec<Shown> = values
            .into_iter()
            .map(|value| self.shown(id, value))
            .collect();
        self.replaced(id, values)
    }

    /// With `placeholder` as the one value of `id`: what the reader types in its place.
    #[must_use]
    pub(crate) fn placeholder(self, id: &str, placeholder: &'static str) -> Self {
        self.replaced(id, vec![Shown::placeholder(placeholder)])
    }

    fn replaced(mut self, id: &str, values: Vec<Shown>) -> Self {
        if values.is_empty() {
            return self.without(id);
        }
        match self.given.iter_mut().find(|given| given.id == id) {
            Some(given) => given.values = Values::Text(values),
            None => self.given.push(Given {
                id: id.to_owned(),
                values: Values::Text(values),
            }),
        }
        self
    }

    /// With `value` added to the values of `id`, after any it was given.
    #[must_use]
    pub(crate) fn add(mut self, id: &str, value: &str) -> Self {
        let value = self.shown(id, value);
        match self.given.iter_mut().find(|given| given.id == id) {
            Some(Given {
                values: Values::Text(values),
                ..
            }) => values.push(value),
            Some(given) => given.values = Values::Text(vec![value]),
            None => self.given.push(Given {
                id: id.to_owned(),
                values: Values::Text(vec![value]),
            }),
        }
        self
    }

    /// `value` given to `id`, as the command line would show it.
    fn shown(&self, id: &str, value: &str) -> Shown {
        let levels = self.levels();
        match levels
            .iter()
            .rev()
            .find_map(|level| level.get_arguments().find(|arg| arg.get_id() == id))
        {
            Some(arg) => shown(arg, value),
            None => {
                debug_assert!(false, "a rerun names {id:?}, which its command lacks");
                Shown::placeholder("VALUE")
            }
        }
    }

    /// The command and each subcommand of the path, root first.
    fn levels(&self) -> Vec<&Command> {
        let mut levels = vec![&self.tree];
        for word in &self.path {
            let Some(child) = levels.last().and_then(|at| at.find_subcommand(word)) else {
                break;
            };
            levels.push(child);
        }
        levels
    }

    /// With the switch `id` on.
    #[must_use]
    pub(crate) fn switch(mut self, id: &str) -> Self {
        if !self.given.iter().any(|given| given.id == id) {
            self.given.push(Given {
                id: id.to_owned(),
                values: Values::Switch,
            });
        }
        self
    }

    /// Without `id` and anything it was given.
    #[must_use]
    pub(crate) fn without(mut self, id: &str) -> Self {
        self.given.retain(|given| given.id != id);
        self
    }

    /// Whether the command line parses: a change clap's rules refuse, such as a flag that
    /// conflicts with one given, leaves a line no one can run.
    pub(crate) fn parses(&self) -> bool {
        self.tree.clone().try_get_matches_from(self.words()).is_ok()
    }

    /// The command line, quoted to paste into a shell.
    pub(crate) fn line(&self) -> String {
        let words = self.words();
        #[cfg(debug_assertions)]
        if let Err(error) = self.tree.clone().try_get_matches_from(&words) {
            panic!("a rerun doesn't parse: {words:?}: {error}");
        }
        shell_words::join(words)
    }

    fn words(&self) -> Vec<String> {
        let levels = self.levels();
        debug_assert_eq!(levels.len(), self.path.len() + 1, "{:?}", self.path);
        let declared = |depth: usize, id: &str| {
            levels
                .get(depth)
                .and_then(|level| level.get_arguments().find(|arg| arg.get_id() == id))
        };
        // The nearest level that declares `id`, and the level its flag is typed after: the
        // root's flags before the subcommand, as `ployz --help` lists them, and any other
        // global after the whole of it.
        let arg = |id: &str| {
            let (depth, arg) = (0..levels.len())
                .rev()
                .find_map(|depth| declared(depth, id).map(|arg| (depth, arg)))?;
            let at = match arg.is_global_set() {
                true if declared(0, id).is_some_and(clap::Arg::is_global_set) => 0,
                true => levels.len() - 1,
                false => depth,
            };
            Some((at, arg))
        };
        // A positional that reads as a flag, or that takes every word after it, goes
        // behind `--`, so every flag goes before it.
        let tail_from = self.given.iter().position(|given| {
            arg(&given.id).is_some_and(|(_, arg)| {
                arg.is_positional()
                    && (arg.is_trailing_var_arg_set()
                        || arg.is_last_set()
                        || matches!(&given.values, Values::Text(values)
                            if values.iter().any(|value| value.as_str().starts_with('-'))))
            })
        });
        // What each level is given, typed after its subcommand's name.
        let mut after: Vec<Vec<String>> = vec![Vec::new(); levels.len()];
        let leaf = levels.len() - 1;
        // A flag whose value may be left out goes last, so no word after it reads as its value.
        let (mut bare, mut tail) = (Vec::new(), Vec::new());
        for (index, given) in self.given.iter().enumerate() {
            let Some((at, arg)) = arg(&given.id) else {
                debug_assert!(
                    false,
                    "a rerun names {:?}, which its command lacks",
                    given.id
                );
                continue;
            };
            let flag = arg
                .get_long()
                .map(|long| format!("--{long}"))
                .or_else(|| arg.get_short().map(|short| format!("-{short}")));
            let Some(into) = after.get_mut(if flag.is_some() { at } else { leaf }) else {
                continue;
            };
            match (flag, &given.values) {
                (None, Values::Text(values)) if tail_from.is_some_and(|from| index >= from) => {
                    tail.extend(values.iter().map(ToString::to_string));
                }
                (None, Values::Text(values)) => into.extend(values.iter().map(ToString::to_string)),
                (Some(flag), Values::Switch) => into.push(flag),
                (Some(flag), Values::Text(values)) if values.is_empty() => bare.push(flag),
                (Some(flag), Values::Text(values)) => {
                    for value in values {
                        if value.as_str().starts_with('-') || arg.is_require_equals_set() {
                            into.push(format!("{flag}={value}"));
                        } else {
                            into.extend([flag.clone(), value.to_string()]);
                        }
                    }
                }
                (None, Values::Switch) => {
                    debug_assert!(false, "{:?} is a positional, not a switch", given.id);
                }
            }
        }
        let mut words = vec!["ployz".to_owned()];
        for (depth, given) in after.into_iter().enumerate() {
            words.extend(
                depth
                    .checked_sub(1)
                    .and_then(|at| self.path.get(at))
                    .cloned(),
            );
            words.extend(given);
        }
        words.extend(bare);
        if !tail.is_empty() {
            words.push("--".to_owned());
            words.extend(tail);
        }
        words
    }
}

impl From<Rerun> for Hint {
    fn from(rerun: Rerun) -> Self {
        Self::Retry(RetryCommand::from(rerun))
    }
}

impl From<Rerun> for Option<Hint> {
    fn from(rerun: Rerun) -> Self {
        Some(rerun.into())
    }
}

impl From<Rerun> for RetryCommand {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place the CLI renders a retry: from a Rerun"
    )]
    fn from(rerun: Rerun) -> Self {
        Self::rendered(rerun.line())
    }
}

/// The subcommand words of `root` and every argument its command line gave, in the
/// order given. A global argument is read once, from the leaf its value reached.
fn given(tree: &Command, root: &ArgMatches) -> (Vec<String>, Vec<Given>) {
    let leaf = super::leaf_matches(root);
    let names: Vec<&str> =
        std::iter::successors(root.subcommand(), |(_, matches)| matches.subcommand())
            .map(|(name, _)| name)
            .collect();
    let depth_of_leaf = names.len();
    // An argument the leaf declares as its own, though the root shares one by its id.
    let leaf_declares = |id: &str| {
        names
            .iter()
            .try_fold(tree, |command, name| command.find_subcommand(name))
            .is_some_and(|command| {
                command
                    .get_arguments()
                    .any(|arg| arg.get_id() == id && !arg.is_global_set())
            })
    };
    let mut path = Vec::new();
    let mut globals = BTreeSet::new();
    let mut given = Vec::new();
    let mut command = Some(tree);
    let mut matches = root;
    loop {
        for arg in command.into_iter().flat_map(Command::get_arguments) {
            let id = arg.get_id().as_str();
            // A global's value reaches the leaf, which may declare the id again.
            if globals.contains(id) {
                continue;
            }
            // Each level counts its own words, and a global takes the place the leaf
            // counted, so the root's globals keep the order they were declared in.
            let (from, at) = match arg.is_global_set() {
                true if path.is_empty() && !leaf_declares(id) => (leaf, (0, Some(0))),
                true => (leaf, (depth_of_leaf, leaf.index_of(id))),
                false => (matches, (path.len(), matches.index_of(id))),
            };
            if arg.is_global_set() {
                globals.insert(id.to_owned());
            }
            if let Some(read) = Given::read(arg, from) {
                given.push((at, read));
            }
        }
        let Some((name, child)) = matches.subcommand() else {
            break;
        };
        path.push(name.to_owned());
        command = command.and_then(|command| command.find_subcommand(name));
        matches = child;
    }
    given.sort_by_key(|((depth, at), _)| (*depth, at.unwrap_or(usize::MAX)));
    (path, given.into_iter().map(|(_, given)| given).collect())
}

impl Given {
    /// What the command line gave `arg`. A default holds for the next command too, and so
    /// does a variable, except one that says where the command runs: it may have been set
    /// for this one command (`PLOYZ_ENV=staging ployz …`), so a rerun spells it out.
    fn read(arg: &Arg, matches: &ArgMatches) -> Option<Self> {
        let id = arg.get_id().as_str();
        if !matches!(matches.try_contains_id(id), Ok(true)) {
            return None;
        }
        match matches.value_source(id) {
            Some(ValueSource::CommandLine) => {}
            Some(ValueSource::EnvVariable) if WHERE_IT_RUNS.contains(&id) => {}
            Some(_) | None => return None,
        }
        let action = arg.get_action();
        if matches!(
            action,
            ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
        ) {
            return None;
        }
        let values = if arg.is_trailing_var_arg_set() {
            // A command line that runs elsewhere, such as exec's, may hold a secret in any
            // word (`env TOKEN=…`, `-p…`), so a rerun names it and repeats none of it.
            let typed = matches
                .get_raw(id)
                .is_some_and(|mut raw| raw.next().is_some());
            Values::Text(typed.then(|| Shown::declared(arg)).into_iter().collect())
        } else if action.takes_values() {
            Values::Text(match matches.try_get_many::<Withheld>(id) {
                Ok(Some(withheld)) => withheld.map(|value| value.shown.clone()).collect(),
                Ok(None) | Err(_) => matches
                    .get_raw(id)
                    .into_iter()
                    .flatten()
                    .map(|raw| shown(arg, &raw.to_string_lossy()))
                    .collect(),
            })
        } else {
            Values::Switch
        };
        Some(Self {
            id: id.to_owned(),
            values,
        })
    }
}

/// What another command keeps of this command line: where it runs.
const CARRIED: [&str; 3] = ["ployz-config", "connect", "context"];

/// The arguments that choose where a command runs.
const WHERE_IT_RUNS: [&str; 4] = ["context", "connect", "project", "env"];

/// What a rerun shows of `typed`, given to `arg`, which doesn't parse to [`Withheld`]:
/// all of it when the argument's kind is never a secret, else the placeholder it declares.
fn shown(arg: &Arg, typed: &str) -> Shown {
    let parser = arg.get_value_parser().type_id();
    let known = !arg.get_possible_values().is_empty()
        || closed().into_iter().any(|kind| parser == kind)
        || (parser == TypeId::of::<String>() && NAMED.contains(&arg.get_id().as_str()));
    match known {
        #[expect(
            clippy::disallowed_methods,
            reason = "a choice, a number, a path or a name, none of which is a secret"
        )]
        true => Shown::verbatim(typed),
        false => Shown::declared(arg),
    }
}

/// The parsed kinds no value of which is a secret: numbers, switches, paths and addresses.
fn closed() -> [TypeId; 14] {
    [
        TypeId::of::<bool>(),
        TypeId::of::<u16>(),
        TypeId::of::<u32>(),
        TypeId::of::<u64>(),
        TypeId::of::<usize>(),
        TypeId::of::<i64>(),
        TypeId::of::<PathBuf>(),
        TypeId::of::<ipnet::Ipv4Net>(),
        TypeId::of::<ployz_core::config::FileMode>(),
        TypeId::of::<ployz_core::ProvisionedVolumeMaximumBytes>(),
        TypeId::of::<ployz_core::StorageChoice>(),
        TypeId::of::<ployz_core::StoragePreparation>(),
        TypeId::of::<ployz_core::MachineRelease>(),
        TypeId::of::<ployz_core::ReleaseChannel>(),
    ]
}

/// The text arguments whose every value names something Ployz shows anyway, such as a
/// Project, a Service, a Server, a Deployment, a member, a version, a file or a domain,
/// compiled or published by Cloud. Any other text argument, such as a message, a reason,
/// a command, a key or a URL, shows as its placeholder.
const NAMED: [&str; 82] = [
    "accept-volume-loss",
    "account",
    "approval",
    "build-concurrency",
    "builder",
    "builder-sha",
    "cluster-name",
    "code",
    "commit",
    "config",
    "confirm",
    "container",
    "context",
    "context-name",
    "copy",
    "cursor",
    "deployment",
    "dir",
    "domain",
    "email",
    "env",
    "environment",
    "events",
    "expect",
    "expect-version",
    "file",
    "fingerprint",
    "first",
    "fix",
    "from",
    "from-env-file",
    "host",
    "id",
    "image",
    "label-add",
    "label-rm",
    "live",
    "machine",
    "match",
    "member",
    "mirror",
    "mount",
    "name",
    "namespace",
    "node",
    "only",
    "organization",
    "path",
    "pattern",
    "platform",
    "ployz-config",
    "port",
    "prefix",
    "project",
    "public-ip",
    "ref",
    "region",
    "rename",
    "repo",
    "repository",
    "run",
    "server",
    "service",
    "service-or-container",
    "signal",
    "since",
    "skip",
    "slug",
    "source",
    "ssh-key",
    "tail",
    "take",
    "timeout",
    "to",
    "undo",
    "until",
    "upload",
    "value",
    "version",
    "volume",
    "wait",
    "wg-endpoint",
];

/// A value whose kind may hold a secret: what was typed, which only the command reads, and
/// what output shows in its place.
#[derive(Clone)]
pub(crate) struct Withheld {
    typed: String,
    shown: Shown,
}

impl Withheld {
    /// What was typed, for the command to run with; output shows [`Self::shown`].
    pub(crate) fn typed(&self) -> &str {
        &self.typed
    }
}

impl std::fmt::Debug for Withheld {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.shown, formatter)
    }
}

/// The parser of an argument whose values may hold a secret: `shown` gives what output
/// repeats of one.
pub(crate) fn withheld(
    shown: fn(&str) -> Shown,
) -> impl clap::builder::TypedValueParser<Value = Withheld> {
    clap::builder::TypedValueParser::map(clap::builder::StringValueParser::new(), move |typed| {
        Withheld {
            shown: shown(&typed),
            typed,
        }
    })
}

/// The retry hint `line` renders as, to compare a hint against in tests.
#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "a test's expected retry, written out to compare a rendered one against"
)]
pub(crate) fn retry_hint(line: &str) -> Hint {
    Hint::Retry(RetryCommand::rendered(line.to_owned()))
}
