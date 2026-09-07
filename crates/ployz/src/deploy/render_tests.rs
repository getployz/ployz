use std::num::NonZeroU64;

use ployz_core::{
    ContainerId, DeployOperation, DockerVolumeId, DockerVolumeName, ExecutionError,
    FailedOperation, HealthFailure, HookFailure, MachineAction, MachineId, MachineName,
    OperationPhase, OperationRow, OperationStatus, PreservedVolume, ProjectName,
    ProvisionedVolumeMaximumBytes, PruneRefusal, QualifiedService, ReplacementCompensation,
    ReplacementOperation, RequestedServiceSpec, ResolvedServiceSpec, RestartAttempt, RpcError,
    RpcErrorCode, ServiceName, StopAttempt, UpdateOrder, VolumeToCreate,
};

use super::super::report::{self, Ink, Role};

use super::*;

#[test]
fn empty_preview_prints_no_changes_without_a_prompt_body() {
    let preview = DeployPreview::new(Vec::new(), Vec::new(), ProjectName::parse("app").unwrap());
    assert_eq!(plan_text(&preview, "default", None), "No changes.\n");
    assert_eq!(
        confirm_prompt("default"),
        "Proceed with deployment to default? [y/N] "
    );
}

#[test]
fn removal_plan_lists_container_and_volume_removes() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let container_id = ContainerId::parse("f".repeat(64)).unwrap();
    let volume_id = DockerVolumeId {
        machine_id,
        name: DockerVolumeName::parse("shop_data").unwrap(),
    };
    let rows = vec![
        OperationRow::pending(
            0,
            DeployOperation::RemoveContainer {
                machine_id,
                container_id,
            },
            Some(MachineName::parse("edge").unwrap()),
            Some("web-1".into()),
            Some(ServiceName::parse("web").unwrap()),
        ),
        OperationRow::pending(
            1,
            DeployOperation::RemoveVolume {
                id: volume_id.clone(),
            },
            Some(MachineName::parse("edge").unwrap()),
            None,
            None,
        ),
    ];
    let mut preview = DeployPreview::new(rows, Vec::new(), ProjectName::parse("shop").unwrap());
    preview.would_remove = vec![QualifiedService::parse("shop/web").unwrap()];
    let text = removal_plan_text(&preview, "default");
    assert!(text.starts_with("Removal plan\n"), "{text}");
    assert!(text.contains("- remove container web-1 on edge"), "{text}");
    assert!(text.contains("- remove volume shop_data on edge"), "{text}");
    preview.operations.clear();
    preview.preserved_volumes = vec![PreservedVolume {
        id: volume_id,
        machine_name: Some(MachineName::parse("edge").unwrap()),
    }];
    let preserved = removal_plan_text(&preview, "default");
    assert!(
        preserved.contains("would preserve volume shop_data on edge"),
        "{preserved}"
    );
}

#[test]
fn service_tree_keeps_volume_and_service_ordering() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let machine = Some(MachineName::parse("edge").unwrap());
    let container = |index, service: &str, display: &str, id: char| {
        OperationRow::pending(
            index,
            DeployOperation::StopContainer {
                machine_id,
                container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
                purpose: ployz_core::StopContainerPurpose::Lifecycle,
            },
            machine.clone(),
            Some(display.into()),
            Some(ServiceName::parse(service).unwrap()),
        )
    };
    let volume = |index, name: &str| {
        OperationRow::pending(
            index,
            DeployOperation::RemoveVolume {
                id: DockerVolumeId {
                    machine_id,
                    name: DockerVolumeName::parse(name).unwrap(),
                },
            },
            machine.clone(),
            None,
            None,
        )
    };
    let rows = vec![
        container(0, "web", "web-old", '1'),
        volume(1, "z"),
        container(2, "api", "api-old", '2'),
        volume(3, "a"),
        container(4, "web", "web-new", '3'),
    ];
    let preview = DeployPreview::new(rows, Vec::new(), ProjectName::parse("app").unwrap());

    assert_eq!(
        service_trees(&preview),
        concat!(
            "- remove volume z on edge\n",
            "- remove volume a on edge\n",
            "~ update service api\n",
            "  ╰── - stop container api-old on edge\n",
            "~ update service web\n",
            "  ├── - stop container web-old on edge\n",
            "  ╰── - stop container web-new on edge\n",
        )
    );
}

