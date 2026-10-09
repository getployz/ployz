//! Configs in the Config Store: named folders of small text files that Services
//! mount read-only. Create one, put and remove its files, mount it into Services,
//! rename and delete it. Every change is staged in Working State until a Deploy.
//! A file reads Service variables as `${{ SERVICE.KEY }}`.

use std::io::{self, IsTerminal};

use clap::{ArgAction, ArgMatches, Command};
use ployz_core::config::{FileMode, ReviewLifecycleKind};
use ployz_core::{ConfigFileName, ConfigName, ServiceName};
use ployz_store::{
    AttachConfig, ConfigId, ConfigItemQuery, ConfigListing, ConfigMountAt, ConfigStaged,
    ConfigSummary, ConfigsQuery, CreateConfig, DeleteConfig, DetachConfig, PutConfigFile,
    RemoveConfigFile, RenameConfig,
};

use super::store::{self, Next};
use super::{Error, leaf_matches, required};
use crate::cli::{base, positional, switch, value};
use crate::ui::{Cell, Fields, Table, Tone};

pub(crate) fn command() -> Command {
    base(
        "config",
        "Manage Configs: folders of text files Services mount",
    )
    .arg_required_else_help(true)
    .subcommand(
        store::scoped(Command::new("add").about("Add a Config; staged until you deploy"))
            .arg(positional("name", true).help("Config name, unique in the Environment"))
            .arg(
                value("mount", None)
                    .action(ArgAction::Append)
                    .value_name("SERVICE:/DIR")
                    .help("Mount it into a Service at an absolute directory; repeatable"),
            ),
    )
    .subcommand(
        store::scoped(
            Command::new("put").about("Write one file into a Config, from --from or stdin; staged"),
        )
        .arg(positional("config", true))
        .arg(
            positional("file", true)
                .help("Path inside the Config, like nginx.conf or conf.d/site.conf"),
        )
        .arg(
            value("from", None)
                .value_name("PATH")
                .help("Read the file from PATH instead of stdin"),
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
        )),
    )
    .subcommand(
        store::scoped(Command::new("rm-file").about("Remove one file from a Config; staged"))
            .arg(positional("config", true))
            .arg(positional("file", true)),
    )
    .subcommand(
        store::scoped(
            Command::new("inspect").about("Show a Config, its files' text and its mounts"),
        )
        .arg(positional("config", true)),
    )
    .subcommand(store::scoped(
        Command::new("ls").about("List Configs and what the next Deploy does to them"),
    ))
    .subcommand(
        store::scoped(Command::new("rename").about("Rename a Config; its files and mounts stay"))
            .arg(positional("config", true))
            .arg(positional("name", true)),
    )
    .subcommand(
        store::scoped(Command::new("rm").about("Remove a Config and unmount it everywhere"))
            .arg(positional("config", true)),
    )
    .subcommand(
        store::scoped(Command::new("mount").about("Mount a Config into a Service at a directory"))
            .arg(positional("service", true))
            .arg(positional("config", true))
            .arg(positional("dir", true).help("Absolute directory, like /etc/nginx/conf.d")),
    )
    .subcommand(
        store::scoped(Command::new("unmount").about("Unmount a Config from a Service"))
            .arg(positional("service", true))
            .arg(positional("config", true)),
    )
}

pub(super) fn handler(path: &str) -> Option<super::Handler> {
    Some(match path {
        "add" => create,
        "put" => put,
        "rm-file" => remove_file,
        "inspect" => inspect,
        "ls" => list,
        "rename" => rename,
        "rm" => delete,
        "mount" => mount,
        "unmount" => unmount,
        _ => return None,
    })
}

fn id_flag(name: &'static str, help: &'static str) -> clap::Arg {
    value(name, None)
        .value_name("ID")
        .value_parser(clap::value_parser!(u32))
        .help(help)
}

fn create(root: &ArgMatches) -> Result<(), Error> {
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
    staged(matches, &created, "Staged new Config")
}

fn put(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let bytes = match matches.get_one::<String>("from") {
        Some(path) => std::fs::read(path)?,
        None => {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut bytes)?;
            bytes
        }
    };
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
    let put = store::store(root)?.write(&PutConfigFile {
        environment: store::environment(matches)?,
        config: config_name(matches, "config")?,
        file,
        content,
        mode,
        uid: matches.get_one("uid").copied(),
        gid: matches.get_one("gid").copied(),
    })?;
    staged(matches, &put, "Staged Config")
}

fn remove_file(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let removed = store::store(root)?.write(&RemoveConfigFile {
        environment: store::environment(matches)?,
        config: config_name(matches, "config")?,
        file: file_name(matches)?,
    })?;
    staged(matches, &removed, "Staged Config")
}

fn list(root: &ArgMatches) -> Result<(), Error> {
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

/// What the next Deploy does to a Config: `new` until a Deploy applies it, unless it deletes it.
fn next_deploy(listing: &ConfigListing) -> Option<String> {
    if listing.deployed || listing.change == Some(ReviewLifecycleKind::Delete) {
        return listing.change.as_ref().map(super::store::word);
    }
    Some("new".to_owned())
}

fn inspect(root: &ArgMatches) -> Result<(), Error> {
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
        let _ = record.write(&mut anstream::stdout(), io::stdout().is_terminal());
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

fn rename(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let renamed = store::store(root)?.write(&RenameConfig {
        environment: store::environment(matches)?,
        config: config_name(matches, "config")?,
        name: config_name(matches, "name")?,
    })?;
    staged(matches, &renamed, "Staged rename of Config")
}

fn delete(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let deleted = store::store(root)?.write(&DeleteConfig {
        environment: store::environment(matches)?,
        config: config_name(matches, "config")?,
    })?;
    staged(matches, &deleted, "Staged delete of Config")
}

fn mount(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let mounted = store::store(root)?.write(&AttachConfig {
        environment: store::environment(matches)?,
        service: service_name(matches)?,
        config: config_name(matches, "config")?,
        dir: required(matches, "dir")?,
    })?;
    staged(matches, &mounted, "Staged mount of Config")
}

fn unmount(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let unmounted = store::store(root)?.write(&DetachConfig {
        environment: store::environment(matches)?,
        service: service_name(matches)?,
        config: config_name(matches, "config")?,
    })?;
    staged(matches, &unmounted, "Staged unmount of Config")
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

/// A staged Config change, its files, and `ployz diff` to review it.
fn staged(matches: &ArgMatches, result: &ConfigStaged, what: &str) -> Result<(), Error> {
    let hint = store::next(matches, &["diff"]);
    crate::ui::finish(&Next::new(result, Some(hint)), || {
        crate::ui::stream(format_args!(
            "{what} {} in {}/{} (revision {}).",
            result.config.name,
            result.environment.project,
            result.environment.name,
            result.environment.revision
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
            "Expected a Config name: up to 63 lowercase letters, digits and -, like sentry",
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
