//! Cloud's frozen configuration and checked-out paths enter the CLI capture seam here.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::compose::{
    BuildOptions, BuildSpec, CapturedBuild, CapturedCompose, ComposeProject, LoadOptions,
};
use ployz_core::{
    RpcError, RpcErrorCode, ServiceName,
    config::{ServiceBuilder, ServiceSource},
};
use serde::Deserialize;
use serde_json::{Value, json};

/// Backend-only frozen settings and repository directories, keyed by runtime Service name.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationInput {
    pub deployment: Value,
    pub sources: BTreeMap<ServiceName, PathBuf>,
}

fn invalid(message: impl ToString) -> RpcError {
    RpcError {
        code: RpcErrorCode::InvalidArgument,
        message: message.to_string(),
        details: Value::Null,
    }
}

/// Capture authorized checkouts using exactly the existing Compose build machinery.
pub(super) fn capture(
    mut input: PreparationInput,
) -> Result<(CapturedCompose, Option<CapturedBuild>), RpcError> {
    let snapshots = input
        .deployment
        .get_mut("snapshots")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| invalid("deployment snapshots must be an array"))?;
    let mut builds = BTreeMap::new();
    for snapshot in snapshots {
        let config = ployz_core::config::parse_service_config(snapshot["config"].clone())
            .map_err(invalid)?;
        if let ServiceSource::Git { root_dir, .. } = &config.settings.source {
            let name = &config.settings.private_dns;
            let repository = input
                .sources
                .remove(name)
                .ok_or_else(|| invalid(format!("missing checkout for {name}")))?;
            let repository = repository
                .canonicalize()
                .map_err(|_| invalid("checkout directory is unavailable"))?;
            let context = contained(&repository, &repository, root_dir)?;
            if !context.is_dir() {
                return Err(invalid("source root must be a directory"));
            }
            let mut build = json!({ "context": context, "x-recipe": match config.settings.build.builder {
                ServiceBuilder::Dockerfile => "dockerfile",
                ServiceBuilder::Railpack => "railpack",
            }});
            if config.settings.build.builder == ServiceBuilder::Dockerfile {
                let dockerfile = config
                    .settings
                    .build
                    .dockerfile_path
                    .as_deref()
                    .unwrap_or("Dockerfile");
                let dockerfile = contained(&repository, &context, dockerfile)?;
                if !dockerfile.is_file() {
                    return Err(invalid("Dockerfile must be a file"));
                }
                build
                    .as_object_mut()
                    .expect("build object")
                    .insert("dockerfile".into(), json!(dockerfile));
            }
            if config.settings.build.builder == ServiceBuilder::Railpack
                && let Some(command) = &config.settings.build.command
            {
                build
                    .as_object_mut()
                    .expect("build object")
                    .insert("args".into(), json!({"RAILPACK_BUILD_CMD": command}));
            }
            builds.insert(
                name.to_string(),
                BuildSpec {
                    raw: serde_norway::to_value(build).map_err(invalid)?,
                },
            );
            // This tag never escapes preparation: bind_builds replaces it with verified content.
            snapshot
                .get_mut("config")
                .and_then(Value::as_object_mut)
                .expect("validated config object")
                .insert(
                    "source".into(),
                    json!({"type":"image", "version":1,
                "image":format!("ployz-build/{name}:pending"), "credentials":{"type":"none"}}),
                );
        }
    }
    if !input.sources.is_empty() {
        return Err(invalid("checkout supplied for a non-Git service"));
    }
    let intent = ployz_core::config::lower_deployment(
        serde_json::from_value(input.deployment).map_err(invalid)?,
    )
    .map_err(invalid)?;
    let dependencies = intent
        .dependencies()
        .iter()
        .map(|(name, edges)| (name.to_string(), edges.clone()))
        .collect();
    let services = intent
        .target
        .into_iter()
        .map(|s| (s.name.to_string(), s))
        .collect::<BTreeMap<_, _>>();
    // Explicit empty provider environment prevents capture from reading Cloud's HOME,
    // Docker credentials or process variables. Runtime variables are already resolved.
    let project = ComposeProject::from_frozen_services(
        intent.project_name.to_string(),
        services,
        builds,
        dependencies,
    );
    crate::preparation::capture(
        project,
        intent.project_name,
        intent.options,
        &LoadOptions::default(),
        &BuildOptions::default(),
        false,
        None,
    )
    .map_err(invalid)
}

fn contained(root: &Path, base: &Path, setting: &str) -> Result<PathBuf, RpcError> {
    let path = base
        .join(setting.trim_start_matches('/'))
        .canonicalize()
        .map_err(|_| invalid("source path does not exist"))?;
    if !path.starts_with(root) {
        return Err(invalid("source path escapes its repository root"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_preserves_cloud_dependencies() {
        let deployment = json!({
            "projectName": "app",
            "snapshots": (["web", "db"].map(|name| json!({"config": {
                "version": 2, "privateDns": name,
                "healthcheck": {"type":"none"}, "restartPolicy":"on-failure",
                "source": {"type": "image", "version": 1, "image": "nginx:latest", "credentials": {"type": "none"}}
            }}))),
            "dependencies": {"web": [{"service": "db", "condition": "service_started"}]}
        });
        let (captured, build) = capture(PreparationInput {
            deployment,
            sources: BTreeMap::new(),
        })
        .unwrap();
        assert!(build.is_none());
        let dependencies = captured.intent().dependencies();
        assert_eq!(
            dependencies
                .get(&ServiceName::parse("web").unwrap())
                .unwrap()
                .first()
                .unwrap()
                .service
                .as_str(),
            "db"
        );
    }

    #[test]
    fn repository_paths_cannot_escape_through_parent_or_symlink() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("app")).unwrap();
        std::os::unix::fs::symlink("/", temp.path().join("outside")).unwrap();
        assert_eq!(
            contained(temp.path(), temp.path(), "/app").unwrap(),
            temp.path().join("app")
        );
        std::fs::create_dir_all(temp.path().join("apps/api")).unwrap();
        std::fs::write(temp.path().join("Dockerfile"), "FROM scratch").unwrap();
        assert_eq!(
            contained(
                temp.path(),
                &temp.path().join("apps/api"),
                "../../Dockerfile"
            )
            .unwrap(),
            temp.path().join("Dockerfile")
        );
        assert!(contained(temp.path(), &temp.path().join("apps/api"), "../../../").is_err());
        assert!(contained(temp.path(), temp.path(), "../").is_err());
        assert!(contained(temp.path(), temp.path(), "outside/etc").is_err());
    }
}
