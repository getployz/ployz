use std::{fs, path::Path};

use ployz_core::{IngressProxyFragment, PreDeployCommand, PreDeployHook};

use super::{
    convert::{duration_millis, environment, invalid, shell},
    model::{ComposeError, RawCaddy, RawPreDeploy},
};

pub(super) fn caddy(
    value: Option<&RawCaddy>,
    directory: &Path,
    service: &str,
) -> Result<Option<IngressProxyFragment>, ComposeError> {
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
    (!config.is_empty())
        .then(|| IngressProxyFragment::parse_caddy(config))
        .transpose()
        .map_err(invalid)
}

pub(super) fn pre_deploy(
    value: Option<&RawPreDeploy>,
) -> Result<Option<PreDeployHook>, ComposeError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if let Some(key) = value.other.keys().next() {
        return Err(invalid(format!("invalid x-pre_deploy key: {key}")));
    }
    let command = PreDeployCommand::parse(shell(&value.command)?).map_err(invalid)?;
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
        user: None,
    }))
}
