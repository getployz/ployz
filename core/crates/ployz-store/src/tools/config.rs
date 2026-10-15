//! Tools on Configs, files mounted into Services: write files, rename, remove them.

use std::collections::BTreeMap;

use ployz_core::config::FileMode;
use ployz_core::{ConfigFileName, ConfigName, ServiceName};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::machine::{Cx, Effect, Stop, Tool, at, written};
use super::refusal::Refusal;
use crate::catalog::NODE_NAME;
use crate::{
    AttachConfig, BatchCommand, ConfigsQuery, CreateConfig, DeleteConfig, EnvironmentName,
    ProjectName, PutConfigFile, RemoveConfigFile, RenameConfig, Written,
};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfigWrite {
    /// Config name. Created when it does not exist.
    #[schemars(with = "String", pattern(NODE_NAME))]
    config: ConfigName,
    /// File name to contents.
    #[schemars(length(min = 1))]
    files: BTreeMap<String, String>,
    /// Octal, e.g. 0440. Omit to keep.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern("^0?[0-7]{1,3}$"))]
    mode: Option<FileMode>,
    #[serde(default)]
    uid: Option<u32>,
    #[serde(default)]
    gid: Option<u32>,
    /// Service to mount it into, with path.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    service: Option<ServiceName>,
    /// Absolute directory in the Service's containers, with service.
    #[serde(default)]
    #[schemars(pattern("^/"))]
    path: Option<String>,
    /// Writes only if the place is still at this revision.
    #[serde(default)]
    expect_revision: Option<u64>,
    /// Project name.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for ConfigWrite {
    const NAME: &'static str = "config_write";
    const DESCRIPTION: &'static str = "Write files into a Config, creating it if needed, and optionally mount it into a Service at a directory. A file's text may read a Service variable as ${{ SERVICE.KEY }}. Staged.";
    const EFFECT: Effect = Effect::Change;

    fn example() -> Value {
        json!({
            "config": "nginx",
            "files": { "nginx.conf": "events {}" },
            "service": "web",
            "path": "/etc/nginx"
        })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let mount = match (self.service, self.path) {
            (Some(service), Some(dir)) => Some((service, dir)),
            (None, None) => None,
            (Some(_), None) => return Err(Refusal::invalid("A mount needs a path", "path").into()),
            (None, Some(_)) => {
                return Err(Refusal::invalid("A mount needs a service", "service").into());
            }
        };
        let files = self
            .files
            .into_iter()
            .map(|(name, content)| {
                ConfigFileName::parse(name.clone())
                    .map(|file| (file, content))
                    .map_err(|_| {
                        Stop::from(Refusal::invalid(
                            "A file name is a relative path of 1 to 4 segments",
                            &format!("files.{name}"),
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if files.is_empty() {
            return Err(Refusal::invalid("Give at least one file", "files").into());
        }
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let configs = cx.one(&ConfigsQuery {
            environment: scope.clone(),
        })?;
        let exists = configs
            .configs
            .iter()
            .any(|listing| listing.config.name == self.config);
        let mut commands = Vec::new();
        if !exists {
            commands.push(BatchCommand::CreateConfig(CreateConfig {
                id: cx.mint(None)?,
                environment: scope.clone(),
                name: self.config.clone(),
                mounts: Vec::new(),
            }));
        }
        let names = files
            .iter()
            .map(|(file, _)| file.to_string())
            .collect::<Vec<_>>();
        for (file, content) in files {
            commands.push(BatchCommand::PutConfigFile(PutConfigFile {
                environment: scope.clone(),
                config: self.config.clone(),
                file,
                content,
                mode: self.mode,
                uid: self.uid,
                gid: self.gid,
            }));
        }
        if let Some((service, dir)) = &mount {
            commands.push(BatchCommand::AttachConfig(AttachConfig {
                environment: scope.clone(),
                service: service.clone(),
                config: self.config.clone(),
                dir: dir.clone(),
            }));
        }
        let results = cx.batch(&scope, self.expect_revision, commands)?;
        let staged = written(results, |one| {
            if let Written::Config(staged) = one {
                Some(staged)
            } else {
                None
            }
        })?;
        Ok(json!({
            "at": at(&staged.environment),
            "config": staged.config.name,
            "created": !exists,
            "written": names,
            "mounted": mount.map(|(service, path)| json!({ "service": service, "path": path })),
            "staged": true,
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfigRename {
    #[schemars(with = "String", pattern(NODE_NAME))]
    config: ConfigName,
    /// New name.
    #[schemars(with = "String", pattern(NODE_NAME))]
    to: ConfigName,
    /// Writes only if the place is still at this revision.
    #[serde(default)]
    expect_revision: Option<u64>,
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for ConfigRename {
    const NAME: &'static str = "config_rename";
    const DESCRIPTION: &'static str = "Rename a Config. Its files and mounts stay. Staged.";
    const EFFECT: Effect = Effect::Change;

    fn example() -> Value {
        json!({ "config": "nginx", "to": "proxy" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let rename = BatchCommand::RenameConfig(RenameConfig {
            environment: scope.clone(),
            config: self.config.clone(),
            name: self.to,
        });
        let results = cx.batch(&scope, self.expect_revision, vec![rename])?;
        let staged = written(results, |one| {
            if let Written::ConfigRenamed(staged) = one {
                Some(staged)
            } else {
                None
            }
        })?;
        Ok(json!({
            "at": at(&staged.environment),
            "config": staged.config.name,
            "renamed_from": self.config,
            "staged": true,
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfigRemove {
    #[schemars(with = "String", pattern(NODE_NAME))]
    config: ConfigName,
    /// One file to remove. Omit to remove the whole Config and unmount it everywhere.
    #[serde(default)]
    #[schemars(with = "Option<String>", length(min = 1))]
    file: Option<ConfigFileName>,
    /// Writes only if the place is still at this revision.
    #[serde(default)]
    expect_revision: Option<u64>,
    /// Project name.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for ConfigRemove {
    const NAME: &'static str = "config_remove";
    const DESCRIPTION: &'static str =
        "Remove one file from a Config, or the whole Config and its mounts. Staged.";
    const EFFECT: Effect = Effect::Change;

    fn example() -> Value {
        json!({ "config": "nginx", "file": "nginx.conf" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let command = match &self.file {
            Some(file) => BatchCommand::RemoveConfigFile(RemoveConfigFile {
                environment: scope.clone(),
                config: self.config.clone(),
                file: file.clone(),
            }),
            None => BatchCommand::DeleteConfig(DeleteConfig {
                environment: scope.clone(),
                config: self.config.clone(),
            }),
        };
        let results = cx.batch(&scope, self.expect_revision, vec![command])?;
        let staged = written(results, |one| {
            if let Written::Config(staged) | Written::ConfigRemoved(staged) = one {
                Some(staged)
            } else {
                None
            }
        })?;
        Ok(json!({
            "at": at(&staged.environment),
            "config": staged.config.name,
            "whole": self.file.is_none(),
            "file": self.file,
            "staged": true,
        }))
    }
}
