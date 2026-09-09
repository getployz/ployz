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

/// Validate a captured recipe, isolate its registry configuration, and return
/// checked private Railpack inputs for execution.
/// # Errors
/// Rejects uncaptured paths, unsupported host settings, and SSH contexts without
/// a captured default key; reports missing or malformed input.
pub fn validate_capture(
    root: &Path,
    definition: &Definition,
) -> Result<Vec<crate::Railpack>, InputError> {
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
        let mut default_ssh = false;
        if let Some(ssh) = build.get("ssh") {
            for key in sequence(ssh)? {
                let (id, paths) = text(key)?
                    .split_once('=')
                    .ok_or("SSH agent sockets cannot be sent to a Build host")?;
                if id.is_empty() {
                    return Err("invalid Build SSH key ID".into());
                }
                default_ssh |= id == "default";
                for path in paths.split(',') {
                    contained(root, Path::new(path), "private", true)?;
                }
            }
        }
        let context = text(
            build
                .get("context")
                .ok_or("Build recipe has no captured context")?,
        )?;
        context_path(root, context, &names, default_ssh)?;
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
                        context_path(root, text(value)?, &names, default_ssh)?;
                    }
                }
                Value::Sequence(contexts) => {
                    for value in contexts {
                        let (_, value) = text(value)?
                            .split_once('=')
                            .ok_or("invalid named Build context")?;
                        context_path(root, value, &names, default_ssh)?;
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
        if let Some(platforms) = build.get("platforms") {
            let platforms = sequence(platforms)?;
            let target = definition
                .targets
                .iter()
                .find(|target| target.name == name)
                .expect("validated target");
            if platforms.iter().map(text).collect::<Result<Vec<_>, _>>()? != target.platforms {
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
    let railpack = railpack(root, definition, services)?;
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
    Ok(railpack)
}

/// Read private Railpack metadata only after validating its source and admitted targets.
fn railpack(
    root: &Path,
    definition: &Definition,
    services: &Mapping,
) -> Result<Vec<crate::Railpack>, InputError> {
    let bytes = match fs::read(root.join("private/railpack.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("cannot read captured Railpack recipes".into()),
    };
    let recipes: Vec<crate::Railpack> =
        serde_json::from_slice(&bytes).map_err(|_| "invalid captured Railpack recipes")?;
    let mut names = BTreeSet::new();
    for recipe in &recipes {
        if !names.insert(&recipe.name)
            || !definition
                .targets
                .iter()
                .any(|target| target.name == recipe.name)
        {
            return Err("Railpack recipe does not identify one admitted target".into());
        }
        contained(root, &recipe.context, "source", false)?;
        if !root.join(&recipe.context).is_dir() {
            return Err("Railpack context must be a captured directory".into());
        }
        let service = mapping(
            services
                .get(recipe.name.as_str())
                .ok_or("Railpack target is missing")?,
        )?;
        let build = mapping(service.get("build").ok_or("Railpack build is missing")?)?;
        if build.get("context").and_then(Value::as_str).map(Path::new)
            != Some(recipe.context.as_path())
            || build.contains_key("dockerfile")
            || build.contains_key("dockerfile_inline")
        {
            return Err("Railpack recipe differs from its captured Build".into());
        }
    }
    Ok(recipes)
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

fn context_path(
    root: &Path,
    value: &str,
    names: &BTreeSet<&str>,
    default_ssh: bool,
) -> Result<(), InputError> {
    if let Some(service) = value.strip_prefix("service:") {
        return if names.contains(service) {
            Ok(())
        } else {
            Err("Build dependency is not captured".into())
        };
    }
    if remote(value) {
        validate_remote_context(value)?;
        if !default_ssh
            && (value.starts_with("git@")
                || url::Url::parse(value).is_ok_and(|url| url.scheme() == "ssh"))
        {
            return Err("remote SSH build contexts require a captured default SSH key; configure build.ssh with default=<key-file>".into());
        }
        return Ok(());
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
        || (matches!(url.scheme(), "http" | "https") && !url.path().ends_with(".git"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_railpack_metadata_cannot_escape_admitted_source_or_override_policy() {
        let root =
            std::env::temp_dir().join(format!("ployz-railpack-policy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("private/docker")).unwrap();
        fs::create_dir(root.join("source")).unwrap();
        fs::write(root.join("private/docker/config.json"), "{}").unwrap();
        fs::write(
            root.join("compose.yaml"),
            "services:\n  api:\n    build: {context: source}\n",
        )
        .unwrap();
        let definition = Definition {
            targets: vec![crate::Target {
                name: "api".into(),
                platforms: Vec::new(),
            }],
            output: crate::Output::Load,
            no_cache: false,
            pull: false,
        };
        let valid = serde_json::json!({"name":"api", "context":"source", "variables":{}, "refresh_cache":false});
        for invalid in [
            serde_json::json!({"context":"/tmp"}),
            serde_json::json!({"context":"source/../../tmp"}),
            serde_json::json!({"name":"unadmitted"}),
            serde_json::json!({"cpu_cores":8}),
        ] {
            let mut recipe = valid.clone();
            recipe
                .as_object_mut()
                .unwrap()
                .extend(invalid.as_object().unwrap().clone());
            fs::write(
                root.join("private/railpack.json"),
                serde_json::to_vec(&vec![recipe]).unwrap(),
            )
            .unwrap();
            assert!(validate_capture(&root, &definition).is_err());
        }
        fs::write(
            root.join("private/railpack.json"),
            serde_json::to_vec(&vec![valid]).unwrap(),
        )
        .unwrap();
        validate_capture(&root, &definition).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remote_ssh_contexts_need_a_default_key_inside_private_capture() {
        let root = std::env::temp_dir().join(format!("ployz-ssh-recipe-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("private/docker")).unwrap();
        fs::create_dir(root.join("source")).unwrap();
        fs::write(root.join("private/docker/config.json"), "{}").unwrap();
        fs::write(root.join("private/key"), "captured-key").unwrap();
        fs::write(root.join("source/key"), "source-content").unwrap();
        let definition = Definition {
            targets: vec![crate::Target {
                name: "api".into(),
                platforms: Vec::new(),
            }],
            output: crate::Output::Load,
            no_cache: false,
            pull: false,
        };
        for ssh in [
            "",
            "other=private/key",
            "default=private/missing",
            "default=source/key",
            "default=private/key",
        ] {
            fs::write(root.join("compose.yaml"), format!(
                "services:\n  api:\n    build:\n      context: SSH://git@example.test/repo#0123456789abcdef0123456789abcdef01234567\n      ssh: [{}]\n", if ssh.is_empty() { String::new() } else { format!("'{ssh}'") }
            )).unwrap();
            assert_eq!(
                validate_capture(&root, &definition).is_ok(),
                ssh == "default=private/key",
                "{ssh}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}
