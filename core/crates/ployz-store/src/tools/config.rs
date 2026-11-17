//! Tools on Configs, files mounted into Services: write files, rename, remove them.

use std::collections::BTreeMap;

use ployz_core::config::FileMode;
use ployz_core::{ConfigFileName, ConfigName, ServiceName};
use serde_json::{Value, json};

use super::input::tool_input;
use super::machine::{Cx, Effect, Stop, Tool, written};
use super::refusal::Refusal;
use crate::catalog::NODE_NAME;
use crate::{
    AttachConfig, BatchCommand, ConfigsQuery, CreateConfig, DeleteConfig, PutConfigFile,
    RemoveConfigFile, RenameConfig, Written,
};

tool_input! {
    pub(crate) struct ConfigWrite {
        /// Config name, e.g. `nginx`. Created if the Environment has none by that name.
        #[schemars(with = "String", pattern(NODE_NAME))]
        config: ConfigName,
        /// Relative file paths (1 to 4 segments) mapped to their full text, e.g. `nginx.conf`. Each file is at most 256 KB.
        #[schemars(length(min = 1))]
        files: BTreeMap<String, String>,
        /// Octal mode for every file written, e.g. `0440`. By default a file keeps its mode and a new one gets 0444.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern("^0?[0-7]{1,3}$"))]
        mode: Option<FileMode>,
        /// Owner user ID for every file written. By default a file keeps its own and a new one gets 0.
        #[serde(default)]
        uid: Option<u32>,
        /// Owner group ID for every file written. By default a file keeps its own and a new one gets 0.
        #[serde(default)]
        gid: Option<u32>,
        /// Service to mount the Config into, e.g. `web`. Needs path.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern(NODE_NAME))]
        service: Option<ServiceName>,
        /// Directory in the Service's containers where the files appear, e.g. `/etc/nginx`. Needs service.
        #[serde(default)]
        #[schemars(pattern("^/"))]
        path: Option<String>,
        @revision,
        @place(named),
    }
}

impl Tool for ConfigWrite {
    const NAME: &'static str = "config_write";
    const TITLE: &'static str = "Write files into a Config";
    const DESCRIPTION: &'static str = "Write text files into a Config, creating the Config if it is new. Files you leave out stay as they are. Pass service and path to also mount it into a Service. A file can read a Service variable as ${{ SERVICE.KEY }}. Staged until the next deploy.";
    // The same input again finds the same files and mount in place.
    const EFFECT: Effect = Effect::Change { idempotent: true };

    fn example() -> Value {
        json!({
            "config": "nginx",
            "files": { "nginx.conf": "events {}" },
            "service": "web",
            "path": "/etc/nginx"
        })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
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
                ConfigFileName::parse(name)
                    .map(|file| (file, content))
                    .map_err(|_| {
                        Stop::from(Refusal::invalid(
                            "A file name is a relative path of 1 to 4 segments",
                            "files",
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if files.is_empty() {
            return Err(Refusal::invalid("Give at least one file", "files").into());
        }
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
            "at": staged.environment.to_string(),
            "config": staged.config.name,
            "created": !exists,
            "written": names,
            "mounted": mount.map(|(service, path)| json!({ "service": service, "path": path })),
            "staged": true,
        }))
    }
}

tool_input! {
    pub(crate) struct ConfigRename {
        /// The Config's current name.
        #[schemars(with = "String", pattern(NODE_NAME))]
        config: ConfigName,
        /// New name, e.g. `proxy`.
        #[schemars(with = "String", pattern(NODE_NAME))]
        to: ConfigName,
        @revision,
        @place(here),
    }
}

impl Tool for ConfigRename {
    const NAME: &'static str = "config_rename";
    const TITLE: &'static str = "Rename a Config";
    const DESCRIPTION: &'static str = "Rename a Config. Its files and mounts stay as they are. Staged until the next deploy. Fails if the new name is taken.";
    // Sent again, it finds the new name taken and is refused.
    const EFFECT: Effect = Effect::Change { idempotent: true };

    fn example() -> Value {
        json!({ "config": "nginx", "to": "proxy" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
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
            "at": staged.environment.to_string(),
            "config": staged.config.name,
            "renamed_from": self.config,
            "staged": true,
        }))
    }
}

tool_input! {
    pub(crate) struct ConfigRemove {
        /// The Config's name.
        #[schemars(with = "String", pattern(NODE_NAME))]
        config: ConfigName,
        /// One file to remove, by its path in the Config, e.g. `nginx.conf`. Leave out to remove the whole Config.
        #[serde(default)]
        #[schemars(with = "Option<String>", length(min = 1))]
        file: Option<ConfigFileName>,
        @revision,
        @place(named),
    }
}

impl Tool for ConfigRemove {
    const NAME: &'static str = "config_remove";
    const TITLE: &'static str = "Remove a Config or one file";
    const DESCRIPTION: &'static str = "Remove one file from a Config, or the whole Config, which also unmounts it from every Service. Staged until the next deploy.";
    // Sent again, it finds no such file or Config and is refused.
    const EFFECT: Effect = Effect::Change { idempotent: true };

    fn example() -> Value {
        json!({ "config": "nginx", "file": "nginx.conf" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
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
            "at": staged.environment.to_string(),
            "config": staged.config.name,
            "whole": self.file.is_none(),
            "file": self.file,
            "staged": true,
        }))
    }
}
