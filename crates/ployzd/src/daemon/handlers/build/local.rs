use std::path::PathBuf;
use std::sync::Arc;

use ployz_api::{BuildLocalRequest, BuildOperationPayload, BuildResultPayload, DaemonPayload};
use ployz_build::local::{
    BuildCommandRunner, TokioBuildCommandRunner, build_command_failure_message, build_command_plan,
    build_image_artifact, normalize_build_image_name, plan_build_invocation,
    prepare_build_command_paths, present_build_availability, render_build_result,
};
use ployz_model::{
    BuildLocation, BuildOperationKind, ImageArtifact, ImageAvailabilityRecord, OperationStatus,
};
use ployz_runtime_api::RuntimeImageBackend;
use ployz_store_api::ImageAvailabilityStore;

use crate::daemon::DaemonState;

impl DaemonState {
    pub(crate) async fn handle_build_local(
        &self,
        request: &BuildLocalRequest,
    ) -> ployz_api::DaemonResponse {
        let runner = TokioBuildCommandRunner;
        self.handle_build_local_with_runner_and_backend(request, &runner, None)
            .await
    }

    async fn handle_build_local_with_runner_and_backend(
        &self,
        request: &BuildLocalRequest,
        runner: &dyn BuildCommandRunner,
        backend_result: Option<Result<Arc<dyn RuntimeImageBackend>, String>>,
    ) -> ployz_api::DaemonResponse {
        let active = match self.require_active(
            "BUILD_LOCAL_INACTIVE",
            "build local requires a running mesh",
        ) {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let invocation = match plan_build_invocation(request.method, &request.inputs) {
            Ok(invocation) => invocation,
            Err(error) => return self.err("BUILD_LOCAL_INPUT_INVALID", error),
        };
        if request.push_target.is_some() || !request.distribute_targets.is_empty() {
            return self.err(
                "BUILD_LOCAL_IMAGE_MOVEMENT_UNSUPPORTED",
                "build local does not support push_target or distribute_targets in this release; run image push or image distribute explicitly after the build",
            );
        }
        let context_dir = PathBuf::from(&request.context_dir);
        if !context_dir.is_dir() {
            return self.err(
                "BUILD_LOCAL_CONTEXT_NOT_FOUND",
                format!(
                    "build context '{}' is not a directory",
                    context_dir.display()
                ),
            );
        }
        let image_name = normalize_build_image_name(&request.image_name);
        let request = if image_name == request.image_name {
            request.clone()
        } else {
            BuildLocalRequest {
                image_name,
                ..request.clone()
            }
        };
        let operation_store = self.build_operation_store();
        let mut operation = match operation_store.begin_with_input_summary(
            BuildOperationKind::Local,
            request.method,
            BuildLocation::Local,
            "waiting for image build lock",
            invocation.summary.clone(),
        ) {
            Ok(operation) => operation,
            Err(error) => return self.err("BUILD_LOCAL_OPERATION_FAILED", error),
        };
        let command_paths = match prepare_build_command_paths(
            &self.data_dir,
            request.method,
            &operation.id,
            &invocation,
        ) {
            Ok(paths) => paths,
            Err(error) => {
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_INPUT_UNSUPPORTED",
                    error,
                );
            }
        };
        let command_plan = match build_command_plan(&request, &invocation, command_paths) {
            Ok(command_plan) => command_plan,
            Err(error) => {
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_INPUT_UNSUPPORTED",
                    error,
                );
            }
        };

