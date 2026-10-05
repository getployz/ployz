//! A Config's lifecycle in Working State: create it, put and remove its files,
//! mount it into Services, rename it and delete it, each staged until a Deploy. A
//! Config is a named folder of small text files that Services mount read-only; its
//! files may reference Service variables by Service name, never bare, because a
//! Config belongs to no Service.

pub(crate) mod query;

use std::collections::BTreeMap;

use ployz_core::config::{
    ConfigAttachment, FileMode, SavedConfigFile, SavedConfigIntent, SavedEnvironmentIntent,
    SavedServiceIntent,
    ValuePart, ValuePartOwner, parse_variable_template, render_variable_parts,
};
use ployz_core::{ConfigFileName, ConfigName, ContainerPath, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::Actor;
use crate::error;
use crate::id::ConfigId;
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary};
use crate::settings::{SettingPath, Target};
use crate::storage::Tx;

/// The most text one Config file holds.
pub const MAX_FILE_BYTES: usize = 256 * 1024;

/// Create a Config, optionally mounted into Services.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CreateConfig {
    /// The new Config's ID, also its lineage.
    pub id: ConfigId,
    /// The Environment to create it in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its name, unique among the Environment's Configs.
    pub name: ConfigName,
    /// Where Services mount it.
    #[serde(default)]
    pub mounts: Vec<ConfigMount>,
}

/// One Service mounting a Config at a directory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ConfigMount {
    /// The Service, by name.
    pub service: ServiceName,
    /// The absolute directory in its containers where the Config's files appear.
    pub dir: String,
}

/// Put one file into a Config, replacing a file of that name. Its text may read
/// Service variables as `${{ SERVICE.KEY }}`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PutConfigFile {
    /// The Environment it is in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// The Config, by name.
    pub config: ConfigName,
    /// The file's path inside the Config.
    pub file: ConfigFileName,
    /// Its text, at most 256 KB.
    pub content: String,
    /// Its permission bits; `0444` unless given.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub mode: Option<FileMode>,
    /// The user ID that owns it; `0` unless given.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub uid: Option<u32>,
    /// The group ID that owns it; `0` unless given.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub gid: Option<u32>,
}

/// Remove one file from a Config.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RemoveConfigFile {
    /// The Environment it is in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// The Config, by name.
    pub config: ConfigName,
    /// The file's path inside the Config.
    pub file: ConfigFileName,
}

/// Rename a Config: a staged change. Its files and mounts stay; only the name
/// paths use changes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RenameConfig {
    /// The Environment it is in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its current name.
    pub config: ConfigName,
    /// Its new name, unique among the Environment's Configs.
    pub name: ConfigName,
}

/// Delete a Config from Working State, unmounting it from every Service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeleteConfig {
    /// The Environment it is in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its name.
    pub config: ConfigName,
}

/// Mount a Config into a Service at a directory, moving it if it was mounted
/// elsewhere.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct AttachConfig {
    /// The Environment they are in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// The Service, by name.
    pub service: ServiceName,
    /// The Config, by name.
    pub config: ConfigName,
    /// The absolute directory in the Service's containers.
    pub dir: String,
}

/// Unmount a Config from a Service; the Config and its files stay.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DetachConfig {
    /// The Environment they are in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// The Service, by name.
    pub service: ServiceName,
    /// The Config, by name.
    pub config: ConfigName,
}

/// A Config changed in Working State, staged until a Deploy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ConfigStaged {
    /// The Config.
    pub config: ConfigSummary,
    /// The Environment, at its revision after the change.
    pub environment: EnvironmentSummary,
    /// What waits for a Deploy: the Config as `configs.NAME`, and each mount it
    /// gained or lost as `SERVICE.configs.NAME`.
    pub staged: Vec<SettingPath>,
}

/// A Config as results name it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ConfigSummary {
    /// Its durable identity.
    pub id: ConfigId,
    /// Its name, which mount paths address it by.
    pub name: ConfigName,
    /// Its files, by path.
    pub files: Vec<ConfigFileSummary>,
}