#[test]
fn plan_identifies_a_provisioned_volume_and_its_bound() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let mut preview =
        DeployPreview::new(Vec::new(), Vec::new(), ProjectName::parse("shop").unwrap());
    preview.volumes_to_create = vec![VolumeToCreate {
        machine_id,
        machine_name: Some(MachineName::parse("edge").unwrap()),
        name: DockerVolumeName::parse("data").unwrap(),
        maximum_bytes: Some(ProvisionedVolumeMaximumBytes::new(
            NonZeroU64::new(1_073_741_824).unwrap(),
        )),
    }];

    let text = plan_text(&preview, "default", None);

    assert!(text.contains("Volumes to create\n"), "{text}");
    assert!(
        text.contains("+ provisioned volume data (maximum 1073741824 bytes) on edge"),
        "{text}"
    );
}

#[test]
fn replace_plan_matches_tree_shape() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let old = ContainerId::parse("f".repeat(64)).unwrap();
    let spec = resolved("excalidraw", "excalidraw/excalidraw:latest");
    let row = OperationRow::pending(
        0,
        DeployOperation::ReplaceContainer(ReplacementOperation {
            machine_id,
            old_container_id: old,
            spec,
            skip_health_monitor: false,
        }),
        Some(MachineName::parse("machine-dc3c").unwrap()),
        Some("excalidraw/fde7ac7f11ad".into()),
        Some(ServiceName::parse("excalidraw").unwrap()),
    );
    let preview = DeployPreview::new(vec![row], Vec::new(), ProjectName::parse("app").unwrap());
    let text = plan_text(&preview, "default", None);
    assert!(text.contains("Deployment plan\ncontext: default\nproject: app\n"));
    assert!(text.contains("~ update service excalidraw\n"));
    assert!(text.contains("  │   image: excalidraw/excalidraw:latest\n"));
    assert!(text.contains("  ╰── +/- replace container excalidraw/fde7ac7f11ad on machine-dc3c\n"));
    assert!(text.contains("1 replace (start-first) · across 1 machine\n"));
}

#[test]
fn plan_shows_dependency_health_wait() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let row = OperationRow::pending(
        0,
        DeployOperation::WaitHealthy {
            machine_id,
            dependent: QualifiedService::parse("app/web").unwrap(),
            dependency: QualifiedService::parse("app/db").unwrap(),
        },
        Some(MachineName::parse("edge").unwrap()),
        None,
        Some(ServiceName::parse("web").unwrap()),
    );
    let preview = DeployPreview::new(vec![row], Vec::new(), ProjectName::parse("app").unwrap());

    assert!(
        plan_text(&preview, "default", None)
            .contains("~ wait for app/db to be healthy before app/web")
    );
}

#[test]
fn plan_lists_would_remove_with_observer_relative_refusal() {
    let mut preview =
        DeployPreview::new(Vec::new(), Vec::new(), ProjectName::parse("shop").unwrap());
    preview.would_remove = vec![QualifiedService::parse("shop/debug").unwrap()];
    preview.prune_refusal = Some(PruneRefusal::IncompleteSnapshot);
    let text = plan_text(&preview, "default", Some("top-level Compose name"));
    assert!(
        text.contains("project: shop (top-level Compose name)"),
        "{text}"
    );
    assert!(text.contains("would remove shop/debug"), "{text}");
    assert!(
        text.contains("incomplete relative to this Machine's current visible fan-out"),
        "{text}"
    );
    assert!(
        !text.to_ascii_lowercase().contains("cluster completeness")
            || text.contains("not Cluster completeness"),
        "{text}"
    );
    assert!(!text.contains("authoritative"));
    assert!(!text.contains("No changes."));
}

