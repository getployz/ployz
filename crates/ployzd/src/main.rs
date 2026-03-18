mod cli;
mod cli_io;
mod request_builder;

use clap::Parser;
#[cfg(test)]
pub(crate) use cli::DebugTickTaskArg;
pub(crate) use cli::{
    Cli, CliError, Command, DebugAction, DeployAction, DeployCommand, DeployManifestArgs,
    DeployServiceArgs, InstallSourceArg, MachineAction, MachineInviteAction,
    MachineOperationAction, MeshAction, RuntimeTargetArg, ServiceModeArg,
};
use cli_io::{cmd_rpc_stdio, render_response, request_daemon};
#[cfg(test)]
use ployz_api::DaemonRequest;
#[cfg(test)]
use ployz_api::{
    DebugTickTask as ProtocolDebugTickTask, InstallRuntimeTarget as ApiInstallRuntimeTarget,
    InstallServiceMode as ApiInstallServiceMode,
};
use ployz_config::{RuntimeTarget, ServiceMode, load_client_config, load_daemon_config};
use ployz_sdk::UnixSocketTransport;
#[cfg(test)]
use ployz_types::spec::DeployManifest;
use ployzd::{BuiltInImages, HostPlatform, init_tracing, run_daemon, validate_runtime};
use request_builder::build_request;
#[cfg(test)]
use request_builder::{
    build_debug_request, build_machine_request, build_service_spec, upsert_service_in_manifest,
};
use std::process;

type Result<T> = std::result::Result<T, CliError>;

#[tokio::main]
async fn main() {
    match run().await {
        Ok(code) => {
            if code != 0 {
                process::exit(code);
            }
        }
        Err(err) => {
            err.print();
            process::exit(err.exit_code());
        }
    }
}

