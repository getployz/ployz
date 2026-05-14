use crate::daemon::DaemonState;
use ployz_api::{
    BranchEnvironmentListPayload, BranchEnvironmentPayload, BranchEnvironmentStatusRequest,
    DaemonPayload, DaemonResponse,
};
use ployz_spec::{Namespace, valid_storage_segment};
use ployz_store_api::DeployStore;

impl DaemonState {
    pub async fn handle_branch_environment_status(
        &self,
        request: &BranchEnvironmentStatusRequest,
    ) -> DaemonResponse {
        if !valid_storage_segment(&request.target_namespace) {
            return self.err(
                "BRANCH_ENVIRONMENT_INVALID_TARGET",
                "branch target namespace must be 1-63 chars of [a-z0-9_-], starting with a letter or digit",
            );
        }
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let target_namespace = Namespace::new(request.target_namespace.clone());
        let environment = match active
            .mesh
            .store
            .get_branch_environment(&target_namespace)
            .await
        {
            Ok(Some(environment)) => environment,
            Ok(None) => {
                return self.err(
                    "BRANCH_ENVIRONMENT_NOT_FOUND",
                    format!("branch environment '{target_namespace}' not found"),
                );
            }
            Err(error) => return self.err("BRANCH_ENVIRONMENT_STATUS_FAILED", error.to_string()),
        };
        self.ok_with_payload(
            format!(
                "branch environment '{}' is {}",
                environment.target_namespace, environment.state
            ),
            Some(DaemonPayload::BranchEnvironment(BranchEnvironmentPayload {
                environment,
            })),
        )
    }

    pub async fn handle_branch_environment_list(&self) -> DaemonResponse {
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let environments = match active.mesh.store.list_branch_environments().await {
            Ok(environments) => environments,
            Err(error) => return self.err("BRANCH_ENVIRONMENT_LIST_FAILED", error.to_string()),
        };
        self.ok_with_payload(
            format!("{} branch environment(s)", environments.len()),
            Some(DaemonPayload::BranchEnvironmentList(
                BranchEnvironmentListPayload { environments },
            )),
        )
    }
}
