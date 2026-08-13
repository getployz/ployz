use std::{fs, path::Path};

use ployz_core::PreDeployHook;

use super::{
    convert::{duration_millis, environment, invalid, scalar, shell},
    model::{ComposeError, RawCaddy, RawPreDeploy},
};

pub(super) fn caddy(
    value: Option<&RawCaddy>,
    directory: &Path,
    service: &str,
) -> Result<Option<String>, ComposeError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let config = match value {
        RawCaddy::String(config) => config.clone(),
        RawCaddy::Object { config, other } => {
            if let Some(key) = other.keys().next() {
                return Err(invalid(format!("invalid x-caddy key: {key}")));
            }
            config.clone()
        }
    };
    let config = if config.is_empty() || config.contains('\n') {
        config
    } else {
        fs::read_to_string(directory.join(&config)).map_err(|error| {
            ComposeError::Io(format!(
                "read Caddy config '{config}' for service '{service}': {error}"
            ))
        })?
    };
    let config = config.trim();
    Ok((!config.is_empty()).then(|| config.to_owned()))
}

pub(super) fn pre_deploy(
    value: Option<&RawPreDeploy>,
) -> Result<Option<PreDeployHook>, ComposeError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let command = shell(&value.command)?;
    if command.is_empty() {
        return Err(invalid(
            "missing required attribute 'command' in x-pre_deploy",
        ));
    }
    // TODO(UT-048): unknown x-pre_deploy attributes remain ignored.
    let _ = &value.other;
    Ok(Some(PreDeployHook {
        command,
        environment: value
            .environment
            .as_ref()
            .map(environment)
            .transpose()?
            .unwrap_or_default(),
        privileged: value.privileged,
        timeout_millis: duration_millis(value.timeout.as_deref())?,
        user: value.user.as_ref().and_then(scalar),
    }))
}