/// One file of a Config, without its text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ConfigFileSummary {
    /// Its path inside the Config.
    pub name: ConfigFileName,
    /// How many bytes its text is, as written, references unresolved.
    pub bytes: usize,
    /// Its permission bits.
    pub mode: FileMode,
    /// The user ID that owns it.
    pub uid: u32,
    /// The group ID that owns it.
    pub gid: u32,
    /// The Services its text references, by name.
    pub references: Vec<String>,
}

pub(crate) fn create_config(
    tx: &mut dyn Tx,
    who: &Actor,
    create: &CreateConfig,
) -> Result<ConfigStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &create.environment)?;
    taken(&environment, &create.name)?;
    let node = SavedConfigIntent {
        resource_id: create.id.to_string(),
        // A new Config starts its own lineage; Branch copies keep it.
        resource_lineage_id: create.id.to_string(),
        name: create.name.clone(),
        files: BTreeMap::new(),
    };
    environment.working.configs.push(node.clone());
    let mut staged = vec![SettingPath::config(&create.name)];
    for mount in &create.mounts {
        if attach(&mut environment, &mount.service, &create.name, &mount.dir)? {
            staged.push(SettingPath::at(
                &mount.service,
                Target::ConfigMount(create.name.clone()),
            ));
        }
    }
    scope::save_working(tx, &mut environment)?;
    scope::introduce(tx, who, &environment.summary.id, scope::Node::Config(&node))?;
    Ok(ConfigStaged {
        config: summary(&node, &environment.names())?,
        environment: environment.summary,
        staged,
    })
}

pub(crate) fn put_file(
    tx: &mut dyn Tx,
    who: &Actor,
    put: &PutConfigFile,
) -> Result<ConfigStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &put.environment)?;
    let names = environment.names();
    let content = parts(&put.file, &put.content, &names)?;
    let config = environment.config_mut(&put.config)?;
    // A rewrite keeps the mode and owner it doesn't name.
    let old = config.files.get(&put.file);
    let file = SavedConfigFile {
        content,
        mode: put
            .mode
            .or(old.map(|old| old.mode))
            .unwrap_or(FileMode::READ_ONLY),
        uid: put.uid.or(old.map(|old| old.uid)).unwrap_or(0),
        gid: put.gid.or(old.map(|old| old.gid)).unwrap_or(0),
    };
    let changed = config.files.get(&put.file) != Some(&file);
    if changed {
        config.files.insert(put.file.clone(), file);
        let configs = &environment.working.configs;
        for node in &environment.working.services {
            let service =
                ServiceName::parse(node.slug.as_str()).map_err(|_| error::corrupt("Service"))?;
            if let Some(error) = collision(&service, node, configs) {
                return Err(error);
            }
        }
        scope::save_working(tx, &mut environment)?;
    }
    staged(environment, &put.config, changed)
}

pub(crate) fn remove_file(
    tx: &mut dyn Tx,
    who: &Actor,
    remove: &RemoveConfigFile,
) -> Result<ConfigStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &remove.environment)?;
    let config = environment.config_mut(&remove.config)?;
    if config.files.remove(&remove.file).is_none() {
        let files: Vec<String> = config.files.keys().map(ToString::to_string).collect();
        return Err(error::choices(
            format!("Config {} has no file {}", remove.config, remove.file),
            remove.file.as_str(),
            files.iter().map(String::as_str),
        ));
    }
    scope::save_working(tx, &mut environment)?;
    staged(environment, &remove.config, true)
}

pub(crate) fn rename_config(
    tx: &mut dyn Tx,
    who: &Actor,
    rename: &RenameConfig,
) -> Result<ConfigStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &rename.environment)?;
    let changed = rename.name != rename.config;
    if changed {
        taken(&environment, &rename.name)?;
        environment.config_mut(&rename.config)?.name = rename.name.clone();
        scope::save_working(tx, &mut environment)?;
    }
    staged(environment, &rename.name, changed)
}

