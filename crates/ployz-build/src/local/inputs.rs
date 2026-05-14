use ployz_build_api::{BuildInvocationPlan, DOCKER_BUILDKIT_ENV};
use ployz_model::{BuildEnvValue, BuildInputSummary, BuildInputs, BuildMethod, BuildSecretSummary};

pub fn plan_build_invocation(
    method: BuildMethod,
    inputs: &BuildInputs,
) -> Result<BuildInvocationPlan, String> {
    let mut summary = BuildInputSummary::default();
    let mut env = Vec::new();
    let mut plain_env = Vec::new();
    let mut secret_env = Vec::new();
    let mut secret_material = Vec::new();
    let mut secret_names = Vec::new();

    for (key, value) in &inputs.env {
        validate_build_input_key(key)?;
        match value {
            BuildEnvValue::Plain { value } => {
                summary.env.push(key.clone());
                env.push((key.clone(), value.clone()));
                plain_env.push((key.clone(), value.clone()));
            }
            BuildEnvValue::Secret { value, fingerprint } => {
                if key == DOCKER_BUILDKIT_ENV {
                    return Err(format!(
                        "build env secret cannot use reserved Docker client env key '{DOCKER_BUILDKIT_ENV}'"
                    ));
                }
                if let Some(fingerprint) = fingerprint {
                    validate_secret_fingerprint(fingerprint)?;
                }
                summary.secrets.push(BuildSecretSummary {
                    name: key.clone(),
                    fingerprint: fingerprint.clone(),
                });
                env.push((key.clone(), value.clone()));
                secret_env.push((key.clone(), value.clone()));
                secret_names.push(key.clone());
                secret_material.push((key.clone(), value.clone()));
            }
        }
    }

    let mut docker_build_args = Vec::new();
    for (key, value) in &inputs.docker_build_args {
        validate_build_input_key(key)?;
        if inputs.env.contains_key(key) {
            return Err(format!(
                "docker build arg '{key}' duplicates a build env key; use one input source for each key"
            ));
        }
        if secret_like_build_arg_name(key) {
            return Err(format!(
                "docker build arg '{key}' looks secret-bearing; pass it as build env secret instead"
            ));
        }
        summary.docker_build_args.push(key.clone());
        docker_build_args.push((key.clone(), value.clone()));
    }

    if method == BuildMethod::Railpack && !docker_build_args.is_empty() {
        return Err("railpack builds do not accept Dockerfile-only build args".into());
    }

    summary.env.sort();
    summary
        .secrets
        .sort_by(|left, right| left.name.cmp(&right.name));
    summary.docker_build_args.sort();
    env.sort_by(|left, right| left.0.cmp(&right.0));
    plain_env.sort_by(|left, right| left.0.cmp(&right.0));
    secret_env.sort_by(|left, right| left.0.cmp(&right.0));
    secret_names.sort();
    docker_build_args.sort_by(|left, right| left.0.cmp(&right.0));
    secret_material.sort_by(|left, right| left.0.cmp(&right.0));

    let railpack_secret_cache_required =
        method == BuildMethod::Railpack && !secret_material.is_empty();

    Ok(BuildInvocationPlan {
        summary,
        env,
        plain_env,
        secret_env,
        docker_build_args,
        buildkit_secret_env: secret_names,
        railpack_secret_cache_required,
    })
}

fn validate_build_input_key(key: &str) -> Result<(), String> {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return Err("build input key cannot be empty".into());
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return Err(format!(
            "build input key '{key}' must start with '_' or ASCII letter"
        ));
    }
    if !chars.all(|value| value == '_' || value.is_ascii_alphanumeric()) {
        return Err(format!(
            "build input key '{key}' may only contain ASCII letters, digits, and '_'"
        ));
    }
    Ok(())
}

fn validate_secret_fingerprint(fingerprint: &str) -> Result<(), String> {
    if fingerprint.is_empty() {
        return Err("secret fingerprint cannot be empty".into());
    }
    if fingerprint.len() > 128 {
        return Err("secret fingerprint cannot exceed 128 bytes".into());
    }
    if !fingerprint
        .chars()
        .all(|value| value.is_ascii_graphic() || value == ' ')
    {
        return Err("secret fingerprint must be printable ASCII".into());
    }
    if looks_like_secret_hash(fingerprint) {
        return Err("secret fingerprint must not be a raw hash of the secret value".into());
    }
    Ok(())
}

fn looks_like_secret_hash(value: &str) -> bool {
    let hash = value.strip_prefix("sha256:").unwrap_or(value);
    hash.len() == 64 && hash.chars().all(|character| character.is_ascii_hexdigit())
}

fn secret_like_build_arg_name(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    [
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "TOKEN",
        "PRIVATE_KEY",
        "ACCESS_KEY",
        "API_KEY",
        "CREDENTIAL",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}
