use crate::cli_io::{read_optional_text_file, read_stdin_string, read_text_source};
use crate::{
    CliError, Command, DebugAction, DeployAction, DeployCommand, DeployManifestArgs,
    DeployServiceArgs, InstallSourceArg, MachineAction, MachineInviteAction,
    MachineOperationAction, MeshAction, Result, RuntimeTargetArg, ServiceModeArg,
};
use ployz_api::{
    BootstrapWaitMode, DaemonRequest, DeployOptions, InstallSource as MachineInstallSource,
    MachineAddOptions, MachineInstallOptions, MachineRemoveMode, MeshReadyOutput,
};
use ployz_sdk::{DaemonClient, Transport};
use ployz_types::spec::{
    ContainerSpec, DeployManifest, NetworkMode, Placement, PortProtocol, PublishedPort, PullPolicy,
    Resources, RestartPolicy, RolloutStrategy, ServicePort, ServiceSpec, VolumeMount, VolumeSource,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestRequestMode {
    Preview,
    Apply,
}

pub(crate) struct ServiceSpecArgs<'a> {
    pub(crate) image: &'a str,
    pub(crate) name: Option<&'a str>,
    pub(crate) publish: &'a [String],
    pub(crate) env: &'a [String],
    pub(crate) volume: &'a [String],
    pub(crate) network: &'a str,
    pub(crate) pull: bool,
    pub(crate) restart: &'a str,
    pub(crate) command: &'a [String],
}

pub(crate) async fn build_request<T: Transport>(
    command: Command,
    transport: &T,
    socket: &str,
) -> Result<DaemonRequest> {
    match command {
        Command::Status => Ok(DaemonRequest::Status),
        Command::Doctor => Ok(DaemonRequest::Doctor),
        Command::Debug { action } => build_debug_request(action),
        Command::Deploy(command) => build_deploy_request(*command, transport, socket).await,
        Command::Mesh { action } => build_mesh_request(action),
        Command::Machine { action } => build_machine_request(action),
        Command::RpcStdio => Err(CliError::Usage(
            "internal error: rpc-stdio is handled directly".into(),
        )),
        Command::Run { .. } => Err(CliError::Usage(
            "internal error: daemon command cannot be encoded as a daemon request".into(),
        )),
    }
}

pub(crate) fn build_debug_request(action: DebugAction) -> Result<DaemonRequest> {
    match action {
        DebugAction::Tick { task, repeat } => Ok(DaemonRequest::DebugTick {
            task: task.into(),
            repeat,
        }),
    }
}

async fn build_deploy_request<T: Transport>(
    command: DeployCommand,
    transport: &T,
    socket: &str,
) -> Result<DaemonRequest> {
    match command.action {
        Some(DeployAction::Preview(args)) => {
            build_manifest_request(args, ManifestRequestMode::Preview)
        }
        Some(DeployAction::Service(args)) => {
            build_deploy_service_request(args, transport, socket).await
        }
        None => build_manifest_request(command.manifest, ManifestRequestMode::Apply),
    }
}

fn build_manifest_request(
    args: DeployManifestArgs,
    mode: ManifestRequestMode,
) -> Result<DaemonRequest> {
    let file = required_value(args.file, "deploy requires --file")?;
    let manifest_json = read_text_source("deploy manifest", &file)?;
    let options = DeployOptions::default();

    if mode == ManifestRequestMode::Preview || args.dry_run {
        Ok(DaemonRequest::DeployPreview {
            manifest_json,
            options,
        })
    } else {
        Ok(DaemonRequest::DeployApply {
            manifest_json,
            options,
        })
    }
}

async fn build_deploy_service_request<T: Transport>(
    args: DeployServiceArgs,
    transport: &T,
    socket: &str,
) -> Result<DaemonRequest> {
    let mut manifest = export_namespace_manifest(transport, socket, &args.namespace).await?;
    let spec = build_service_spec(ServiceSpecArgs {
        image: &args.image,
        name: Some(args.name.as_str()),
        publish: &args.publish,
        env: &args.env,
        volume: &args.volume,
        network: &args.network,
        pull: args.pull,
        restart: &args.restart,
        command: &args.command,
    });
    upsert_service_in_manifest(&mut manifest, spec);
    let manifest_json = encode_manifest_json(&manifest)?;
    let options = DeployOptions::default();

    if args.dry_run {
        Ok(DaemonRequest::DeployPreview {
            manifest_json,
            options,
        })
    } else {
        Ok(DaemonRequest::DeployApply {
            manifest_json,
            options,
        })
    }
}

async fn export_namespace_manifest<T: Transport>(
    transport: &T,
    _socket: &str,
    namespace: &str,
) -> Result<DeployManifest> {
    DaemonClient::new(transport)
        .deploy_export_manifest(namespace)
        .await
        .map_err(|error| CliError::Serialize(format!("failed to export namespace manifest: {error}")))
}

