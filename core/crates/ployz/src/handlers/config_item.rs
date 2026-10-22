//! Configs in the Config Store: named folders of small text files that Services
//! mount read-only. Create one, put and remove its files, mount it into Services,
//! rename and delete it. Every change is staged in Working State until a Deploy.
//! A file reads Service variables as `${{ SERVICE.KEY }}`.

use clap::{ArgAction, ArgMatches, Command};
use ployz_core::config::{FileMode, ReviewLifecycleKind};
use ployz_core::{ConfigFileName, ConfigName, ServiceName};
use ployz_store::{
    AttachConfig, ConfigId, ConfigItemQuery, ConfigListing, ConfigMountAt, ConfigStaged,
    ConfigSummary, ConfigsQuery, CreateConfig, DeleteConfig, DetachConfig, PutConfigFile,
    RemoveConfigFile, RenameConfig,
};

use super::store::{self, Next};
use super::{Error, MAX_CLI_INPUT_BYTES, input_too_long, leaf_matches, read_bounded, required};
use crate::cli::{base, positional, switch, value};
use crate::failure::io_code;
use crate::ui::{Cell, Fields, Table, Tone};

pub(super) fn group(command: Command) -> Command {
    base(
        command,
        "Manage Configs: folders of text files Services mount",
    )
    .arg_required_else_help(true)
}

pub(super) fn create_command(command: Command) -> Command {
    store::scoped(command.about("Add a Config; staged until you deploy"))
        .arg(positional("name", true).help("Config name, unique in the Environment"))
        .arg(
            value("mount", None)
                .action(ArgAction::Append)
                .value_name("SERVICE:/DIR")
                .help("Mount it into a Service at an absolute directory; repeatable"),
        )
}

pub(super) fn put_command(command: Command) -> Command {
    store::scoped(command.about("Write one file into a Config, from --from or stdin; staged"))
        .arg(positional("config", true).help("Config name"))
        .arg(
            positional("file", true)
                .help("Path inside the Config, like nginx.conf or conf.d/site.conf"),
        )
        .arg(
            value("from", None)
                .value_name("PATH")
                .help("Read the file from PATH instead of stdin; - is stdin"),
        )
        .arg(
            value("mode", None)
                .value_name("OCTAL")
                .value_parser(|mode: &str| {
                    FileMode::parse(mode)
                        .map_err(|_| "Use an octal mode from 0000 to 0777, like 0644")
                })
                .help("Permission bits [default: 0444, or the file's current mode]"),
        )
        .arg(
            switch("executable", None)
                .conflicts_with("mode")
                .help("Shorthand for --mode 0555"),
        )
        .arg(id_flag(
            "uid",
            "Owning user ID [default: 0, or the file's current owner]",
        ))
        .arg(id_flag(
            "gid",
            "Owning group ID [default: 0, or the file's current group]",
        ))
}

pub(super) fn remove_file_command(command: Command) -> Command {
    store::scoped(command.about("Remove one file from a Config; staged"))
        .arg(positional("config", true).help("Config name"))
        .arg(positional("file", true).help("Path of the file inside the Config"))
}

pub(super) fn inspect_command(command: Command) -> Command {
    store::scoped(command.about("Show a Config, its files' text and its mounts"))
        .arg(positional("config", true).help("Config name"))
}

pub(super) fn list_command(command: Command) -> Command {
    store::scoped(command.about("List Configs and what the next Deploy does to them"))
}

pub(super) fn rename_command(command: Command) -> Command {
    store::scoped(command.about("Rename a Config; its files and mounts stay"))
        .arg(positional("config", true).help("Config name"))
        .arg(positional("name", true).help("Its new name, unique in the Environment"))
}

pub(super) fn delete_command(command: Command) -> Command {
    store::scoped(command.about("Remove a Config and unmount it everywhere"))
        .arg(positional("config", true).help("Config name"))
}

pub(super) fn mount_command(command: Command) -> Command {
    store::scoped(command.about("Mount a Config into a Service at a directory"))
        .arg(positional("service", true).help("Service name"))
        .arg(positional("config", true).help("Config name"))
        .arg(positional("dir", false).help("Absolute directory, like /etc/nginx/conf.d"))
        .override_usage("ployz config mount [OPTIONS] <SERVICE> <CONFIG> <DIR>")
}

pub(super) fn unmount_command(command: Command) -> Command {
    store::scoped(command.about("Unmount a Config from a Service"))
        .arg(positional("service", true).help("Service name"))
        .arg(positional("config", true).help("Config name"))
}

fn id_flag(name: &'static str, help: &'static str) -> clap::Arg {
    value(name, None)
        .value_name("ID")
        .value_parser(clap::value_parser!(u32))
        .help(help)
}