#[test]
fn plan_lists_preserved_volumes_instead_of_no_changes() {
    let mut preview =
        DeployPreview::new(Vec::new(), Vec::new(), ProjectName::parse("shop").unwrap());
    preview.preserved_volumes = vec![ployz_core::PreservedVolume {
        id: ployz_core::DockerVolumeId {
            machine_id: MachineId::parse("d".repeat(32)).unwrap(),
            name: ployz_core::DockerVolumeName::parse("shop_data").unwrap(),
        },
        machine_name: Some(MachineName::parse("edge").unwrap()),
    }];
    let text = plan_text(&preview, "default", None);
    assert!(
        text.contains("would preserve volume shop_data on edge"),
        "{text}"
    );
    assert!(!text.contains("No changes."));
}

#[test]
fn plan_shows_prune_as_remove_operations_before_confirm() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let row = OperationRow::pending(
        0,
        DeployOperation::RemoveContainer {
            machine_id,
            container_id: ContainerId::parse("f".repeat(64)).unwrap(),
        },
        Some(MachineName::parse("machine-dc3c").unwrap()),
        Some("debug/fde7ac7f11ad".into()),
        Some(ServiceName::parse("debug").unwrap()),
    );
    let mut preview =
        DeployPreview::new(vec![row], Vec::new(), ProjectName::parse("shop").unwrap());
    preview.would_remove = vec![QualifiedService::parse("shop/debug").unwrap()];
    let text = plan_text(&preview, "default", None);
    assert!(text.contains("- remove service debug\n"), "{text}");
    assert!(
        text.contains("- remove container debug/fde7ac7f11ad on machine-dc3c"),
        "{text}"
    );
    assert!(text.contains("1 remove · across 1 machine"), "{text}");
    assert!(!text.contains("~ update service debug"), "{text}");
    assert!(!text.contains("would remove"), "{text}");
    assert!(!text.contains("will not remove"), "{text}");
    assert_eq!(
        confirm_prompt("default"),
        "Proceed with deployment to default? [y/N] "
    );
    assert!(!preview.noop());
}

#[test]
fn replica_shrink_still_prints_update_not_service_remove() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let row = OperationRow::pending(
        0,
        DeployOperation::RemoveContainer {
            machine_id,
            container_id: ContainerId::parse("f".repeat(64)).unwrap(),
        },
        Some(MachineName::parse("machine-dc3c").unwrap()),
        Some("web/fde7ac7f11ad".into()),
        Some(ServiceName::parse("web").unwrap()),
    );
    let preview = DeployPreview::new(vec![row], Vec::new(), ProjectName::parse("shop").unwrap());
    let text = plan_text(&preview, "default", None);
    assert!(text.contains("~ update service web\n"), "{text}");
    assert!(
        text.contains("- remove container web/fde7ac7f11ad on machine-dc3c"),
        "{text}"
    );
    assert!(!text.contains("- remove service web"), "{text}");
}

#[test]
fn progress_snapshot_prints_healthy_elapsed_and_removed() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let machine = Some(MachineName::parse("machine-dc3c").unwrap());
    let spec = resolved("excalidraw", "excalidraw/excalidraw:latest");
    let healthy = OperationRow {
        index: 0,
        machine_id,
        machine_name: machine.clone(),
        operation: DeployOperation::RunContainer {
            machine_id,
            spec,
            skip_health_monitor: false,
        },
        display_name: Some("excalidraw-0z12".into()),
        service_name: Some(ServiceName::parse("excalidraw").unwrap()),
        status: OperationStatus::Running {
            phase: OperationPhase::WaitingForHealth {
                container_id: ContainerId::parse("a".repeat(64)).unwrap(),
                health: None,
                elapsed_ms: 30_600,
                deadline_ms: 60_000,
            },
        },
    };
    let removed = OperationRow {
        index: 1,
        machine_id,
        machine_name: machine,
        operation: DeployOperation::RemoveContainer {
            machine_id,
            container_id: ContainerId::parse("f".repeat(64)).unwrap(),
        },
        display_name: Some("excalidraw/fde7ac7f11ad".into()),
        service_name: Some(ServiceName::parse("excalidraw").unwrap()),
        status: OperationStatus::Completed,
    };
    let event = DeployEvent::Progress {
        completed: 2,
        total: 2,
        rows: vec![healthy, removed],
    };
    let text = progress_text(&event, "Deploying to default");
    assert!(text.contains("[+] Deploying to default 2/2\n"));
    assert!(text.contains("Container excalidraw-0z12 on machine-dc3c"));
    assert!(text.contains("waiting for health"));
    assert!(!text.contains("Healthy"));
    assert!(text.contains("30.6s"));
    assert!(text.contains("Container excalidraw/fde7ac7f11ad on machine-dc3c"));
    assert!(text.contains("Removed"));
}