async fn run() -> Result<i32> {
    let cli = Cli::parse();

    match cli.command {
        Command::Run {
            runtime,
            service_mode,
            remote_control_port,
        } => {
            init_tracing();
            let platform = HostPlatform::detect();
            let cfg = load_daemon_config(
                cli.config,
                cli.data_dir,
                cli.socket,
                remote_control_port,
                &platform.paths_context(),
            )
            .map_err(|err| CliError::Config(err.to_string()))?;
            let runtime_target: RuntimeTarget = runtime.into();
            let service_mode: ServiceMode = service_mode.into();
            validate_runtime(runtime_target, service_mode, platform).map_err(CliError::Config)?;
            let built_in_images = BuiltInImages::load(cfg.builtin_images_manifest.as_deref())
                .map_err(CliError::Config)?;
            run_daemon(
                &cfg.data_dir,
                runtime_target,
                service_mode,
                &cfg.socket,
                built_in_images,
                cfg.cluster_cidr,
                cfg.subnet_prefix_len,
                cfg.remote_control_port,
                cfg.gateway_listen_addr,
                cfg.gateway_threads,
            )
            .await
            .map_err(CliError::Io)?;
            Ok(0)
        }
        other @ Command::Status
        | other @ Command::Doctor
        | other @ Command::Debug { .. }
        | other @ Command::Deploy(_)
        | other @ Command::Mesh { .. }
        | other @ Command::Machine { .. }
        | other @ Command::RpcStdio => {
            let platform = HostPlatform::detect();
            let resolved = load_client_config(cli.config, cli.socket, &platform.paths_context())
                .map_err(|err| CliError::Config(err.to_string()))?;
            let socket = resolved.socket;
            if let Command::RpcStdio = other {
                return cmd_rpc_stdio(&socket).await;
            }
            let transport = UnixSocketTransport::new(socket.clone());
            let request = build_request(other, &transport, &socket).await?;
            let response = request_daemon(&transport, &socket, request).await?;

            render_response(cli.json, cli.plain, cli.quiet, &response)?;
            if response.ok { Ok(0) } else { Ok(1) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parse_deploy_apply_primitives() {
        let cli = Cli::try_parse_from(["ployzd", "deploy", "--file", "manifest.json"])
            .expect("deploy apply args should parse");

        let Command::Deploy(command) = cli.command else {
            panic!("expected deploy command");
        };
        assert!(command.action.is_none());
        assert_eq!(command.manifest.file.as_deref(), Some("manifest.json"));
    }

    #[test]
    fn parse_deploy_preview_subcommand() {
        let cli = Cli::try_parse_from(["ployzd", "deploy", "preview", "--file", "-"])
            .expect("deploy preview args should parse");

        let Command::Deploy(command) = cli.command else {
            panic!("expected deploy command");
        };
        let Some(DeployAction::Preview(args)) = command.action else {
            panic!("expected deploy preview subcommand");
        };
        assert_eq!(args.file.as_deref(), Some("-"));
    }

    #[test]
    fn parse_deploy_service_subcommand() {
        let cli = Cli::try_parse_from([
            "ployzd",
            "deploy",
            "service",
            "api",
            "--namespace",
            "prod",
            "--image",
            "nginx:latest",
        ])
        .expect("deploy service args should parse");

        let Command::Deploy(command) = cli.command else {
            panic!("expected deploy command");
        };
        let Some(DeployAction::Service(args)) = command.action else {
            panic!("expected deploy service subcommand");
        };
        assert_eq!(args.name, "api");
        assert_eq!(args.namespace, "prod");
        assert_eq!(args.image, "nginx:latest");
    }

    #[test]
    fn upsert_service_replaces_existing_service_and_sorts() {
        let mut manifest = DeployManifest {
            namespace: ployz_types::spec::Namespace("prod".into()),
            services: vec![
                build_service_spec(
                    "redis:latest",
                    Some("cache"),
                    &[],
                    &[],
                    &[],
                    "overlay",
                    false,
                    "unless-stopped",
                    &[],
                ),
                build_service_spec(
                    "nginx:1",
                    Some("api"),
                    &[],
                    &[],
                    &[],
                    "overlay",
                    false,
                    "unless-stopped",
                    &[],
                ),
            ],
        };

        upsert_service_in_manifest(
            &mut manifest,
            build_service_spec(
                "nginx:2",
                Some("api"),
                &[],
                &[],
                &[],
                "overlay",
                false,
                "unless-stopped",
                &[],
            ),
        );

        let services: Vec<(&str, &str)> = manifest
            .services
            .iter()
            .map(|service| (service.name.as_str(), service.template.image.as_str()))
            .collect();
        assert_eq!(
            services,
            vec![("api", "nginx:2"), ("cache", "redis:latest")]
        );
    }

    #[test]
    fn build_machine_add_request_reads_identity_file_contents() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("ployz-machine-add-identity-{nanos}.key"));
        std::fs::write(&path, "test-private-key").expect("write identity");

        let request = build_machine_request(MachineAction::Add {
            identity: Some(path.clone()),
            runtime: Some(RuntimeTargetArg::Host),
            service_mode: Some(ServiceModeArg::User),
            install_source: Some(InstallSourceArg::Git),
            install_version: None,
            install_git_url: Some("https://example.invalid/ployz.git".into()),
            install_git_ref: Some("main".into()),
            targets: vec!["lab@example".into()],
        })
        .expect("machine add request");

        let DaemonRequest::MachineAdd { targets, options } = request else {
            panic!("expected machine add request");
        };
        assert_eq!(targets, vec!["lab@example"]);
        assert_eq!(
            options.ssh_identity_private_key.as_deref(),
            Some("test-private-key")
        );
        assert_eq!(
            options
                .install
                .as_ref()
                .and_then(|install| install.runtime_target),
            Some(ApiInstallRuntimeTarget::Host)
        );
        assert_eq!(
            options
                .install
                .as_ref()
                .and_then(|install| install.service_mode),
            Some(ApiInstallServiceMode::User)
        );

        std::fs::remove_file(path).expect("remove identity");
    }

    #[test]
    fn parse_doctor_command() {
        let cli = Cli::try_parse_from(["ployzd", "doctor"]).expect("doctor args should parse");

        let Command::Doctor = cli.command else {
            panic!("expected doctor command");
        };
    }

    #[test]
    fn build_debug_tick_request_defaults_to_all() {
        let request = build_debug_request(DebugAction::Tick {
            task: DebugTickTaskArg::All,
            repeat: 1,
        })
        .expect("debug tick request");

        let DaemonRequest::DebugTick { task, repeat } = request else {
            panic!("expected debug tick request");
        };
        assert_eq!(task, ProtocolDebugTickTask::All);
        assert_eq!(repeat, 1);
    }
}
