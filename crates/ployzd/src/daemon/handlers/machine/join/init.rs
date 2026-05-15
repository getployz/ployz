use crate::daemon::DaemonState;
use crate::daemon::ssh::SshOptions;
use ployz_api::{DaemonRequest, DaemonResponse, MachineInstallOptions};

use super::super::operations::{
    MachineOperationArtifacts, MachineOperationKind, MachineOperationStatus,
};
use super::bootstrap::bootstrap_remote_machine;
use super::remote::remote_rpc_expect_ok;

impl DaemonState {
    pub(crate) async fn handle_machine_init(
        &self,
        target: &str,
        network: &str,
        install: &MachineInstallOptions,
    ) -> DaemonResponse {
        if self.active.is_some() {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                "machine init requires no local running network; switch context or run `mesh down` first",
            );
        }

        let operation_store = self.machine_operation_store();
        let mut operation = match operation_store.begin(
            MachineOperationKind::Init,
            Some(network.to_string()),
            vec![target.to_string()],
            "bootstrapping",
            MachineOperationArtifacts::default(),
        ) {
            Ok(operation) => operation,
            Err(err) => return self.err("MACHINE_OPERATION_START_FAILED", err),
        };

        if let Err(err) = bootstrap_remote_machine(target, install, &SshOptions::default()).await {
            let _ = operation_store.update_status(
                &mut operation,
                MachineOperationStatus::Failed,
                Some(err.clone()),
            );
            return self.err("SSH_BOOTSTRAP_FAILED", err);
        }

        if let Err(err) = operation_store.update_stage(&mut operation, "remote-init") {
            tracing::warn!(error = %err, operation_id = %operation.id, "machine init: failed to persist operation stage");
        }

        if let Err(err) = remote_rpc_expect_ok(
            target,
            DaemonRequest::MeshInit {
                network: network.to_string(),
            },
            &SshOptions::default(),
        )
        .await
        {
            let _ = operation_store.update_status(
                &mut operation,
                MachineOperationStatus::Failed,
                Some(err.clone()),
            );
            return self.err("REMOTE_INIT_FAILED", err);
        }

        let _ = operation_store.update_stage(&mut operation, "complete");
        let _ =
            operation_store.update_status(&mut operation, MachineOperationStatus::Succeeded, None);
        self.ok(format!(
            "remote founder initialized\n  target:  {target}\n  network: {network}"
        ))
    }
}