#[test]
fn volume_live_row_is_volume_not_container() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let row = OperationRow::pending(
        0,
        DeployOperation::RemoveVolume {
            id: DockerVolumeId {
                machine_id,
                name: DockerVolumeName::parse("cashdash_data").unwrap(),
            },
        },
        Some(MachineName::parse("vultr").unwrap()),
        None,
        None,
    );
    let event = DeployEvent::Progress {
        completed: 0,
        total: 3,
        rows: vec![row],
    };
    let text = progress_text(&event, "Deploying to default");
    assert!(text.contains("Volume cashdash_data on vultr"), "{text}");
    assert!(!text.contains("Container cashdash_data"), "{text}");
}

#[test]
fn run_style_titles_the_task_list_for_an_ad_hoc_service() {
    let event = DeployEvent::Progress {
        completed: 0,
        total: 1,
        rows: Vec::new(),
    };
    let text = progress_text(&event, "Running service api");
    assert_eq!(text, "[+] Running service api 0/1\n");
}

#[test]
fn failed_progress_row_includes_the_error_next_to_the_named_container() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let spec = resolved("cashdash-frontend", "app:latest");
    let row = OperationRow {
        index: 0,
        machine_id,
        machine_name: Some(MachineName::parse("machine-2").unwrap()),
        operation: DeployOperation::ReplaceContainer(ReplacementOperation {
            machine_id,
            old_container_id: ContainerId::parse("f".repeat(64)).unwrap(),
            spec,
            skip_health_monitor: false,
        }),
        display_name: None,
        service_name: None,
        status: OperationStatus::Failed {
            error: timed_out_create(),
        },
    };
    let event = DeployEvent::Progress {
        completed: 0,
        total: 1,
        rows: vec![row],
    };
    let text = progress_text(&event, "Deploying to default");
    assert!(
        text.contains("Container cashdash-frontend on machine-2"),
        "{text}"
    );
    assert!(
        text.contains("create failed: target Machine RPC timed out"),
        "{text}"
    );
    assert!(!text.contains("CreateContainer"), "{text}");
    assert!(
        !text.contains(&"f".repeat(64)) && !text.contains(&"d".repeat(32)),
        "{text}"
    );
}

#[test]
fn failed_deploy_footer_names_the_service_and_error_without_a_hash_dump() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let failed = DeployOperation::ReplaceContainer(ReplacementOperation {
        machine_id,
        old_container_id: ContainerId::parse("f".repeat(64)).unwrap(),
        spec: resolved("cashdash-frontend", "app:latest"),
        skip_health_monitor: false,
    });
    let unexecuted = vec![
        DeployOperation::ReplaceContainer(ReplacementOperation {
            machine_id,
            old_container_id: ContainerId::parse("a".repeat(64)).unwrap(),
            spec: resolved("cashdash-horizon", "app:latest"),
            skip_health_monitor: false,
        }),
        DeployOperation::RunContainer {
            machine_id,
            spec: resolved("cashdash-web", "app:latest"),
            skip_health_monitor: false,
        },
    ];
    let outcome = DeployOutcome::Failed {
        completed: Vec::new(),
        failed: FailedOperation::Operation {
            operation: failed,
            error: timed_out_create(),
        },
        unexecuted,
    };
    let text = outcome_text(&outcome);
    assert!(
        text.contains("• Container cashdash-horizon  Unexecuted"),
        "{text}"
    );
    assert!(
        text.contains("• Container cashdash-web  Unexecuted"),
        "{text}"
    );
    let footer = text
        .split_once("Failed:")
        .map(|(_, rest)| rest)
        .unwrap_or("");
    assert!(
        footer.contains("replace cashdash-frontend\n  create failed: target Machine RPC timed out"),
        "{text}"
    );
    assert!(!footer.contains("cashdash-horizon"), "{text}");
    assert!(!footer.contains("cashdash-web"), "{text}");
    assert!(!text.contains("Completed"), "{text}");
    assert!(!text.contains(&"d".repeat(32)), "{text}");
    assert!(!text.contains("next: ployz logs"), "{text}");
}

