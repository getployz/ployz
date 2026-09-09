//! Materialize only the captured inputs referenced by one remote Build.

use serde_norway::Value;

use crate::compose::build::is_remote_context;

use super::*;

impl BuildInputs {
    pub(in crate::compose) fn for_service(&self, name: &str) -> Result<Self, ComposeError> {
        let invalid = || ComposeError::Invalid("invalid captured Build recipe".into());
        let mut recipe: Value = serde_norway::from_slice(
            &fs::read(self.root.join("compose.yaml")).map_err(input_error)?,
        )
        .map_err(|_| invalid())?;
        let services = recipe
            .get_mut("services")
            .and_then(Value::as_mapping_mut)
            .ok_or_else(invalid)?;
        services.retain(|key, _| key.as_str() == Some(name));
        let build = services
            .get(name)
            .and_then(|service| service.get("build"))
            .and_then(Value::as_mapping)
            .ok_or_else(invalid)?;
        let mut paths = BTreeSet::new();
        let context = build
            .get("context")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        if !is_remote_context(context) {
            paths.insert(PathBuf::from(context));
            if let Some(dockerfile) = build.get("dockerfile").and_then(Value::as_str) {
                let dockerfile = Path::new(context).join(dockerfile);
                let mut ignore = dockerfile.as_os_str().to_owned();
                ignore.push(".dockerignore");
                if self.root.join(&ignore).try_exists().map_err(input_error)? {
                    paths.insert(ignore.into());
                }
                paths.insert(dockerfile);
            }
        }
        if let Some(contexts) = build.get("additional_contexts") {
            let local_path =
                |context: &str| (!is_remote_context(context)).then(|| PathBuf::from(context));
            match contexts {
                Value::Mapping(values) => paths.extend(
                    values
                        .values()
                        .filter_map(Value::as_str)
                        .filter_map(local_path),
                ),
                Value::Sequence(values) => paths.extend(
                    values
                        .iter()
                        .filter_map(|value| value.as_str()?.split_once('=').map(|(_, path)| path))
                        .filter_map(local_path),
                ),
                Value::Null => {}
                Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Tagged(_) => {
                    return Err(invalid());
                }
            }
        }
        if let Some(ssh) = build.get("ssh").and_then(Value::as_sequence) {
            for key in ssh {
                let (_, keys) = key
                    .as_str()
                    .and_then(|key| key.split_once('='))
                    .ok_or_else(invalid)?;
                paths.extend(keys.split(',').map(PathBuf::from));
            }
        }
        let names = build
            .get("secrets")
            .and_then(Value::as_sequence)
            .into_iter()
            .flatten()
            .map(|secret| {
                secret
                    .as_str()
                    .or_else(|| secret.get("source").and_then(Value::as_str))
                    .map(str::to_owned)
                    .ok_or_else(invalid)
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        if let Some(secrets) = recipe.get_mut("secrets").and_then(Value::as_mapping_mut) {
            secrets.retain(|key, _| key.as_str().is_some_and(|name| names.contains(name)));
            for secret in secrets.values() {
                paths.insert(PathBuf::from(
                    secret
                        .get("file")
                        .and_then(Value::as_str)
                        .ok_or_else(invalid)?,
                ));
            }
        }
        // The explicitly supplied, normalized Docker configuration is command-wide.
        // Its original capture can contain discarded fields and is never uploaded.
        if self.root.join("private/docker").exists() {
            paths.insert("private/docker".into());
        }
        let inputs = Self::new()?;
        let paths = paths
            .into_iter()
            .map(|path| {
                let source = self.root.join(path).canonicalize().map_err(input_error)?;
                let relative = source
                    .strip_prefix(&self.root)
                    .map_err(|_| invalid())?
                    .to_owned();
                if !relative.starts_with("source") && !relative.starts_with("private") {
                    return Err(invalid());
                }
                Ok(relative)
            })
            .collect::<Result<BTreeSet<_>, ComposeError>>()?;
        for path in &paths {
            if paths
                .iter()
                .any(|parent| parent != path && path.starts_with(parent))
            {
                continue;
            }
            let source = self.root.join(path);
            copy(&source, &inputs.root.join(path), &source, None).map_err(input_error)?;
        }
        let metadata = self.root.join("private/railpack.json");
        if metadata.exists() {
            let mut recipes: Vec<ployz_build::Railpack> =
                serde_json::from_slice(&fs::read(metadata).map_err(input_error)?)
                    .map_err(|_| invalid())?;
            recipes.retain(|recipe| recipe.name == name);
            inputs.railpack(&recipes)?;
        }
        inputs.compose(&serde_norway::to_string(&recipe).map_err(|_| invalid())?)?;
        Ok(inputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_service_upload_contains_only_referenced_inputs() {
        let inputs = BuildInputs::new().unwrap();
        for path in [
            "source/api",
            "source/web",
            "source/shared",
            "private/docker",
        ] {
            fs::create_dir(inputs.root.join(path)).unwrap();
        }
        for path in [
            "source/api/code",
            "source/web/code",
            "source/shared/artifact",
            "source/api.Dockerfile",
            "source/api.Dockerfile.dockerignore",
            "source/web.Dockerfile",
            "private/api-key",
            "private/web-key",
            "private/api-token",
            "private/web-token",
            "private/shared-token",
            "private/docker/config.json",
            "private/original-docker-config",
        ] {
            fs::write(inputs.root.join(path), path).unwrap();
        }
        inputs
            .compose(
                r#"
services:
  api:
    build:
      context: source/api
      dockerfile: ../api.Dockerfile
      additional_contexts: {shared: source/shared, base: 'service:web'}
      ssh: ['default=private/api-key']
      secrets: [api-token, {source: shared-token, target: shared}]
  web:
    build:
      context: source/web
      dockerfile: ../web.Dockerfile
      additional_contexts: ['shared=source/shared']
      ssh: ['default=private/web-key']
      secrets: [web-token, shared-token]
secrets:
  api-token: {file: private/api-token}
  web-token: {file: private/web-token}
  shared-token: {file: private/shared-token}
"#,
            )
            .unwrap();
        for (name, other) in [("api", "web"), ("web", "api")] {
            let upload = inputs.for_service(name).unwrap();
            for required in [
                format!("source/{name}/code"),
                format!("source/{name}.Dockerfile"),
                format!("private/{name}-key"),
                format!("private/{name}-token"),
                "source/shared/artifact".into(),
                "private/shared-token".into(),
                "private/docker/config.json".into(),
            ] {
                assert!(
                    upload.root.join(&required).exists(),
                    "{name} lost {required}"
                );
            }
            for forbidden in [
                format!("source/{other}"),
                format!("source/{other}.Dockerfile"),
                format!("private/{other}-key"),
                format!("private/{other}-token"),
                "private/original-docker-config".into(),
            ] {
                assert!(
                    !upload.root.join(&forbidden).exists(),
                    "{name} received {forbidden}"
                );
            }
            assert_eq!(
                upload
                    .root
                    .join("source/api.Dockerfile.dockerignore")
                    .exists(),
                name == "api"
            );
            let recipe: serde_norway::Value =
                serde_norway::from_slice(&fs::read(upload.root.join("compose.yaml")).unwrap())
                    .unwrap();
            assert!(
                recipe
                    .get("secrets")
                    .unwrap()
                    .get(format!("{other}-token"))
                    .is_none()
            );
            assert!(recipe.get("services").unwrap().get(other).is_none());
        }
    }

    #[test]
    fn per_service_capture_keeps_only_its_railpack_recipe() {
        let inputs = BuildInputs::new().unwrap();
        for name in ["api", "web", "base"] {
            fs::create_dir(inputs.root.join("source").join(name)).unwrap();
        }
        inputs
            .compose("services: {api: {build: {context: source/api}}, web: {build: {context: source/web}}, base: {build: {context: source/base}}}\n")
            .unwrap();
        inputs
            .railpack(&["api", "web"].map(|name| ployz_build::Railpack {
                name: name.into(),
                context: format!("source/{name}").into(),
                variables: BTreeMap::new(),
                refresh_cache: false,
            }))
            .unwrap();
        for name in ["api", "web", "base"] {
            let selected = inputs.for_service(name).unwrap();
            let metadata = selected.root().join("private/railpack.json");
            if name == "base" {
                assert!(!metadata.exists());
            } else {
                let recipes: Vec<ployz_build::Railpack> =
                    serde_json::from_slice(&fs::read(metadata).unwrap()).unwrap();
                assert_eq!(recipes.len(), 1);
                assert_eq!(recipes.first().unwrap().name, name);
            }
        }
        let original: Vec<ployz_build::Railpack> =
            serde_json::from_slice(&fs::read(inputs.root().join("private/railpack.json")).unwrap())
                .unwrap();
        assert_eq!(
            original.len(),
            2,
            "per-target selection changed the original capture"
        );
    }
}