pub(crate) fn delete_config(
    tx: &mut dyn Tx,
    who: &Actor,
    delete: &DeleteConfig,
) -> Result<ConfigStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &delete.environment)?;
    let names = environment.names();
    let config = summary(environment.config(&delete.config)?, &names)?;
    let mut staged = vec![SettingPath::config(&config.name)];
    for service in &mut environment.working.services {
        let before = service.config_attachments.len();
        service
            .config_attachments
            .retain(|mount| mount.config_resource_id != config.id.as_str());
        if service.config_attachments.len() != before {
            let name =
                ServiceName::parse(service.slug.as_str()).map_err(|_| error::corrupt("Service"))?;
            staged.push(SettingPath::at(
                &name,
                Target::ConfigMount(config.name.clone()),
            ));
        }
    }
    environment
        .working
        .configs
        .retain(|node| node.resource_id != config.id.as_str());
    scope::save_working(tx, &mut environment)?;
    Ok(ConfigStaged {
        config,
        environment: environment.summary,
        staged,
    })
}

pub(crate) fn attach_config(
    tx: &mut dyn Tx,
    who: &Actor,
    mount: &AttachConfig,
) -> Result<ConfigStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &mount.environment)?;
    let changed = attach(&mut environment, &mount.service, &mount.config, &mount.dir)?;
    if changed {
        scope::save_working(tx, &mut environment)?;
    }
    mounted(environment, mount, changed)
}

pub(crate) fn detach_config(
    tx: &mut dyn Tx,
    who: &Actor,
    unmount: &DetachConfig,
) -> Result<ConfigStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &unmount.environment)?;
    let changed = detach(&mut environment, &unmount.service, &unmount.config)?;
    if changed {
        scope::save_working(tx, &mut environment)?;
    }
    let staged = changed
        .then(|| SettingPath::at(&unmount.service, Target::ConfigMount(unmount.config.clone())))
        .into_iter()
        .collect();
    Ok(ConfigStaged {
        config: summary(environment.config(&unmount.config)?, &environment.names())?,
        environment: environment.summary,
        staged,
    })
}

/// What mounting a Config staged: the mount, when it changed.
fn mounted(
    environment: Environment,
    mount: &AttachConfig,
    changed: bool,
) -> Result<ConfigStaged, RpcError> {
    let staged = changed
        .then(|| SettingPath::at(&mount.service, Target::ConfigMount(mount.config.clone())))
        .into_iter()
        .collect();
    Ok(ConfigStaged {
        config: summary(environment.config(&mount.config)?, &environment.names())?,
        environment: environment.summary,
        staged,
    })
}

/// Refuse a second Config named `name`.
fn taken(environment: &Environment, name: &ConfigName) -> Result<(), RpcError> {
    if environment.config(name).is_err() {
        return Ok(());
    }
    Err(error::conflict(
        format!(
            "Environment {} already has a Config named {name}",
            environment.summary.name
        ),
        json!({ "config": name }),
    ))
}

/// What changing Config `name` staged: the Config, when anything changed.
fn staged(
    environment: Environment,
    name: &ConfigName,
    changed: bool,
) -> Result<ConfigStaged, RpcError> {
    Ok(ConfigStaged {
        config: summary(environment.config(name)?, &environment.names())?,
        environment: environment.summary,
        staged: changed
            .then(|| SettingPath::config(name))
            .into_iter()
            .collect(),
    })
}

