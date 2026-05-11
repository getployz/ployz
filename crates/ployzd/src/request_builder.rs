use crate::cli::{
    BranchResourceModeArg, BranchStatusArgs, BuildAction, BuildOperationAction, VolumeAction,
    VolumeZfsAction, VolumeZfsTransferAction,
};
use crate::cli_io::{read_optional_text_file, read_stdin_string, read_text_source, request_daemon};
use crate::{
    BranchAction, BranchApplyPreparedArgs, BranchNamespaceArgs, CliError, Command, DebugAction,
    DeployAction, DeployCommand, DeployManifestArgs, DeployServiceArgs, ImageAction,
    ImageOperationAction, InstallSourceArg, MachineAction, MachineInviteAction,
    MachineOperationAction, MachineStorageAction, MeshAction, MigrateAction, MigrateServiceArgs,
    Result, RuntimeTargetArg, ServiceModeArg,
};
use ployz_api::{
    BranchApplyPreparedRequest, BranchEnvironmentStatusRequest, BranchNamespaceMode,
    BranchNamespaceRequest, BranchResourceMode, BranchResourceModeOverride, BuildInputs,
    BuildLocalRequest, DaemonRequest, DeployOptions, ImageDistributeRequest, ImageInspectRequest,
    ImagePushRequest, ImageStatusRequest, InstallSource as MachineInstallSource, MachineAddOptions,
    MachineInstallOptions, MachineStoragePromoteRequest, MigrateServiceMode, MigrateServiceRequest,
};
use ployz_sdk::Transport;
use ployz_types::model::{DeployId, ImageDigest, ImagePlatform, MachineId, StorageReplicaPolicy};
use ployz_types::spec::{
    ContainerSpec, DeployManifest, Mount, MountSource, Namespace, NetworkMode, Placement,
    PortProtocol, PublishedPort, PullPolicy, Resources, RestartPolicy, RolloutStrategy,
    ServicePort, ServiceSpec, valid_storage_segment,
};
use std::collections::BTreeMap;

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
        Command::Migrate { action } => build_migrate_request(action),
        Command::Branch { action } => build_branch_request(action),
        Command::Runtime { .. } => Err(CliError::Usage(
            "internal error: runtime stream is handled directly".into(),
        )),
        Command::Mesh { action } => build_mesh_request(action),
        Command::Machine { action } => build_machine_request(action),
        Command::Image { action } => build_image_request(action),
        Command::Build { action } => build_build_request(action),
        Command::Volume { action } => build_volume_request(action),
        Command::RpcStdio => Err(CliError::Usage(
            "internal error: rpc-stdio is handled directly".into(),
        )),
        Command::Run { .. } => Err(CliError::Usage(
            "internal error: daemon command cannot be encoded as a daemon request".into(),
        )),
    }
}

pub(crate) fn build_build_request(action: BuildAction) -> Result<DaemonRequest> {
    match action {
        BuildAction::Local {
            method,
            image,
            platform,
            context,
        } => {
            let context_dir = std::fs::canonicalize(&context).map_err(|error| {
                CliError::Usage(format!(
                    "build context '{}' is not readable: {error}",
                    context.display()
                ))
            })?;
            Ok(DaemonRequest::BuildLocal {
                request: BuildLocalRequest {
                    method: method.into(),
                    context_dir: context_dir.display().to_string(),
                    image_name: image,
                    platform: platform.map(parse_image_platform).transpose()?,
                    push_target: None,
                    distribute_targets: Vec::new(),
                    inputs: BuildInputs::default(),
                },
            })
        }
        BuildAction::Operation { action } => match action {
            BuildOperationAction::List => Ok(DaemonRequest::BuildOperationList),
            BuildOperationAction::Get { id } => Ok(DaemonRequest::BuildOperationGet { id }),
        },
    }
}