pub(crate) fn upsert_service_in_manifest(manifest: &mut DeployManifest, spec: ServiceSpec) {
    match manifest
        .services
        .iter()
        .position(|existing| existing.name == spec.name)
    {
        Some(index) => {
            let Some(slot) = manifest.services.get_mut(index) else {
                return;
            };
            *slot = spec;
        }
        None => manifest.services.push(spec),
    }
    manifest
        .services
        .sort_by(|left, right| left.name.cmp(&right.name));
}

fn build_mesh_request(action: MeshAction) -> Result<DaemonRequest> {
    match action {
        MeshAction::List => Ok(DaemonRequest::MeshList),
        MeshAction::Status { network } => Ok(DaemonRequest::MeshStatus { network }),
        MeshAction::Join { token, token_stdin } => Ok(DaemonRequest::MeshJoin {
            token: string_arg_or_stdin("mesh join token", "--token-stdin", token, token_stdin)?,
        }),
        MeshAction::Ready { json } => Ok(DaemonRequest::MeshReady {
            output: if json {
                MeshReadyOutput::Json
            } else {
                MeshReadyOutput::Text
            },
        }),
        MeshAction::Create { network } => Ok(DaemonRequest::MeshCreate { network }),
        MeshAction::Init {
            network,
            name_stdin,
        } => Ok(DaemonRequest::MeshInit {
            network: string_arg_or_stdin("mesh init network", "--name-stdin", network, name_stdin)?,
        }),
        MeshAction::Up {
            network,
            skip_bootstrap_wait,
        } => Ok(DaemonRequest::MeshUp {
            network,
            bootstrap_wait: if skip_bootstrap_wait {
                BootstrapWaitMode::Skip
            } else {
                BootstrapWaitMode::Wait
            },
        }),
        MeshAction::Down => Ok(DaemonRequest::MeshDown),
        MeshAction::Destroy {
            network,
            name_stdin,
        } => Ok(DaemonRequest::MeshDestroy {
            network: string_arg_or_stdin(
                "mesh destroy network",
                "--name-stdin",
                network,
                name_stdin,
            )?,
        }),
        MeshAction::SelfRecord => Ok(DaemonRequest::MeshSelfRecord),
        MeshAction::Accept { response } => Ok(DaemonRequest::MeshAccept { response }),
    }
}

pub(crate) fn build_machine_request(action: MachineAction) -> Result<DaemonRequest> {
    match action {
        MachineAction::Ls => Ok(DaemonRequest::MachineList),
        MachineAction::Init {
            target,
            network,
            runtime,
            service_mode,
            install_source,
            install_version,
            install_git_url,
            install_git_ref,
        } => Ok(DaemonRequest::MachineInit {
            target,
            network,
            install: build_machine_install_options(
                runtime,
                service_mode,
                install_source,
                install_version,
                install_git_url,
                install_git_ref,
            ),
        }),
        MachineAction::Add {
            identity,
            runtime,
            service_mode,
            install_source,
            install_version,
            install_git_url,
            install_git_ref,
            targets,
        } => {
            let install = build_machine_install_options(
                runtime,
                service_mode,
                install_source,
                install_version,
                install_git_url,
                install_git_ref,
            );
            let options = MachineAddOptions {
                ssh_identity_private_key: read_optional_text_file(
                    "machine add identity",
                    identity.as_deref(),
                )?,
                install: if install == MachineInstallOptions::default() {
                    None
                } else {
                    Some(install)
                },
            };
            Ok(DaemonRequest::MachineAdd { targets, options })
        }
        MachineAction::Rm { id, force } => Ok(DaemonRequest::MachineRemove {
            id,
            mode: if force {
                MachineRemoveMode::Force
            } else {
                MachineRemoveMode::DisabledOnly
            },
        }),
        MachineAction::Invite { action } => match action {
            MachineInviteAction::Create { ttl_secs } => {
                Ok(DaemonRequest::MachineInviteCreate { ttl_secs })
            }
            MachineInviteAction::Import { token } => {
                Ok(DaemonRequest::MachineInviteImport { token })
            }
        },
        MachineAction::Operation { action } => match action {
            MachineOperationAction::List => Ok(DaemonRequest::MachineOperationList),
            MachineOperationAction::Get { id } => Ok(DaemonRequest::MachineOperationGet { id }),
        },
    }
}

fn build_machine_install_options(
    runtime: Option<RuntimeTargetArg>,
    service_mode: Option<ServiceModeArg>,
    install_source: Option<InstallSourceArg>,
    install_version: Option<String>,
    install_git_url: Option<String>,
    install_git_ref: Option<String>,
) -> MachineInstallOptions {
    let has_version = install_version.is_some();
    let has_git = install_git_url.is_some() || install_git_ref.is_some();
    let resolved_source = match install_source {
        Some(source) => Some(source.into()),
        None if has_version && !has_git => Some(MachineInstallSource::Release),
        None if !has_version && has_git => Some(MachineInstallSource::Git),
        None => None,
    };
    MachineInstallOptions {
        runtime_target: runtime.map(Into::into),
        service_mode: service_mode.map(Into::into),
        source: resolved_source,
        version: install_version,
        git_url: install_git_url,
        git_ref: install_git_ref,
    }
}