pub(super) fn create(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let mounts = matches
        .get_many::<String>("mount")
        .into_iter()
        .flatten()
        .map(|mount| {
            let (service, dir) = mount
                .split_once(':')
                .and_then(|(service, dir)| Some((ServiceName::parse(service).ok()?, dir)))
                .ok_or_else(|| {
                    Error::usage("Expected --mount SERVICE:/DIR, like web:/etc/sentry")
                })?;
            Ok(ConfigMountAt {
                service,
                dir: dir.to_owned(),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let created = store::store(root)?.write(&CreateConfig {
        id: ConfigId::parse(store::mint())?,
        environment: store::environment(matches)?,
        name: config_name(matches, "name")?,
        mounts,
    })?;
    staged(matches, &created, "Staged new Config", "")
}

pub(super) fn put(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let (bytes, from) = match matches
        .get_one::<String>("from")
        .filter(|path| *path != "-")
    {
        Some(path) => (
            std::fs::File::open(path)
                .and_then(|from| read_bounded(from, MAX_CLI_INPUT_BYTES))
                .map_err(|error| {
                    Error::caused(io_code(&error), format!("Can't read --from {path}"), error)
                })?,
            path.as_str(),
        ),
        None => (
            read_bounded(std::io::stdin(), MAX_CLI_INPUT_BYTES)?,
            "stdin",
        ),
    };
    let bytes = bytes.ok_or_else(|| input_too_long(from, "a Config file"))?;
    let file = file_name(matches)?;
    let content = String::from_utf8(bytes).map_err(|_| {
        Error::usage(format!(
            "{file}: a Config file is UTF-8 text; this one is binary"
        ))
    })?;
    let mode = if matches.get_flag("executable") {
        Some(FileMode::EXECUTABLE)
    } else {
        matches.get_one::<FileMode>("mode").copied()
    };
    let config = config_name(matches, "config")?;
    let unchanged = format!("Config {config} already holds {file} as given");
    let put = store::store(root)?.write(&PutConfigFile {
        environment: store::environment(matches)?,
        config,
        file,
        content,
        mode,
        uid: matches.get_one("uid").copied(),
        gid: matches.get_one("gid").copied(),
    })?;
    staged(matches, &put, "Staged Config", &unchanged)
}

pub(super) fn remove_file(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let removed = store::store(root)?.write(&RemoveConfigFile {
        environment: store::environment(matches)?,
        config: config_name(matches, "config")?,
        file: file_name(matches)?,
    })?;
    staged(matches, &removed, "Staged Config", "")
}

pub(super) fn list(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let view = store::store(root)?.read(&ConfigsQuery {
        environment: store::environment(matches)?,
    })?;
    let mut table = Table::new(
        ["CONFIG", "FILES", "MOUNTS", "NEXT DEPLOY"],
        format!("No Configs in {} yet.", view.environment.name),
    );
    for listing in &view.configs {
        table.row([
            Cell::from(listing.config.name.to_string()),
            Cell::from(files_word(&listing.config)),
            Cell::from(mounts_word(&listing.mounts)),
            next_deploy(listing)
                .map_or_else(|| Cell::from(""), |word| Cell::status(word, Tone::Change)),
        ]);
    }
    crate::ui::list(&view, &table)
}

fn next_deploy(listing: &ConfigListing) -> Option<String> {
    if !listing.deployed && listing.change != Some(ReviewLifecycleKind::Delete) {
        return Some("new".to_owned());
    }
    if let Some(change) = &listing.change {
        return Some(super::store::word(change));
    }
    let mounts: Vec<String> = listing
        .mount_changes
        .iter()
        .map(|change| match (&change.before, &change.after) {
            (None, _) => format!("mount on {}", change.service),
            (_, None) => format!("unmount from {}", change.service),
            (Some(_), Some(_)) => format!("move on {}", change.service),
        })
        .collect();
    (!mounts.is_empty()).then(|| mounts.join(", "))
}

pub(super) fn inspect(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let view = store::store(root)?.read(&ConfigItemQuery {
        environment: store::environment(matches)?,
        config: config_name(matches, "config")?.into(),
    })?;
    let listing = &view.config;
    let mut record = Fields::new()
        .field("config", &listing.config.name)
        .field("id", &listing.config.id);
    if let Some(word) = next_deploy(listing) {
        record.push("next deploy", word);
    }
    for mount in &listing.mounts {
        record.push(
            "mounted by",
            format_args!("{} at {}", mount.service, mount.dir),
        );
    }
    crate::ui::finish(&view, || {
        let _ = record.write(&mut crate::ui::stdout(), crate::ui::stdout_is_terminal());
        for file in &listing.config.files {
            crate::ui::stream(format_args!(
                "\n{} ({} bytes, mode {}, uid {}, gid {})",
                file.name, file.bytes, file.mode, file.uid, file.gid
            ));
            if let Some(text) = view.contents.get(&file.name) {
                crate::ui::stream(text);
            }
        }
    })
}

pub(super) fn rename(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let renamed = store::store(root)?.write(&RenameConfig {
        environment: store::environment(matches)?,
        config: config_name(matches, "config")?,
        name: config_name(matches, "name")?,
    })?;
    let unchanged = format!("Config {} already has that name", renamed.config.name);
    staged(matches, &renamed, "Staged rename of Config", &unchanged)
}

pub(super) fn delete(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let deleted = store::store(root)?.write(&DeleteConfig {
        environment: store::environment(matches)?,
        config: config_name(matches, "config")?,
    })?;
    staged(matches, &deleted, "Staged delete of Config", "")
}

pub(super) fn mount(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let dir = mount_dir(matches)?;
    let service = service_name(matches)?;
    let mounted = store::store(root)?.write(&AttachConfig {
        environment: store::environment(matches)?,
        service: service.clone(),
        config: config_name(matches, "config")?,
        dir,
    })?;
    let unchanged = format!(
        "{service} already mounts Config {} there",
        mounted.config.name
    );
    staged(matches, &mounted, "Staged mount of Config", &unchanged)
}

/// `DIR`, or for `config add`'s `SERVICE:/DIR` typed here, the order this takes.
fn mount_dir(matches: &ArgMatches) -> Result<String, Error> {
    if let Some(dir) = matches.get_one::<String>("dir") {
        return Ok(dir.clone());
    }
    let words =
        ["service", "config"].map(|id| matches.get_one::<String>(id).map_or("", String::as_str));
    let split = |word: &str| {
        word.split_once(":/")
            .map(|(service, dir)| (service.to_owned(), format!("/{dir}")))
    };
    let (service, config, dir) = match (split(words[0]), split(words[1])) {
        (_, Some((service, dir))) => (service, words[0], dir),
        (Some((service, dir)), None) => (service, words[1], dir),
        (None, None) => return Err(Error::usage("dir is required")),
    };
    Err(Error::usage(format!(
        "config mount takes SERVICE CONFIG DIR: ployz config mount {service} {config} {dir}"
    )))
}

pub(super) fn unmount(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let service = service_name(matches)?;
    let unmounted = store::store(root)?.write(&DetachConfig {
        environment: store::environment(matches)?,
        service: service.clone(),
        config: config_name(matches, "config")?,
    })?;
    let unchanged = format!("{service} does not mount Config {}", unmounted.config.name);
    staged(matches, &unmounted, "Staged unmount of Config", &unchanged)
}

fn files_word(config: &ConfigSummary) -> String {
    config
        .files
        .iter()
        .map(|file| file.name.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn mounts_word(mounts: &[ConfigMountAt]) -> String {
    mounts
        .iter()
        .map(|mount| format!("{}:{}", mount.service, mount.dir))
        .collect::<Vec<_>>()
        .join(",")
}

/// A staged Config change and `ployz diff` to review it. A change that was already
/// so says nothing changed and why: `unchanged`.
fn staged(
    matches: &ArgMatches,
    result: &ConfigStaged,
    what: &str,
    unchanged: &str,
) -> Result<(), Error> {
    if result.staged.is_empty() {
        return crate::ui::done(
            &Next::new(result, None),
            format_args!(
                "Nothing changed in {}/{}; {unchanged}.",
                result.environment.project, result.environment.name
            ),
        );
    }
    let hint = store::next(matches, &["diff"]);
    crate::ui::finish(&Next::new(result, Some(hint)), || {
        crate::ui::stream(format_args!(
            "{what} {} in {}/{}.",
            result.config.name, result.environment.project, result.environment.name
        ));
        for file in &result.config.files {
            crate::ui::stream(format_args!(
                "  {} ({} bytes, mode {})",
                file.name, file.bytes, file.mode
            ));
        }
    })
}

fn config_name(matches: &ArgMatches, arg: &str) -> Result<ConfigName, Error> {
    ConfigName::parse(required(matches, arg)?).map_err(|_| {
        Error::usage(
            "Expected a Config name: up to 63 lowercase letters, digits and -, starting and ending with a letter or digit, like sentry",
        )
    })
}

fn file_name(matches: &ArgMatches) -> Result<ConfigFileName, Error> {
    ConfigFileName::parse(required(matches, "file")?).map_err(|_| {
        Error::usage(
            "Expected a file path inside the Config: up to 4 segments, none empty, . or .., like conf.d/site.conf",
        )
    })
}

fn service_name(matches: &ArgMatches) -> Result<ServiceName, Error> {
    ServiceName::parse(required(matches, "service")?)
        .map_err(|_| Error::usage("Expected a Service name, like web"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mount_written_as_service_colon_dir_names_the_order_mount_takes() {
        let refused = |args: &[&str]| {
            let matches = mount_command(Command::new("mount"))
                .try_get_matches_from(std::iter::once("mount").chain(args.iter().copied()))
                .unwrap();
            mount_dir(&matches).unwrap_err().to_string()
        };
        let hint = "config mount takes SERVICE CONFIG DIR: ployz config mount web app /etc/app";
        assert_eq!(refused(&["app", "web:/etc/app"]), hint);
        assert_eq!(refused(&["web:/etc/app", "app"]), hint);
        assert_eq!(refused(&["web", "app"]), "dir is required");
    }
}