pub(crate) fn build_image_request(action: ImageAction) -> Result<DaemonRequest> {
    match action {
        ImageAction::Status { digest, machine } => {
            let digest = digest
                .map(ImageDigest::try_new)
                .transpose()
                .map_err(CliError::Usage)?;
            Ok(DaemonRequest::ImageStatus {
                request: ImageStatusRequest {
                    digest,
                    machine_id: machine.map(MachineId::new),
                },
            })
        }
        ImageAction::Push {
            image,
            targets,
            platform,
            expected_digest,
        } => {
            let expected_digest = expected_digest
                .map(ImageDigest::try_new)
                .transpose()
                .map_err(CliError::Usage)?;
            Ok(DaemonRequest::ImagePush {
                request: ImagePushRequest {
                    source_image: image,
                    target_machines: targets.into_iter().map(MachineId::new).collect(),
                    platform: platform.map(parse_image_platform).transpose()?,
                    expected_digest,
                },
            })
        }
        ImageAction::Distribute {
            digest,
            source,
            targets,
            platform,
        } => {
            let digest = ImageDigest::try_new(digest).map_err(CliError::Usage)?;
            Ok(DaemonRequest::ImageDistribute {
                request: ImageDistributeRequest {
                    digest,
                    source_machine: MachineId::new(source),
                    target_machines: targets.into_iter().map(MachineId::new).collect(),
                    platform: platform.map(parse_image_platform).transpose()?,
                },
            })
        }
        ImageAction::Inspect {
            digest,
            reference,
            machine,
        } => {
            let digest = ImageDigest::try_new(digest).map_err(CliError::Usage)?;
            Ok(DaemonRequest::ImageInspect {
                request: ImageInspectRequest {
                    digest,
                    reference,
                    machines: machine.map(MachineId::new).into_iter().collect(),
                },
            })
        }
        ImageAction::Operation { action } => match action {
            ImageOperationAction::List => Ok(DaemonRequest::ImageOperationList),
            ImageOperationAction::Get { id } => Ok(DaemonRequest::ImageOperationGet { id }),
        },
    }
}

fn parse_image_platform(value: String) -> Result<ImagePlatform> {
    let parts = value.split('/').collect::<Vec<_>>();
    let (os, architecture, variant) = match parts.as_slice() {
        [os, architecture] => (*os, *architecture, None),
        [os, architecture, variant] if !variant.is_empty() => {
            (*os, *architecture, Some((*variant).to_string()))
        }
        _ => {
            return Err(CliError::Usage(format!(
                "image platform must use os/arch or os/arch/variant, got '{value}'"
            )));
        }
    };
    if os.is_empty() || architecture.is_empty() {
        return Err(CliError::Usage(format!(
            "image platform must include os and architecture, got '{value}'"
        )));
    }
    Ok(ImagePlatform {
        os: os.to_string(),
        architecture: architecture.to_string(),
        variant,
    })
}

fn build_volume_request(action: VolumeAction) -> Result<DaemonRequest> {
    match action {
        VolumeAction::Zfs { action } => build_volume_zfs_request(action),
    }
}

fn build_volume_zfs_request(action: VolumeZfsAction) -> Result<DaemonRequest> {
    match action {
        VolumeZfsAction::Inspect {
            namespace_volume,
            machine,
        } => {
            let (namespace, volume) = parse_namespace_volume(&namespace_volume)?;
            Ok(DaemonRequest::VolumeZfsInspect {
                namespace,
                volume,
                machine,
            })
        }
        VolumeZfsAction::Snapshot {
            namespace_volume,
            name,
        } => {
            let (namespace, volume) = parse_namespace_volume(&namespace_volume)?;
            Ok(DaemonRequest::VolumeZfsSnapshot {
                namespace,
                volume,
                snapshot: name,
            })
        }
        VolumeZfsAction::Send {
            namespace_volume,
            snapshot,
            to,
            from_snapshot,
        } => {
            let (namespace, volume) = parse_namespace_volume(&namespace_volume)?;
            Ok(DaemonRequest::VolumeZfsSend {
                namespace,
                volume,
                snapshot,
                target_machine: to,
                from_snapshot,
            })
        }
        VolumeZfsAction::Transfer { action } => match action {
            VolumeZfsTransferAction::Get { id } => Ok(DaemonRequest::VolumeZfsTransferGet { id }),
            VolumeZfsTransferAction::List => Ok(DaemonRequest::VolumeZfsTransferList),
        },
    }
}