#[test]
fn failed_deploy_footer_mentions_completed_ops_only_when_some_landed() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let completed = DeployOperation::RunContainer {
        machine_id,
        spec: resolved("cashdash-reverb", "app:latest"),
        skip_health_monitor: true,
    };
    let failed = DeployOperation::RunContainer {
        machine_id,
        spec: resolved("cashdash-web", "app:latest"),
        skip_health_monitor: true,
    };
    let outcome = DeployOutcome::Failed {
        completed: vec![completed],
        failed: FailedOperation::Operation {
            operation: failed,
            error: timed_out_create(),
        },
        unexecuted: Vec::new(),
    };
    let text = outcome_text(&outcome);
    assert!(!text.contains("Completed"), "{text}");
    assert!(
        text.contains("Failed: create cashdash-web\n  create failed: target Machine RPC timed out"),
        "{text}"
    );
}

#[test]
fn failed_footer_names_machine_from_live_rows() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let spec = resolved("cashdash-frontend", "app:latest");
    let failed = DeployOperation::ReplaceContainer(ReplacementOperation {
        machine_id,
        old_container_id: ContainerId::parse("f".repeat(64)).unwrap(),
        spec,
        skip_health_monitor: false,
    });
    let web = DeployOperation::RunContainer {
        machine_id,
        spec: resolved("cashdash-web", "app:latest"),
        skip_health_monitor: false,
    };
    let volume = DeployOperation::RemoveVolume {
        id: DockerVolumeId {
            machine_id,
            name: DockerVolumeName::parse("cashdash_data").unwrap(),
        },
    };
    let rows = vec![
        OperationRow {
            index: 0,
            machine_id,
            machine_name: Some(MachineName::parse("machine-2").unwrap()),
            operation: failed.clone(),
            display_name: None,
            service_name: Some(ServiceName::parse("cashdash-frontend").unwrap()),
            status: OperationStatus::Failed {
                error: timed_out_create(),
            },
        },
        OperationRow {
            index: 1,
            machine_id,
            machine_name: Some(MachineName::parse("vultr").unwrap()),
            operation: web.clone(),
            display_name: None,
            service_name: Some(ServiceName::parse("cashdash-web").unwrap()),
            status: OperationStatus::Unexecuted,
        },
        OperationRow {
            index: 2,
            machine_id,
            machine_name: Some(MachineName::parse("vultr").unwrap()),
            operation: volume.clone(),
            display_name: None,
            service_name: None,
            status: OperationStatus::Unexecuted,
        },
    ];
    let outcome = DeployOutcome::Failed {
        completed: Vec::new(),
        failed: FailedOperation::Operation {
            operation: failed,
            error: timed_out_create(),
        },
        unexecuted: vec![web, volume],
    };
    let text = outcome_text_after(&outcome, &rows);
    assert!(
        text.contains("Failed: replace cashdash-frontend on machine-2"),
        "{text}"
    );
    assert!(
        text.contains("create failed: target Machine RPC timed out"),
        "{text}"
    );
    assert!(!text.contains("cashdash-web"), "{text}");
    assert!(!text.contains("cashdash_data"), "{text}");
    assert!(!text.contains("Completed"), "{text}");
    assert!(!text.contains(&"d".repeat(32)), "{text}");
}

