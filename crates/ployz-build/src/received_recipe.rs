//! Validate path-bearing normalized input at the remote trust boundary.
//! Buildx remains the Compose translator and Dockerfile frontend owner.

use crate::remote::Definition;
use crate::remote::InputError;
use serde_norway::{Mapping, Value};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path},
};

pub(crate) fn validate(root: &Path, definition: &Definition) -> Result<(), InputError> {
    let names: BTreeSet<_> = definition
        .targets
        .iter()
        .map(|target| target.name.as_str())
        .collect();
    if names.is_empty()
        || names.len() > 128
        || names.len() != definition.targets.len()
        || names.iter().any(|name| {
            !name
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
                || !name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        })
    {
        return Err("invalid Build target names".into());
    }
    let bytes = fs::read(root.join("compose.yaml")).map_err(|_| "Build recipe is missing")?;
    let document: Value =
        serde_norway::from_slice(&bytes).map_err(|_| "invalid captured Build recipe")?;
    no_interpolation(&document)?;
    let top = mapping(&document)?;
    only(top, &["services", "secrets"], "recipe")?;
    let services = mapping(top.get("services").ok_or("Build recipe has no services")?)?;
    if services.len() != names.len() {
        return Err("Build recipe targets differ from the admitted request".into());
    }
    for (name, service) in services {
        let name = text(name)?;
        if !names.contains(name) {
            return Err("Build recipe has an unadmitted target".into());
        }
        let service = mapping(service)?;
        only(service, &["image", "build"], "service")?;
        let build = mapping(service.get("build").ok_or("Build target has no recipe")?)?;
        only(
            build,
            &[
                "context",
                "dockerfile",
                "dockerfile_inline",
                "target",
                "tags",
                "labels",
                "args",
                "secrets",
                "ssh",
                "additional_contexts",
                "platforms",
                "cache_from",
                "cache_to",
                "network",
                "extra_hosts",
                "x-bake",
                "no_cache",
                "pull",
            ],
            "build",
        )?;
        let context = text(
            build
                .get("context")
                .ok_or("Build recipe has no captured context")?,
        )?;
        context_path(root, context, &names)?;
        if let Some(recipe) = build.get("dockerfile") {
            let recipe = text(recipe)?;
            if remote(context) {
                if Path::new(recipe)
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
                {
                    return Err("remote Dockerfile path escapes its context".into());
                }
            } else {
                contained(root, &Path::new(context).join(recipe), "source", true)?;
            }
        }
        if let Some(contexts) = build.get("additional_contexts") {
            match contexts {
                Value::Mapping(contexts) => {
                    for value in contexts.values() {
                        context_path(root, text(value)?, &names)?;
                    }
                }
                Value::Sequence(contexts) => {
                    for value in contexts {
                        let (_, value) = text(value)?
                            .split_once('=')
                            .ok_or("invalid named Build context")?;
                        context_path(root, value, &names)?;
                    }
                }
                Value::Null => {}
                Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Tagged(_) => {
                    return Err("invalid named Build contexts".into());
                }
            }
        }
        if let Some(args) = build.get("args")
            && mapping(args)?.values().any(Value::is_null)
        {
            return Err("Build arguments must have captured values".into());
        }
        if let Some(ssh) = build.get("ssh") {
            for key in sequence(ssh)? {
                let (id, paths) = text(key)?
                    .split_once('=')
                    .ok_or("SSH agent sockets cannot be sent to a Build host")?;
                if id.is_empty() {
                    return Err("invalid Build SSH key ID".into());
                }
                for path in paths.split(',') {
                    contained(root, Path::new(path), "private", true)?;
                }
            }
        }
        if let Some(platforms) = build.get("platforms") {
            let platforms = sequence(platforms)?;
            let target = definition
                .targets
                .iter()
                .find(|target| target.name == name)
                .expect("validated target");
            if platforms.len() != usize::from(target.platform.is_some())
                || platforms.first().map(text).transpose()? != target.platform.as_deref()
            {
                return Err("Build recipe platforms differ from the admitted request".into());
            }
        }
        if build
            .get("network")
            .is_some_and(|value| !matches!(value.as_str(), Some("default" | "none")))
        {
            return Err("host-specific build.network is unsupported".into());
        }
        if let Some(hosts) = build.get("extra_hosts")
            && serde_norway::to_string(hosts)
                .map_err(|_| "invalid extra_hosts")?
                .contains("host-gateway")
        {
            return Err("host-specific build.extra_hosts is unsupported".into());
        }
        if let Some(bake) = build.get("x-bake") {
            only(mapping(bake)?, &["no-cache-filter"], "build.x-bake")?;
        }
        for cache in ["cache_from", "cache_to"] {
            if let Some(entries) = build.get(cache) {
                for entry in sequence(entries)? {
                    let value = text(entry)?;
                    let mut kinds = value
                        .split(',')
                        .filter_map(|part| part.trim().strip_prefix("type="));
                    let kind = kinds.next();
                    if kinds.next().is_some()
                        || kind.is_some_and(|kind| !matches!(kind, "registry" | "inline"))
                        || (kind.is_none() && value.contains('='))
                    {
                        return Err(format!("host-specific build.{cache} is unsupported").into());
                    }
                }
            }
        }
    }
    if let Some(secrets) = top.get("secrets") {
        for value in mapping(secrets)?.values() {
            let secret = mapping(value)?;
            only(secret, &["file"], "secret")?;
            contained(
                root,
                Path::new(text(
                    secret.get("file").ok_or("Build secret is not captured")?,
                )?),
                "private",
                true,
            )?;
        }
    }
    let config_path = root.join("private/docker/config.json");
    let mut config: serde_json::Value = serde_json::from_slice(
        &fs::read(&config_path).map_err(|_| "Build registry configuration is missing")?,
    )
    .map_err(|_| "invalid Build registry configuration")?;
    // Never use a client's plugin executable paths or credential helpers on
    // the host. Docker finds host-installed plugins through its system paths.
    let mut safe = serde_json::Map::new();
    if let Some(config) = config.as_object_mut() {
        for key in ["auths", "proxies"] {
            if let Some(value) = config.remove(key) {
                safe.insert(key.into(), value);
            }
        }
    }
    fs::write(
        config_path,
        serde_json::to_vec(&safe).map_err(|_| "invalid Build registry configuration")?,
    )
    .map_err(|_| "cannot isolate Build registry credentials")?;
    Ok(())
}