fn parse_namespace_volume(value: &str) -> Result<(String, String)> {
    parse_scoped_name(value, "volume")
}

fn parse_scoped_name(value: &str, item_label: &str) -> Result<(String, String)> {
    let expected = format!("namespace/{item_label}");
    let Some((namespace, item)) = value.split_once('/') else {
        return Err(CliError::Usage(format!(
            "expected {expected}, got '{value}'"
        )));
    };
    if namespace.is_empty() || item.is_empty() || item.contains('/') {
        return Err(CliError::Usage(format!(
            "expected {expected}, got '{value}'"
        )));
    }
    Ok((namespace.to_string(), item.to_string()))
}

pub(crate) fn build_debug_request(action: DebugAction) -> Result<DaemonRequest> {
    match action {
        DebugAction::Tick { task, repeat } => Ok(DaemonRequest::DebugTick {
            task: task.into(),
            repeat,
        }),
    }
}

pub(crate) fn build_migrate_request(action: MigrateAction) -> Result<DaemonRequest> {
    match action {
        MigrateAction::Apply(args) => {
            build_migrate_service_request(args, MigrateServiceMode::Apply)
        }
        MigrateAction::Preview(args) => {
            build_migrate_service_request(args, MigrateServiceMode::Preview)
        }
        MigrateAction::RenderManifest(args) => {
            build_migrate_service_request(args, MigrateServiceMode::RenderManifest)
        }
    }
}

pub(crate) fn build_migrate_service_request(
    args: MigrateServiceArgs,
    mode: MigrateServiceMode,
) -> Result<DaemonRequest> {
    let (namespace, service) = parse_namespace_service(&args.service_ref)?;
    if args.to.trim().is_empty() {
        return Err(CliError::Usage("migrate --to cannot be empty".into()));
    }
    Ok(DaemonRequest::MigrateService {
        request: MigrateServiceRequest {
            namespace,
            service,
            target_machine: args.to,
            mode,
        },
    })
}

pub(crate) fn build_branch_request(action: BranchAction) -> Result<DaemonRequest> {
    match action {
        BranchAction::Apply(args) => {
            build_branch_namespace_request(args, BranchNamespaceMode::Apply)
        }
        BranchAction::ApplyPrepared(args) => build_branch_apply_prepared_request(args),
        BranchAction::List => Ok(DaemonRequest::BranchEnvironmentList),
        BranchAction::Prepare(args) => {
            build_branch_namespace_request(args, BranchNamespaceMode::Prepare)
        }
        BranchAction::Preview(args) => {
            build_branch_namespace_request(args, BranchNamespaceMode::Preview)
        }
        BranchAction::RenderManifest(args) => {
            build_branch_namespace_request(args, BranchNamespaceMode::RenderManifest)
        }
        BranchAction::Status(args) => build_branch_status_request(args),
    }
}

fn build_branch_apply_prepared_request(args: BranchApplyPreparedArgs) -> Result<DaemonRequest> {
    if args.prepared_deploy_id.trim().is_empty() {
        return Err(CliError::Usage(
            "branch apply-prepared requires a prepared deploy id".into(),
        ));
    }
    Ok(DaemonRequest::BranchApplyPrepared {
        request: BranchApplyPreparedRequest {
            prepared_deploy_id: DeployId::new(args.prepared_deploy_id),
        },
    })
}

fn build_branch_status_request(args: BranchStatusArgs) -> Result<DaemonRequest> {
    if !valid_storage_segment(&args.target_namespace) {
        return Err(CliError::Usage(
            "branch target namespace must be 1-63 chars of [a-z0-9_-], starting with a letter or digit".into(),
        ));
    }
    Ok(DaemonRequest::BranchEnvironmentStatus {
        request: BranchEnvironmentStatusRequest {
            target_namespace: args.target_namespace,
        },
    })
}