/// Mount Config `config` into `service` at `dir`, replacing where it was mounted.
/// Returns whether Working State changed.
pub(crate) fn attach(
    environment: &mut Environment,
    service: &ServiceName,
    config: &ConfigName,
    dir: &str,
) -> Result<bool, RpcError> {
    let dir = ContainerPath::parse(dir).map_err(|_| {
        error::invalid(
            format!("{service}.configs.{config}: expected an absolute directory without null characters"),
            json!({ "example": "/etc/app" }),
        )
    })?;
    let id = environment.config(config)?.resource_id.clone();
    let volumes = environment.working.volumes.clone();
    let configs = environment.working.configs.clone();
    let node = environment.service_mut(service)?;
    let mount = ConfigAttachment {
        config_resource_id: id.clone(),
        mount_dir: dir.clone(),
    };
    if node.config_attachments.contains(&mount) {
        return Ok(false);
    }
    // One directory holds one thing: a Volume or a Config.
    let same_volume = node
        .volume_attachments
        .iter()
        .filter(|held| held.mount_path == dir.as_str())
        .find_map(|held| {
            volumes
                .iter()
                .find(|volume| volume.resource_id == held.volume_resource_id)
                .map(|volume| format!("Volume {}", volume.name))
        });
    let same_config = node
        .config_attachments
        .iter()
        .filter(|held| held.config_resource_id != id && held.mount_dir == dir)
        .find_map(|held| {
            configs
                .iter()
                .find(|other| other.resource_id == held.config_resource_id)
                .map(|other| format!("Config {}", other.name))
        });
    if let Some(what) = same_volume.or(same_config) {
        return Err(error::conflict(
            format!("{service} already mounts {what} at {dir}; pick another directory"),
            json!({ "service": service, "dir": dir }),
        ));
    }
    node.config_attachments
        .retain(|mount| mount.config_resource_id != id);
    node.config_attachments.push(mount);
    if let Some(error) = collision(service, node, &configs) {
        return Err(error);
    }
    Ok(true)
}

/// Why `service`'s Config Mounts cannot all apply: two files land on one path, or a
/// file sits where another needs a directory.
fn collision(
    service: &ServiceName,
    node: &SavedServiceIntent,
    configs: &[SavedConfigIntent],
) -> Option<RpcError> {
    let mut paths: BTreeMap<String, &ConfigName> = BTreeMap::new();
    for mount in &node.config_attachments {
        let config = configs
            .iter()
            .find(|config| config.resource_id == mount.config_resource_id)?;
        let dir = mount.mount_dir.as_str().trim_end_matches('/');
        for name in config.files.keys() {
            let path = format!("{dir}/{name}");
            if let Some(other) = paths.insert(path.clone(), &config.name)
                && other != &config.name
            {
                return Some(error::conflict(
                    format!(
                        "{service} would mount {path} from both Config {other} and Config {}; pick another directory",
                        config.name
                    ),
                    json!({ "service": service, "path": path, "configs": [other, &config.name] }),
                ));
            }
        }
    }
    let nested = paths.iter().find_map(|(path, config)| {
        path.match_indices('/')
            .find_map(|(at, _)| paths.get(&path[..at]).map(|file| (path, config, &path[..at], file)))
    });
    nested.map(|(path, config, file, other)| {
        error::conflict(
            format!(
                "{service} would mount {path} from Config {config} under {file}, a file of Config {other}; pick another directory"
            ),
            json!({ "service": service, "path": path, "file": file }),
        )
    })
}

/// Unmount Config `config` from `service`; the Config stays. Returns whether Working
/// State changed.
pub(crate) fn detach(
    environment: &mut Environment,
    service: &ServiceName,
    config: &ConfigName,
) -> Result<bool, RpcError> {
    let id = environment.config(config)?.resource_id.clone();
    let node = environment.service_mut(service)?;
    let before = node.config_attachments.len();
    node.config_attachments
        .retain(|mount| mount.config_resource_id != id);
    Ok(node.config_attachments.len() != before)
}