fn contained(root: &Path, path: &Path, area: &str, file: bool) -> Result<(), InputError> {
    if path.is_absolute() {
        return Err("Build recipe names an uncaptured host path".into());
    }
    let mut relative = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => relative.push(name),
            Component::CurDir => continue,
            Component::ParentDir if relative != Path::new(area) && relative.pop() => continue,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err("Build recipe path escapes staging".into());
            }
        }
        if !relative.starts_with(area) {
            return Err("Build recipe path escapes staging".into());
        }
    }
    let resolved = root
        .join(relative)
        .canonicalize()
        .map_err(|_| "Build recipe names missing captured input")?;
    if !resolved.starts_with(root.join(area)) || (file && !resolved.is_file()) {
        return Err("Build recipe path escapes its captured material".into());
    }
    Ok(())
}

fn context_path(root: &Path, value: &str, names: &BTreeSet<&str>) -> Result<(), InputError> {
    if let Some(service) = value.strip_prefix("service:") {
        return if names.contains(service) {
            Ok(())
        } else {
            Err("Build dependency is not captured".into())
        };
    }
    if remote(value) {
        return validate_remote_context(value);
    }
    contained(root, Path::new(value), "source", false)?;
    if !root.join(value).is_dir() {
        return Err("Build source context is not a directory".into());
    }
    Ok(())
}
fn remote(value: &str) -> bool {
    value.contains("://") || value.starts_with("git@")
}
fn mapping(value: &Value) -> Result<&Mapping, InputError> {
    value
        .as_mapping()
        .ok_or_else(|| "invalid Build recipe mapping".into())
}
fn sequence(value: &Value) -> Result<&[Value], InputError> {
    value
        .as_sequence()
        .map(Vec::as_slice)
        .ok_or_else(|| "invalid Build recipe sequence".into())
}
fn text(value: &Value) -> Result<&str, InputError> {
    value
        .as_str()
        .ok_or_else(|| "invalid Build recipe string".into())
}
fn only(mapping: &Mapping, keys: &[&str], location: &str) -> Result<(), InputError> {
    for key in mapping.keys() {
        let key = text(key)?;
        if !keys.contains(&key) {
            return Err(format!("unsupported remote {location}.{key}").into());
        }
    }
    Ok(())
}
fn no_interpolation(value: &Value) -> Result<(), InputError> {
    match value {
        Value::String(value) if value.replace("$$", "").contains('$') => {
            Err("Build recipe contains uncaptured interpolation".into())
        }
        Value::Mapping(values) => {
            for (key, value) in values {
                no_interpolation(key)?;
                no_interpolation(value)?;
            }
            Ok(())
        }
        Value::Sequence(values) => {
            for value in values {
                no_interpolation(value)?;
            }
            Ok(())
        }
        Value::Tagged(_) => Err("Build recipe contains a custom YAML tag".into()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Ok(()),
    }
}
/// Validate an immutable remote context without embedded private credentials.
/// # Errors
/// Rejects mutable references, unsupported protocols, and escaping subdirectories.
pub fn validate_remote_context(source: &str) -> Result<(), InputError> {
    let refusal = || {
        "remote build context must use an immutable Git commit or image digest; use a Git URL without embedded credentials and a contained subdirectory".into()
    };
    if let Some(image) = source.strip_prefix("docker-image://") {
        let reference: oci_client::Reference = image.parse().map_err(|_| refusal())?;
        return if reference
            .digest()
            .and_then(|digest| digest.strip_prefix("sha256:"))
            .is_some_and(|digest| {
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            }) {
            Ok(())
        } else {
            Err(refusal())
        };
    }
    // Normalize Git's scp spelling for URL validation, preserving the original
    // spelling handed to upstream fetching.
    let scp = source
        .strip_prefix("git@")
        .filter(|_| !source.contains("://"))
        .and_then(|source| source.split_once(':'))
        .map(|(host, path)| format!("ssh://git@{host}/{path}"));
    let url = url::Url::parse(scp.as_deref().unwrap_or(source)).map_err(|_| refusal())?;
    if !matches!(url.scheme(), "http" | "https" | "ssh" | "git")
        || url.host_str().is_none()
        || url.password().is_some()
        || (!url.username().is_empty() && url.scheme() != "ssh")
        || !url.path().ends_with(".git")
        || url.query().is_some()
    {
        return Err(refusal());
    }
    let (commit, directory) = url
        .fragment()
        .ok_or_else(refusal)?
        .split_once(':')
        .map_or((url.fragment().unwrap_or(""), ""), |pair| pair);
    if commit.len() != 40
        || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        || Path::new(directory).components().any(|part| {
            !matches!(
                part,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        return Err(refusal());
    }
    Ok(())
}
