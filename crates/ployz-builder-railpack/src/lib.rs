use ployz_build_api::{
    BuildCommand, BuildCommandPaths, BuildCommandPlan, BuildCommandStep, BuildCommandStepKind,
    BuildInvocationPlan, DOCKER_BUILDKIT_ENV,
};
use ployz_model::BuildLocalRequest;

const RAILPACK_FRONTEND: &str = "ghcr.io/railwayapp/railpack-frontend";
const RAILPACK_SECRET_PLACEHOLDER: &str = "__PLOYZ_BUILDKIT_SECRET__";

pub fn command_plan(
    request: &BuildLocalRequest,
    invocation: &BuildInvocationPlan,
    paths: BuildCommandPaths,
) -> Result<BuildCommandPlan, String> {
    let Some(plan_path) = paths.railpack_plan_path.as_ref() else {
        return Err("railpack command plan missing plan output path".into());
    };
    let Some(info_path) = paths.railpack_info_path.as_ref() else {
        return Err("railpack command plan missing info output path".into());
    };

    let mut prepare_args = vec![
        "prepare".into(),
        "--plan-out".into(),
        plan_path.display().to_string(),
        "--info-out".into(),
        info_path.display().to_string(),
    ];
    for (key, value) in &invocation.plain_env {
        prepare_args.push("--env".into());
        prepare_args.push(format!("{key}={value}"));
    }
    for (key, _value) in &invocation.secret_env {
        prepare_args.push("--env".into());
        prepare_args.push(format!("{key}={RAILPACK_SECRET_PLACEHOLDER}"));
    }
    prepare_args.push(".".into());

    let mut build_args = vec![
        "buildx".into(),
        "build".into(),
        "-t".into(),
        request.image_name.clone(),
        "--build-arg".into(),
        format!("BUILDKIT_SYNTAX={RAILPACK_FRONTEND}"),
        "-f".into(),
        plan_path.display().to_string(),
    ];
    if let Some(platform) = &request.platform {
        build_args.push("--platform".into());
        build_args.push(format_platform(platform));
    }
    build_args.push("--load".into());
    for (key, value) in &invocation.plain_env {
        build_args.push("--build-arg".into());
        build_args.push(format!("{key}={value}"));
    }
    for (key, path) in &paths.buildkit_secret_files {
        build_args.push("--secret".into());
        build_args.push(format!("id={key},src={}", path.display()));
    }
    if let Some(token) = &paths.railpack_secret_cache_token {
        build_args.push("--build-arg".into());
        build_args.push(format!("secrets-hash={token}"));
    }
    build_args.push(".".into());

    let docker_env = vec![(DOCKER_BUILDKIT_ENV.into(), "1".into())];
    let redaction_values = command_redaction_values(invocation, &paths);
    Ok(BuildCommandPlan::new(
        vec![BuildCommandStep {
            kind: BuildCommandStepKind::RailpackPrepare,
            command: BuildCommand {
                program: "railpack",
                args: prepare_args,
                env: Vec::new(),
                redaction_values: redaction_values.clone(),
            },
        }],
        BuildCommandStep {
            kind: BuildCommandStepKind::ImageBuild,
            command: BuildCommand {
                program: "docker",
                args: build_args,
                env: docker_env,
                redaction_values,
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
    fn railpack_plan_has_prepare_and_docker_build_steps() {
        let request = BuildLocalRequest {
            method: BuildMethod::Railpack,
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
                railpack_plan_path: Some("railpack-plan.json".into()),
                railpack_info_path: Some("railpack-info.json".into()),
                buildkit_secret_files: Vec::new(),
                railpack_secret_cache_token: None,
            },
        )
        .expect("plan");

        assert_eq!(plan.pre_build_steps[0].command.program, "railpack");
        assert_eq!(plan.image_build.command.program, "docker");
    }
}