/// `text` as a file's parts, referencing Services by their names in `names`
/// (lineage → name).
///
/// # Errors
/// `invalid_argument` for text over 256 KB, with a null character, an unfinished
/// reference, a bare `${{ KEY }}`, or a reference naming no Service.
fn parts(
    file: &ConfigFileName,
    text: &str,
    names: &BTreeMap<String, String>,
) -> Result<Vec<ValuePart>, RpcError> {
    if text.len() > MAX_FILE_BYTES {
        return Err(error::invalid(
            format!(
                "{file}: a Config file holds at most 256 KB of text; this one is {} KB",
                text.len().div_ceil(1024)
            ),
            json!({ "file": file, "bytes": text.len(), "max_bytes": MAX_FILE_BYTES }),
        ));
    }
    if text.contains('\0') {
        return Err(error::invalid(
            format!("{file}: null characters are not allowed"),
            json!({ "file": file }),
        ));
    }
    let template = parse_variable_template(text, |name| {
        names
            .iter()
            .find(|(_, slug)| slug.as_str() == name)
            .map(|(lineage, _)| lineage.clone())
    });
    if template.unterminated {
        return Err(error::invalid(
            format!(
                "{file}: a `${{{{` has no closing `}}}}`: finish the reference, or write `$${{{{` for a literal `${{{{`"
            ),
            json!({ "file": file }),
        ));
    }
    if template.malformed {
        return Err(error::invalid(
            format!(
                "{file}: a `${{{{ }}}}` is not a reference: write `${{{{ service.KEY }}}}`, or `$${{{{` for a literal `${{{{`"
            ),
            json!({ "file": file }),
        ));
    }
    if let Some(name) = template.unresolved.first() {
        return Err(error::invalid(
            format!("{file}: `${{{{ {name}.KEY }}}}` names no Service in this Environment"),
            error::suggest(name, names.values().map(String::as_str)),
        ));
    }
    let bare = template.parts.iter().find_map(|part| match part {
        ValuePart::Ref {
            owner: ValuePartOwner::Self_,
            key,
        } => Some(key.as_str()),
        ValuePart::Ref { .. } | ValuePart::Text { .. } => None,
    });
    if let Some(key) = bare {
        let service = names.values().min().map_or("web", String::as_str);
        return Err(error::invalid(
            format!("{file}: Configs are shared. Write `${{{{ {service}.{key} }}}}`."),
            json!({ "file": file, "key": key }),
        ));
    }
    Ok(template.parts)
}


/// A Config file's row cell as reads show it: its text rendered, never its parts.
pub(crate) fn shown_file(file: Value, names: &BTreeMap<String, String>) -> Value {
    let Some(parts) = file.get("content") else {
        return file;
    };
    let parts: Vec<ValuePart> = serde_json::from_value(parts.clone()).unwrap_or_default();
    let mut shown = file;
    shown["content"] = Value::String(render_variable_parts(&parts, names));
    shown
}

/// Every Service name in `intents` by lineage, for rendering references; an earlier
/// intent's name wins.
pub(crate) fn names_in(intents: &[&SavedEnvironmentIntent]) -> BTreeMap<String, String> {
    intents
        .iter()
        .rev()
        .flat_map(|intent| &intent.services)
        .map(|service| (service.lineage_id.clone(), service.slug.clone()))
        .collect()
}

pub(crate) fn summary(
    node: &SavedConfigIntent,
    names: &BTreeMap<String, String>,
) -> Result<ConfigSummary, RpcError> {
    Ok(ConfigSummary {
        id: ConfigId::parse(node.resource_id.as_str()).map_err(|_| error::corrupt("Config ID"))?,
        name: node.name.clone(),
        files: node
            .files
            .iter()
            .map(|(name, file)| file_summary(name, file, names))
            .collect(),
    })
}

pub(crate) fn file_summary(
    name: &ConfigFileName,
    file: &SavedConfigFile,
    names: &BTreeMap<String, String>,
) -> ConfigFileSummary {
    let mut references: Vec<String> = file
        .referenced_lineages()
        .filter_map(|lineage| names.get(lineage).cloned())
        .collect();
    references.sort();
    references.dedup();
    ConfigFileSummary {
        name: name.clone(),
        bytes: render_variable_parts(&file.content, names).len(),
        mode: file.mode,
        uid: file.uid,
        gid: file.gid,
        references,
    }
}