fn build_branch_namespace_request(
    args: BranchNamespaceArgs,
    mode: BranchNamespaceMode,
) -> Result<DaemonRequest> {
    if !valid_storage_segment(&args.source_namespace) {
        return Err(CliError::Usage(
            "branch source namespace must be 1-63 chars of [a-z0-9_-], starting with a letter or digit".into(),
        ));
    }
    if !valid_storage_segment(&args.target_namespace) {
        return Err(CliError::Usage(
            "branch target namespace must be 1-63 chars of [a-z0-9_-], starting with a letter or digit".into(),
        ));
    }
    Ok(DaemonRequest::BranchNamespace {
        request: BranchNamespaceRequest {
            source_namespace: args.source_namespace,
            target_namespace: args.target_namespace,
            mode,
            default_service_mode: branch_resource_mode(args.service_mode),
            default_volume_mode: branch_resource_mode(args.volume_mode),
            services: parse_branch_overrides(args.service, "service")?,
            volumes: parse_branch_overrides(args.volume, "volume")?,
        },
    })
}

fn branch_resource_mode(mode: BranchResourceModeArg) -> BranchResourceMode {
    match mode {
        BranchResourceModeArg::Fresh => BranchResourceMode::Fresh,
        BranchResourceModeArg::Branch => BranchResourceMode::Branch,
    }
}

fn parse_branch_overrides(
    values: Vec<String>,
    label: &'static str,
) -> Result<Vec<BranchResourceModeOverride>> {
    values
        .into_iter()
        .map(|value| {
            let Some((name, mode)) = value.split_once('=') else {
                return Err(CliError::Usage(format!(
                    "branch {label} override must be NAME=fresh or NAME=branch"
                )));
            };
            if !valid_storage_segment(name) {
                return Err(CliError::Usage(format!(
                    "branch {label} override name must be 1-63 chars of [a-z0-9_-], starting with a letter or digit"
                )));
            }
            let mode = match mode {
                "fresh" => BranchResourceMode::Fresh,
                "branch" => BranchResourceMode::Branch,
                _ => {
                    return Err(CliError::Usage(format!(
                        "branch {label} override mode must be fresh or branch"
                    )));
                }
            };
            Ok(BranchResourceModeOverride {
                name: name.to_string(),
                mode,
            })
        })
        .collect()
}

fn parse_namespace_service(value: &str) -> Result<(String, String)> {
    parse_scoped_name(value, "service")
}

async fn build_deploy_request<T: Transport>(
    command: DeployCommand,
    transport: &T,
    socket: &str,
) -> Result<DaemonRequest> {
    match command.action {
        Some(DeployAction::Preview(args)) => build_manifest_request(args, true),
        Some(DeployAction::Service(args)) => {
            build_deploy_service_request(args, transport, socket).await
        }
        None => build_manifest_request(command.manifest, false),
    }
}

fn build_manifest_request(args: DeployManifestArgs, force_preview: bool) -> Result<DaemonRequest> {
    let file = required_value(args.file, "deploy requires --file")?;
    let manifest_json = read_text_source("deploy manifest", &file)?;
    let options = DeployOptions::default();

    if force_preview || args.dry_run {
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
    if !args.volume.is_empty() && args.namespace != Namespace::system().as_str() {
        return Err(CliError::Usage(
            "deploy service -v creates a host bind mount and is only supported in the system namespace; declare managed volumes in a manifest for user workloads"
                .into(),
        ));
    }
    let mut manifest = export_namespace_manifest(transport, socket, &args.namespace).await?;
    let spec = build_service_spec(
        &args.image,
        Some(args.name.as_str()),
        &args.publish,
        &args.env,
        &args.volume,
        &args.network,
        args.pull,
        &args.restart,
        &args.command,
    );
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
    socket: &str,
    namespace: &str,
) -> Result<DeployManifest> {
    let response = request_daemon(
        transport,
        socket,
        DaemonRequest::DeployExport {
            namespace: namespace.to_string(),
        },
    )
    .await?;

    if !response.ok {
        return Err(CliError::Daemon {
            code: response.code,
            message: response.message,
        });
    }

    serde_json::from_str(&response.message).map_err(|error| {
        CliError::Serialize(format!(
            "failed to decode exported namespace manifest: {error}"
        ))
    })
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
        MeshAction::Ready { json } => Ok(DaemonRequest::MeshReady { json }),
        MeshAction::Create { network } => Ok(DaemonRequest::MeshCreate { network }),
        MeshAction::Init {
            network,
            name_stdin,
        } => Ok(DaemonRequest::MeshInit {
            network: string_arg_or_stdin("mesh init network", "--name-stdin", network, name_stdin)?,
        }),
        MeshAction::Start { network } => Ok(DaemonRequest::MeshStart { network }),
        MeshAction::Stop { force } => Ok(DaemonRequest::MeshStop { force }),
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
    }
}

