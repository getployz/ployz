//! Bind completed Service outputs to BuildKit named contexts. The existing
//! Machine image server supplies bytes directly; no external registry fallback.

use crate::{BuildError, Request};
use serde_norway::Value;
use std::{fs, net::IpAddr};

pub(crate) fn prepare(request: &Request<'_>) -> Result<(), BuildError> {
    if request.image_contexts.is_empty() {
        return Ok(());
    }
    let invalid = |message: &str| BuildError::Request(message.into());
    let mut document: Value = serde_norway::from_slice(
        &fs::read(request.working_dir.join(request.compose_file)).map_err(input_error)?,
    )
    .map_err(|_| invalid("invalid captured Build recipe"))?;
    let services = document
        .get_mut("services")
        .and_then(Value::as_mapping_mut)
        .ok_or_else(|| invalid("Build recipe has no services"))?;
    for target in request.targets {
        let build = services
            .get_mut(Value::String(target.name.clone()))
            .and_then(|service| service.get_mut("build"))
            .and_then(Value::as_mapping_mut)
            .ok_or_else(|| invalid("Build target has no recipe"))?;
        let contexts = build.get("additional_contexts");
        let entries = match contexts {
            Some(Value::Mapping(values)) => values
                .iter()
                .map(|(name, value)| name.as_str().zip(value.as_str()))
                .collect::<Option<Vec<_>>>(),
            Some(Value::Sequence(values)) => values
                .iter()
                .map(|value| value.as_str().and_then(|value| value.split_once('=')))
                .collect::<Option<Vec<_>>>(),
            None | Some(Value::Null) => Some(Vec::new()),
            Some(_) => None,
        }
        .ok_or_else(|| invalid("invalid additional contexts"))?;
        let mut bound = serde_norway::Mapping::new();
        for (name, value) in entries {
            if let Some(dependency) = value.strip_prefix("service:") {
                let image = request.image_contexts.get(dependency).ok_or_else(|| {
                    invalid("Service image context has no completed Build output")
                })?;
                bound.insert(
                    name.into(),
                    format!(
                        "docker-image://{}/{}",
                        std::net::SocketAddr::new(
                            image
                                .source
                                .management_address
                                .0
                                .to_ipv4_mapped()
                                .map_or(IpAddr::V6(image.source.management_address.0), IpAddr::V4),
                            image.source.port,
                        ),
                        image.reference
                    )
                    .into(),
                );
            } else {
                bound.insert(name.into(), value.into());
            }
        }
        if !bound.is_empty() {
            build.insert("additional_contexts".into(), Value::Mapping(bound));
        }
    }
    fs::write(
        request.working_dir.join(request.compose_file),
        serde_norway::to_string(&document)
            .map_err(|error| BuildError::Request(error.to_string()))?,
    )
    .map_err(input_error)?;
    Ok(())
}

fn input_error(error: std::io::Error) -> BuildError {
    BuildError::Request(format!("prepare completed image contexts: {error}"))
}