#[test]
fn failed_volume_footer_keeps_the_failing_machine_when_names_collide() {
    let alpha = MachineId::parse("a".repeat(32)).unwrap();
    let beta = MachineId::parse("b".repeat(32)).unwrap();
    let name = DockerVolumeName::parse("data").unwrap();
    let on_alpha = DeployOperation::RemoveVolume {
        id: DockerVolumeId {
            machine_id: alpha,
            name: name.clone(),
        },
    };
    let on_beta = DeployOperation::RemoveVolume {
        id: DockerVolumeId {
            machine_id: beta,
            name,
        },
    };
    let rows = vec![
        OperationRow {
            index: 0,
            machine_id: alpha,
            machine_name: Some(MachineName::parse("alpha").unwrap()),
            operation: on_alpha.clone(),
            display_name: None,
            service_name: None,
            status: OperationStatus::Completed,
        },
        OperationRow {
            index: 1,
            machine_id: beta,
            machine_name: Some(MachineName::parse("beta").unwrap()),
            operation: on_beta.clone(),
            display_name: None,
            service_name: None,
            status: OperationStatus::Running {
                phase: OperationPhase::RemovingVolume,
            },
        },
    ];
    let outcome = DeployOutcome::Failed {
        completed: vec![on_alpha],
        failed: FailedOperation::Operation {
            operation: on_beta,
            error: ExecutionError::Machine {
                action: MachineAction::RemoveVolume,
                error: RpcError {
                    code: RpcErrorCode::Unavailable,
                    message: "target Machine RPC timed out".into(),
                    details: serde_json::Value::Null,
                },
            },
        },
        unexecuted: Vec::new(),
    };
    let text = outcome_text_after(&outcome, &rows);
    assert!(text.contains("Failed: remove data on beta"), "{text}");
    assert!(!text.contains("Failed: remove data on alpha"), "{text}");
}

#[test]
fn wait_healthy_footer_omits_the_dependent_machine() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let operation = DeployOperation::WaitHealthy {
        machine_id,
        dependent: QualifiedService::parse("app/web").unwrap(),
        dependency: QualifiedService::parse("app/db").unwrap(),
    };
    let row = OperationRow {
        index: 0,
        machine_id,
        machine_name: Some(MachineName::parse("edge").unwrap()),
        operation: operation.clone(),
        display_name: None,
        service_name: Some(ServiceName::parse("web").unwrap()),
        status: OperationStatus::Failed {
            error: ExecutionError::DependencyHealth {
                dependency: QualifiedService::parse("app/db").unwrap(),
                failure: ployz_core::DependencyHealthFailure::NoContainers,
            },
        },
    };
    let outcome = DeployOutcome::Failed {
        completed: Vec::new(),
        failed: FailedOperation::Operation {
            operation,
            error: ExecutionError::DependencyHealth {
                dependency: QualifiedService::parse("app/db").unwrap(),
                failure: ployz_core::DependencyHealthFailure::NoContainers,
            },
        },
        unexecuted: Vec::new(),
    };
    let text = outcome_text_after(&outcome, &[row]);
    assert!(text.contains("Failed: wait app/db\n"), "{text}");
    assert!(!text.contains(" on edge"), "{text}");
}

#[test]
fn health_cause_is_english_without_id_or_debug() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let container_id = ContainerId::parse("c".repeat(64)).unwrap();
    let spec = resolved("cashdash-frontend", "app:latest");
    let row = OperationRow {
        index: 0,
        machine_id,
        machine_name: Some(MachineName::parse("machine-2").unwrap()),
        operation: DeployOperation::ReplaceContainer(ReplacementOperation {
            machine_id,
            old_container_id: ContainerId::parse("f".repeat(64)).unwrap(),
            spec,
            skip_health_monitor: false,
        }),
        display_name: None,
        service_name: Some(ServiceName::parse("cashdash-frontend").unwrap()),
        status: OperationStatus::Failed {
            error: health_timeout(container_id),
        },
    };
    let event = DeployEvent::Progress {
        completed: 0,
        total: 1,
        rows: vec![row],
    };
    let text = progress_text(&event, "Deploying to default");
    assert!(text.contains("health check timed out"), "{text}");
    assert!(!text.contains(&"c".repeat(64)), "{text}");
    assert!(!text.contains("TimedOut"), "{text}");
}

