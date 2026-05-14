use ployz_build_api::{
    BuildCommand, BuildCommandPaths, BuildCommandPlan, BuildCommandStep, BuildCommandStepKind,
    BuildInvocationPlan, DOCKER_BUILDKIT_ENV,
};
use ployz_model::BuildLocalRequest;

pub fn command_plan(
    request: &BuildLocalRequest,
    invocation: &BuildInvocationPlan,
    paths: BuildCommandPaths,
) -> Result<BuildCommandPlan, String> {
    let mut args = vec!["build".into(), "-t".into(), request.image_name.clone()];
    if let Some(platform) = &request.platform {
        args.push("--platform".into());
        args.push(format_platform(platform));
    };
    for (key, value) in &invocation.plain_env {
        args.push("--build-arg".into());
        args.push(format!("{key}={value}"));
    }
    for (key, value) in &invocation.docker_build_args {
        args.push("--build-arg".into());
        args.push(format!("{key}={value}"));
    }
    for (key, path) in &paths.buildkit_secret_files {
        args.push("--secret".into());
        args.push(format!("id={key},src={}", path.display()));
    }
    args.push(".".into());
    let env = if invocation.buildkit_secret_env.is_empty() {
        Vec::new()
    } else {
        vec![(DOCKER_BUILDKIT_ENV.into(), "1".into())]
    };
    Ok(BuildCommandPlan::new(
        Vec::new(),
        BuildCommandStep {
            kind: BuildCommandStepKind::ImageBuild,
            command: BuildCommand {
                program: "docker",
                args,
                env,
                redaction_values: command_redaction_values(invocation, &paths),
            },
        },
        paths.cleanup_dirs,
    ))
}

fn command_redaction_values(
    invocation: &BuildInvocationPlan,
    paths: &BuildCommandPaths,
) -> Vec<String> {
    let mut values = invocation
        .env
        .iter()
        .chain(invocation.docker_build_args.iter())
        .map(|(_key, value)| value.clone())
        .collect::<Vec<_>>();
    if let Some(token) = &paths.railpack_secret_cache_token {
        values.push(token.clone());
    }
    values
}

fn format_platform(platform: &ployz_model::ImagePlatform) -> String {
    match platform.variant.as_deref() {
        Some(variant) => format!("{}/{}/{}", platform.os, platform.architecture, variant),
        None => format!("{}/{}", platform.os, platform.architecture),
    }
}

#[cfg(test)]
mod tests {
    use ployz_build_api::{BuildCommandPaths, BuildInvocationPlan};
    use ployz_model::{BuildInputSummary, BuildInputs, BuildLocalRequest, BuildMethod};

    use super::*;

    #[test]
    fn dockerfile_plan_uses_docker_build() {
        let request = BuildLocalRequest {
            method: BuildMethod::Dockerfile,
            context_dir: ".".into(),
            image_name: "example:latest".into(),
            platform: None,
            push_target: None,
            distribute_targets: Vec::new(),
            inputs: BuildInputs::default(),
        };
        let plan = command_plan(
            &request,
            &BuildInvocationPlan {
                summary: BuildInputSummary::default(),
                env: Vec::new(),
                plain_env: Vec::new(),
                secret_env: Vec::new(),
                docker_build_args: Vec::new(),
                buildkit_secret_env: Vec::new(),
                railpack_secret_cache_required: false,
            },
            BuildCommandPaths {
                cleanup_dirs: Vec::new(),
                railpack_plan_path: None,
                railpack_info_path: None,
                buildkit_secret_files: Vec::new(),
                railpack_secret_cache_token: None,
            },
        )
        .expect("plan");

        assert_eq!(plan.image_build.command.program, "docker");
        assert_eq!(
            plan.image_build.command.args,
            ["build", "-t", "example:latest", "."]
        );
    }
}