pub(crate) fn build_machine_request(action: MachineAction) -> Result<DaemonRequest> {
    match action {
        MachineAction::Ls => Ok(DaemonRequest::MachineList),
        MachineAction::Rtt => Ok(DaemonRequest::MachineRtt),
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
        MachineAction::Update { version, ids } => Ok(DaemonRequest::MachineUpdate { ids, version }),
        MachineAction::Storage { action } => build_machine_storage_request(action),
        MachineAction::Activate { target } => Ok(DaemonRequest::MachineActivate { target }),
        MachineAction::Drain { target } => Ok(DaemonRequest::MachineDrain { target }),
        MachineAction::Standby { target, force } => {
            Ok(DaemonRequest::MachineStandby { target, force })
        }
        MachineAction::Rm { id, force } => Ok(DaemonRequest::MachineRemove { id, force }),
        MachineAction::Invite { action } => match action {
            MachineInviteAction::Create { ttl_secs } => {
                Ok(DaemonRequest::MachineInviteCreate { ttl_secs })
            }
            MachineInviteAction::List => Ok(DaemonRequest::MachineInviteList),
            MachineInviteAction::Revoke { invite_id } => {
                Ok(DaemonRequest::MachineInviteRevoke { invite_id })
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

fn build_machine_storage_request(action: MachineStorageAction) -> Result<DaemonRequest> {
    match action {
        MachineStorageAction::Promote { replicas, targets } => {
            let replicas =
                StorageReplicaPolicy::try_from_replicas(replicas).map_err(CliError::Usage)?;
            if replicas == StorageReplicaPolicy::Single {
                return Err(CliError::Usage(
                    "machine storage promote requires --replicas 3 or --replicas 5".into(),
                ));
            }
            Ok(DaemonRequest::MachineStoragePromote {
                request: MachineStoragePromoteRequest { targets, replicas },
            })
        }
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_service_spec(
    image: &str,
    name: Option<&str>,
    publish: &[String],
    env: &[String],
    volume: &[String],
    network: &str,
    pull: bool,
    restart: &str,
    command: &[String],
) -> ServiceSpec {
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

    let mounts: Vec<Mount> = volume
        .iter()
        .filter_map(|mapping| {
            let parts: Vec<&str> = mapping.splitn(3, ':').collect();
            match parts.as_slice() {
                [src, dst] => Some(Mount {
                    source: MountSource::Bind(src.to_string()),
                    target: dst.to_string(),
                    readonly: false,
                }),
                [src, dst, opts] => Some(Mount {
                    source: MountSource::Bind(src.to_string()),
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
        placement: Placement::replicated(1),
        template: ContainerSpec {
            image: image.to_string(),
            command: if command.is_empty() {
                None
            } else {
                Some(command.to_vec())
            },
            entrypoint: None,
            env: env_map,
            mounts,
            cap_add: vec![],
            cap_drop: vec![],
            privileged: false,
            user: None,
            stop_grace_period: None,
            pid_mode: None,
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
        restart: match restart {
            "always" => RestartPolicy::Always,
            "on-failure" => RestartPolicy::OnFailure,
            "no" => RestartPolicy::No,
            "unless-stopped" => RestartPolicy::UnlessStopped,
            _ => RestartPolicy::UnlessStopped,
        },
    }
}