#[test]
fn hook_exit_cause_is_english_without_id_or_debug() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let container_id = ContainerId::parse("b".repeat(64)).unwrap();
    let spec = resolved("migrate", "app:latest");
    let row = OperationRow {
        index: 0,
        machine_id,
        machine_name: Some(MachineName::parse("edge").unwrap()),
        operation: DeployOperation::RunHook {
            machine_id,
            spec,
            old_hook_containers: Vec::new(),
        },
        display_name: None,
        service_name: Some(ServiceName::parse("migrate").unwrap()),
        status: OperationStatus::Failed {
            error: ExecutionError::Hook {
                container_id,
                failure: HookFailure::Exit { code: 1 },
            },
        },
    };
    let text = progress_text(
        &DeployEvent::Progress {
            completed: 0,
            total: 1,
            rows: vec![row],
        },
        "Deploying to default",
    );
    assert!(text.contains("pre-deploy hook exited 1"), "{text}");
    assert!(!text.contains("Exit {"), "{text}");
    assert!(!text.contains(&"b".repeat(64)), "{text}");
}

#[test]
fn replacement_health_prints_compensation_facts() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let operation = ReplacementOperation {
        machine_id,
        old_container_id: ContainerId::parse("f".repeat(64)).unwrap(),
        spec: resolved("cashdash-frontend", "app:latest"),
        skip_health_monitor: false,
    };
    let outcome = DeployOutcome::Failed {
        completed: Vec::new(),
        failed: FailedOperation::ReplacementHealth {
            operation,
            error: health_timeout(ContainerId::parse("c".repeat(64)).unwrap()),
            compensation: ReplacementCompensation::StartFirst {
                stop_new_container: StopAttempt::Stopped,
            },
        },
        unexecuted: Vec::new(),
    };
    let text = outcome_text(&outcome);
    assert!(text.contains("stopped the new container"), "{text}");
    assert!(text.contains("health check timed out"), "{text}");
    assert!(!text.to_ascii_lowercase().contains("rolled back"), "{text}");
    assert!(!text.contains("reverted"), "{text}");
    assert!(!text.contains("restored"), "{text}");
}

#[test]
fn operation_failure_is_silent_on_compensation() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let outcome = DeployOutcome::Failed {
        completed: Vec::new(),
        failed: FailedOperation::Operation {
            operation: DeployOperation::ReplaceContainer(ReplacementOperation {
                machine_id,
                old_container_id: ContainerId::parse("f".repeat(64)).unwrap(),
                spec: resolved("cashdash-frontend", "app:latest"),
                skip_health_monitor: false,
            }),
            error: health_timeout(ContainerId::parse("c".repeat(64)).unwrap()),
        },
        unexecuted: Vec::new(),
    };
    let text = outcome_text(&outcome);
    assert!(!text.contains("stopped the new container"), "{text}");
    assert!(!text.contains("restarted the old container"), "{text}");
    assert!(!text.contains("did not restart"), "{text}");
    assert!(!text.contains("compensation"), "{text}");
}

#[test]
fn stop_first_replacement_prints_restart_facts() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let outcome = DeployOutcome::Failed {
        completed: Vec::new(),
        failed: FailedOperation::ReplacementHealth {
            operation: ReplacementOperation {
                machine_id,
                old_container_id: ContainerId::parse("f".repeat(64)).unwrap(),
                spec: resolved("cashdash-frontend", "app:latest"),
                skip_health_monitor: false,
            },
            error: health_timeout(ContainerId::parse("c".repeat(64)).unwrap()),
            compensation: ReplacementCompensation::StopFirst {
                stop_new_container: StopAttempt::Stopped,
                restart_old_container: RestartAttempt::NotAttempted,
            },
        },
        unexecuted: Vec::new(),
    };
    let text = outcome_text(&outcome);
    assert!(text.contains("stopped the new container"), "{text}");
    assert!(text.contains("did not restart the old container"), "{text}");
}