fn required_value<T>(value: Option<T>, message: &str) -> Result<T> {
    match value {
        Some(value) => Ok(value),
        None => Err(CliError::Usage(message.to_string())),
    }
}

fn string_arg_or_stdin(
    label: &str,
    stdin_flag: &str,
    value: Option<String>,
    read_stdin: bool,
) -> Result<String> {
    match (value, read_stdin) {
        (Some(text), false) => Ok(text),
        (None, true) => read_stdin_string(label),
        (None, false) => Err(CliError::Usage(format!(
            "{label} requires either an argument or {stdin_flag}"
        ))),
        (Some(_), true) => Err(CliError::Usage(format!(
            "{label} cannot use both an argument and {stdin_flag}"
        ))),
    }
}

fn encode_manifest_json(manifest: &DeployManifest) -> Result<String> {
    serde_json::to_string(manifest).map_err(|error| {
        CliError::Serialize(format!("failed to serialize deploy manifest: {error}"))
    })
}

pub(crate) fn build_service_spec(args: ServiceSpecArgs<'_>) -> ServiceSpec {
    let ServiceSpecArgs {
        image,
        name,
        publish,
        env,
        volume,
        network,
        pull,
        restart,
        command,
    } = args;

    let service_name = match name {
        Some(name) => name.to_string(),
        None => {
            let image_tail = image.rsplit('/').next().unwrap_or(image);
            image_tail
                .split(':')
                .next()
                .unwrap_or(image_tail)
                .to_string()
        }
    };

    let mut service_ports = Vec::new();
    let mut published_ports = Vec::new();
    for (index, mapping) in publish.iter().enumerate() {
        let parts: Vec<&str> = mapping.split(':').collect();
        let [host, container] = parts.as_slice() else {
            eprintln!("warning: ignoring invalid port mapping: {mapping}");
            continue;
        };
        let service_port = format!("port-{:04}", index + 1);
        let Some(host_port) = host.parse().ok() else {
            eprintln!("warning: ignoring invalid host port in mapping: {mapping}");
            continue;
        };
        let Some(container_port) = container.parse().ok() else {
            eprintln!("warning: ignoring invalid container port in mapping: {mapping}");
            continue;
        };
        service_ports.push(ServicePort {
            name: service_port.clone(),
            container_port,
            protocol: PortProtocol::Tcp,
        });
        published_ports.push(PublishedPort {
            service_port,
            host_port,
            host_ip: None,
        });
    }

    let env_map: BTreeMap<String, String> = env
        .iter()
        .filter_map(|entry| {
            let (key, value) = entry.split_once('=')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect();

    let volumes: Vec<VolumeMount> = volume
        .iter()
        .filter_map(|mapping| {
            let parts: Vec<&str> = mapping.splitn(3, ':').collect();
            match parts.as_slice() {
                [src, dst] => Some(VolumeMount {
                    source: VolumeSource::Bind(src.to_string()),
                    target: dst.to_string(),
                    readonly: false,
                }),
                [src, dst, opts] => Some(VolumeMount {
                    source: VolumeSource::Bind(src.to_string()),
                    target: dst.to_string(),
                    readonly: *opts == "ro",
                }),
                _ => {
                    eprintln!("warning: ignoring invalid volume: {mapping}");
                    None
                }
            }
        })
        .collect();

    let network_mode = match network {
        "host" => NetworkMode::Host,
        "none" => NetworkMode::None,
        "overlay" => NetworkMode::Overlay,
        other => NetworkMode::Service(other.to_string()),
    };

    ServiceSpec {
        name: service_name,
        placement: Placement::Replicated { count: 1 },
        template: ContainerSpec {
            image: image.to_string(),
            command: if command.is_empty() {
                None
            } else {
                Some(command.to_vec())
            },
            entrypoint: None,
            env: env_map,
            volumes,
            cap_add: vec![],
            cap_drop: vec![],
            privileged: false,
            user: None,
            pull_policy: if pull {
                PullPolicy::Always
            } else {
                PullPolicy::IfNotPresent
            },
            resources: Resources::empty(),
            sysctls: BTreeMap::new(),
        },
        network: network_mode,
        service_ports,
        publish: published_ports,
        routes: vec![],
        readiness: None,
        rollout: RolloutStrategy::Recreate,
        labels: BTreeMap::new(),
        stop_grace_period: None,
        restart: parse_restart_policy(restart),
    }
}

fn parse_restart_policy(restart: &str) -> RestartPolicy {
    match restart {
        "always" => RestartPolicy::Always,
        "on-failure" => RestartPolicy::OnFailure,
        "no" => RestartPolicy::No,
        "unless-stopped" => RestartPolicy::UnlessStopped,
        value => {
            eprintln!("warning: unknown restart policy '{value}', defaulting to unless-stopped");
            RestartPolicy::UnlessStopped
        }
    }
}
