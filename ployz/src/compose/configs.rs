use std::{collections::BTreeMap, fs, path::Path};

use ployz_core::{ConfigMount, ConfigSpec, ContainerPath};

use super::{
    convert::{file_mode, invalid},
    model::{ComposeError, RawProject, RawService, RawServiceConfig},
};

pub(super) fn configs(
    service: &RawService,
    root: &RawProject,
    directory: &Path,
) -> Result<(Vec<ConfigSpec>, Vec<ConfigMount>), ComposeError> {
    if short_config(service).is_some() {
        return Err(invalid("short-syntax configs are not supported"));
    }
    let mut specs = BTreeMap::new();
    let mut mounts = Vec::new();
    for item in &service.configs {
        // TODO(UT-053): short-syntax configs remain unsupported.
        let RawServiceConfig::Long {
            source,
            target,
            uid,
            gid,
            mode,
        } = item
        else {
            return Err(invalid("short-syntax configs are not supported"));
        };
        let config = root
            .configs
            .get(source)
            .ok_or_else(|| invalid(format!("config '{source}' not found in project configs")))?;
        let content = if let Some(file) = &config.file {
            fs::read(directory.join(file)).map_err(|error| {
                ComposeError::Io(format!("read config from file '{file}': {error}"))
            })?
        } else {
            config.content.clone().unwrap_or_default().into_bytes()
        };
        specs.entry(source.clone()).or_insert(ConfigSpec {
            name: source.clone(),
            content,
        });
        mounts.push(ConfigMount {
            config_name: source.clone(),
            target: Some(
                ContainerPath::parse(target.clone().unwrap_or_else(|| format!("/{source}")))
                    .map_err(invalid)?,
            ),
            uid: uid
                .as_ref()
                .map(|uid| {
                    uid.parse()
                        .map_err(|_| invalid("config uid must be an integer"))
                })
                .transpose()?,
            gid: gid
                .as_ref()
                .map(|gid| {
                    gid.parse()
                        .map_err(|_| invalid("config gid must be an integer"))
                })
                .transpose()?,
            mode: mode
                .as_ref()
                .map(|mode| file_mode(mode).ok_or_else(|| invalid("config mode must be octal")))
                .transpose()?,
        });
    }
    Ok((specs.into_values().collect(), mounts))
}

pub(super) fn short_config(service: &RawService) -> Option<&str> {
    service.configs.iter().find_map(|config| match config {
        RawServiceConfig::Short(name) => Some(name.as_str()),
        RawServiceConfig::Long { .. } => None,
    })
}