#[test]
fn health_failure_offers_logs_when_service_is_known() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let spec = resolved("cashdash-frontend", "app:latest");
    let operation = DeployOperation::ReplaceContainer(ReplacementOperation {
        machine_id,
        old_container_id: ContainerId::parse("f".repeat(64)).unwrap(),
        spec,
        skip_health_monitor: false,
    });
    let row = OperationRow {
        index: 0,
        machine_id,
        machine_name: Some(MachineName::parse("machine-2").unwrap()),
        operation: operation.clone(),
        display_name: None,
        service_name: Some(ServiceName::parse("cashdash-frontend").unwrap()),
        status: OperationStatus::Failed {
            error: health_timeout(ContainerId::parse("c".repeat(64)).unwrap()),
        },
    };
    let outcome = DeployOutcome::Failed {
        completed: Vec::new(),
        failed: FailedOperation::Operation {
            operation,
            error: health_timeout(ContainerId::parse("c".repeat(64)).unwrap()),
        },
        unexecuted: Vec::new(),
    };
    let text = outcome_text_after(&outcome, &[row]);
    assert!(
        text.contains("next: ployz logs cashdash-frontend"),
        "{text}"
    );
    assert!(
        !text.contains("next: ployz logs cashdash-frontend/"),
        "{text}"
    );
    assert!(!text.contains('/'), "{text}");
}

#[test]
fn colored_failed_mark_emits_csi_and_plain_does_not() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let row = OperationRow {
        index: 0,
        machine_id,
        machine_name: Some(MachineName::parse("machine-2").unwrap()),
        operation: DeployOperation::ReplaceContainer(ReplacementOperation {
            machine_id,
            old_container_id: ContainerId::parse("f".repeat(64)).unwrap(),
            spec: resolved("cashdash-frontend", "app:latest"),
            skip_health_monitor: false,
        }),
        display_name: None,
        service_name: None,
        status: OperationStatus::Failed {
            error: timed_out_create(),
        },
    };
    let rows = [row];
    let plain = report::paint_live("Deploying to default", 0, 1, &rows, &Ink::plain());
    let color = report::paint_live("Deploying to default", 0, 1, &rows, &Ink::color());
    assert!(!plain.contains('\u{1b}'), "{plain:?}");
    assert!(color.contains('\u{1b}'), "{color:?}");
    assert!(plain.contains("✖"), "{plain}");
}

#[test]
fn colored_failed_footer_does_not_color_the_service_name() {
    let machine_id = MachineId::parse("d".repeat(32)).unwrap();
    let row = OperationRow {
        index: 0,
        machine_id,
        machine_name: Some(MachineName::parse("machine-2").unwrap()),
        operation: DeployOperation::ReplaceContainer(ReplacementOperation {
            machine_id,
            old_container_id: ContainerId::parse("f".repeat(64)).unwrap(),
            spec: resolved("cashdash-frontend", "app:latest"),
            skip_health_monitor: false,
        }),
        display_name: None,
        service_name: None,
        status: OperationStatus::Failed {
            error: timed_out_create(),
        },
    };
    let outcome = DeployOutcome::Failed {
        completed: Vec::new(),
        failed: FailedOperation::Operation {
            operation: row.operation.clone(),
            error: timed_out_create(),
        },
        unexecuted: Vec::new(),
    };
    let color = report::paint_closing(&outcome, &[row], true, &Ink::color());
    let ink = Ink::color();
    assert!(
        color.contains(&ink.paint(Role::Fail, "Failed:")),
        "{color:?}"
    );
    assert!(
        !color.contains(&ink.paint(Role::Fail, "Failed: replace cashdash-frontend on machine-2")),
        "{color:?}"
    );
    assert!(color.contains("cashdash-frontend"), "{color:?}");
}

fn timed_out_create() -> ExecutionError {
    ExecutionError::Machine {
        action: MachineAction::CreateContainer,
        error: RpcError {
            code: RpcErrorCode::Unavailable,
            message: "target Machine RPC timed out".into(),
            details: serde_json::Value::Null,
        },
    }
}

fn health_timeout(container_id: ContainerId) -> ExecutionError {
    ExecutionError::Health {
        container_id,
        failure: HealthFailure::TimedOut,
    }
}

fn resolved(name: &str, image: &str) -> ResolvedServiceSpec {
    let requested: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": image, "pull_policy": "missing" }
    }))
    .unwrap();
    requested
        .to_resolved(
            ployz_core::ServiceId::parse("a".repeat(32)).unwrap(),
            ployz_core::ResolvedUpdateConfig {
                order: UpdateOrder::StartFirst,
                monitor_millis: None,
            },
        )
        .expect("volume graph is scoped")
}