        let build_lock = self.local_build_lock(&request.image_name).await;
        let Ok(_build_guard) = build_lock.try_lock() else {
            let message = format!(
                "another local build for image '{}' is already running",
                request.image_name
            );
            return self.fail_build_local_operation(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_IMAGE_BUSY",
                message,
            );
        };
        let backend = match match backend_result {
            Some(result) => result,
            None => self.runtime_image_backend().await,
        } {
            Ok(backend) => backend,
            Err(error) => {
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_RUNTIME_UNAVAILABLE",
                    error,
                );
            }
        };
        for step in &command_plan.pre_build_steps {
            if let Err(error) = operation_store.update_stage(&mut operation, step.kind.stage()) {
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_OPERATION_FAILED",
                    error,
                );
            }
            match runner.run(&step.command, &context_dir).await {
                Ok(output) if output.status_success => {}
                Ok(output) => {
                    let message = build_command_failure_message(&step.command, &output);
                    return self.fail_build_local_operation(
                        &operation_store,
                        &mut operation,
                        "BUILD_LOCAL_COMMAND_FAILED",
                        message,
                    );
                }
                Err(error) => {
                    let error = command_plan.redact_text(&error);
                    return self.fail_build_local_operation(
                        &operation_store,
                        &mut operation,
                        "BUILD_LOCAL_COMMAND_FAILED",
                        error,
                    );
                }
            }
        }
        let image_build = &command_plan.image_build;
        if let Err(error) = operation_store.update_stage(&mut operation, image_build.kind.stage()) {
            return self.fail_build_local_operation(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_OPERATION_FAILED",
                error,
            );
        }
        let output = match runner.run(&image_build.command, &context_dir).await {
            Ok(output) if output.status_success => output,
            Ok(output) => {
                let message = build_command_failure_message(&image_build.command, &output);
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_COMMAND_FAILED",
                    message,
                );
            }
            Err(error) => {
                let error = command_plan.redact_text(&error);
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_COMMAND_FAILED",
                    error,
                );
            }
        };

        if let Err(error) = operation_store.update_stage(&mut operation, "inspecting built image") {
            return self.fail_build_local_operation(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_OPERATION_FAILED",
                error,
            );
        }
        let image = match backend.as_ref().inspect_image(&request.image_name).await {
            Ok(Some(image)) => image,
            Ok(None) => {
                let message = format!("built image '{}' was not found", request.image_name);
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_IMAGE_NOT_FOUND",
                    message,
                );
            }
            Err(error) => {
                let message = format!("inspect built image '{}': {error}", request.image_name);
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_INSPECT_FAILED",
                    message,
                );
            }
        };
        let artifact = match build_image_artifact(&request, &image) {
            Ok(artifact) => artifact,
            Err(error) => {
                return self.fail_build_local_operation(
                    &operation_store,
                    &mut operation,
                    "BUILD_LOCAL_MISSING_DIGEST",
                    error,
                );
            }
        };
        let record =
            present_build_availability(&self.identity.machine_id, artifact.clone(), &operation.id);

        if let Err(error) = operation_store.update_artifact(&mut operation, artifact.clone()) {
            return self.fail_build_local_operation_with_result(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_OPERATION_FAILED",
                error,
                artifact,
                None,
            );
        }
        if let Err(error) = active.mesh.store.upsert_image_availability(&record).await {
            let message = format!("record built image availability: {error}");
            return self.fail_build_local_operation_with_result(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_AVAILABILITY_FAILED",
                message,
                artifact,
                None,
            );
        }
        if let Err(error) =
            operation_store.update_status(&mut operation, OperationStatus::Succeeded, None)
        {
            return self.fail_build_local_operation_with_result(
                &operation_store,
                &mut operation,
                "BUILD_LOCAL_OPERATION_FAILED",
                error,
                artifact,
                Some(record),
            );
        }

        let image_build_command = command_plan.image_build_command();
        let message = render_build_result(&operation.id, &record, output, image_build_command);
        self.ok_with_payload(
            message,
            Some(DaemonPayload::BuildResult(BuildResultPayload {
                operation_id: operation.id,
                artifact,
                availability: Some(record),
            })),
        )
    }

    fn fail_build_local_operation(
        &self,
        operation_store: &super::operations::BuildOperationStore,
        operation: &mut ployz_model::BuildOperationRecord,
        code: &str,
        message: String,
    ) -> ployz_api::DaemonResponse {
        let _ = operation_store.update_status(
            operation,
            OperationStatus::Failed,
            Some(message.clone()),
        );
        self.err_with_payload(
            code,
            message,
            Some(DaemonPayload::BuildOperation(BuildOperationPayload {
                operation: operation.clone(),
            })),
        )
    }

    fn fail_build_local_operation_with_result(
        &self,
        operation_store: &super::operations::BuildOperationStore,
        operation: &mut ployz_model::BuildOperationRecord,
        code: &str,
        message: String,
        artifact: ImageArtifact,
        availability: Option<ImageAvailabilityRecord>,
    ) -> ployz_api::DaemonResponse {
        let _ = operation_store.update_status(
            operation,
            OperationStatus::Failed,
            Some(message.clone()),
        );
        self.err_with_payload(
            code,
            message,
            Some(DaemonPayload::BuildResult(BuildResultPayload {
                operation_id: operation.id.clone(),
                artifact,
                availability,
            })),
        )
    }
}
