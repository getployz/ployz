//! One Server's Trash as Ployz Cloud drives it: listed by any Operator, restored or emptied
//! only with a person's approval of exactly those entries on that Server, which the Server
//! checks itself. `ployz volume trash` reaches the Server's own socket instead.

use ployz_core::{
    Claim, EmptyTrashRequest, ListTrashRequest, MachineId, MachineTarget, RestoreTrashRequest,
    RpcError, TRASH_APPROVAL_CAPABILITY, TrashEmptied, TrashFailure, TrashList, TrashRestore,
    TrashRestored,
    authority::{Signed, TrashAction, TrashApproval},
    op,
};
use serde::Serialize;
use ts_rs::TS;

use super::{Session, resolve_target, visible_machine_id};
use crate::cluster::{Call, Deadline};
use crate::connect::{Client, TARGET_RPC_TIMEOUT};
use crate::handlers::volume::{restore::copies, trash::proven};

/// What a restore of each entry an approval names came to: the entries restored, and why
/// each other one wasn't.
#[derive(Clone, Debug, Default, Serialize, TS)]
pub struct TrashRestores {
    pub restored: Vec<TrashRestored>,
    pub failed: Vec<TrashFailure>,
}

impl Session {
    /// `machine`'s Trash as it lists it now. A Server whose Ployz lists it only to its own
    /// socket is refused as `unsupported` before it is asked.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, `machine` is not a
    /// visible Machine Target, its Ployz predates Cloud's Trash, or it doesn't answer.
    pub async fn list_trash(&self, machine: &str) -> Result<TrashList, RpcError> {
        let target = resolve_target(machine)?;
        let mut client = self.client()?;
        self.until_closed(async {
            let machines = client.machines().await?;
            let observed = crate::cluster::visible_machine(&target, &machines)?;
            if observed.machine.runtime.claim(TRASH_APPROVAL_CAPABILITY) == Claim::Lacks {
                return Err(RpcError::unsupported(format!(
                    "{} runs an older Ployz that shows its Trash only on the Server. Upgrade \
                     it to see its Trash here.",
                    observed.machine.name
                )));
            }
            client
                .call_shared::<op::ListTrash>(ListTrashRequest {}, once(&target))
                .await
                .map_err(crate::connect::rpc_error)
        })
        .await
    }

    /// Restore each entry `approval` names on `machine`, one at a time. A deleted root is
    /// pinned first, then made the Volume's data once every Server shows it holds no copy
    /// of it, as `ployz volume trash restore` does. An entry that fails is reported and the
    /// rest still go.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, `machine` is not a
    /// visible Machine Target, or `approval` isn't a restore of `machine`'s entries.
    pub async fn restore_trash(
        &self,
        machine: &str,
        approval: Signed<TrashApproval>,
    ) -> Result<TrashRestores, RpcError> {
        let target = resolve_target(machine)?;
        let body = approval
            .open()
            .map_err(|refusal| RpcError::invalid_argument(refusal.to_string()))?;
        if body.action != TrashAction::Restore {
            return Err(RpcError::invalid_argument(
                "This approval is for deleting, not restoring.",
            ));
        }
        let mut client = self.client()?;
        self.until_closed(async {
            let here = visible_machine_id(&mut client, &target).await?;
            let mut outcome = TrashRestores::default();
            for id in body.ids {
                match restore_one(&client, &target, here, id, &approval).await {
                    Ok(restored) => outcome.restored.push(restored),
                    Err(error) => outcome.failed.push(TrashFailure {
                        id,
                        reason: error.message,
                    }),
                }
            }
            Ok(outcome)
        })
        .await
    }

    /// Empty exactly the entries `approval` names on `machine`, pinned ones included.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, `machine` is not a
    /// Machine Target, `approval` isn't an empty, or the Server refuses it or doesn't
    /// answer.
    pub async fn empty_trash(
        &self,
        machine: &str,
        approval: Signed<TrashApproval>,
    ) -> Result<TrashEmptied, RpcError> {
        let target = resolve_target(machine)?;
        let body = approval
            .open()
            .map_err(|refusal| RpcError::invalid_argument(refusal.to_string()))?;
        if body.action != TrashAction::Empty {
            return Err(RpcError::invalid_argument(
                "This approval is for restoring, not deleting.",
            ));
        }
        let client = self.client()?;
        let request = EmptyTrashRequest {
            ids: body.ids,
            bulk: false,
            approval: Some(approval),
        };
        self.until_closed(async {
            client
                .call_shared::<op::EmptyTrash>(request, once(&target))
                .await
                .map_err(crate::connect::rpc_error)
        })
        .await
    }
}

async fn restore_one(
    client: &Client,
    target: &MachineTarget,
    here: MachineId,
    id: ployz_core::TrashId,
    approval: &Signed<TrashApproval>,
) -> Result<TrashRestored, RpcError> {
    let first = restore_call(client, target, id, false, approval).await?;
    let TrashRestore::Pinned { .. } = first.state else {
        return Ok(first);
    };
    let frame = client.runtime_frame().await?;
    let seen = copies(client, &frame, &first.name, true).await;
    proven(&first.name, id, here, &frame, &seen).map_err(|refused| refused.report())?;
    restore_call(client, target, id, true, approval).await
}

async fn restore_call(
    client: &Client,
    target: &MachineTarget,
    id: ployz_core::TrashId,
    promote: bool,
    approval: &Signed<TrashApproval>,
) -> Result<TrashRestored, RpcError> {
    client
        .call_shared::<op::RestoreTrash>(
            RestoreTrashRequest {
                id,
                promote,
                approval: Some(approval.clone()),
            },
            once(target),
        )
        .await
        .map_err(crate::connect::rpc_error)
}

fn once(target: &MachineTarget) -> crate::cluster::Once<'_> {
    Call::once()
        .to(target)
        .within(Deadline::Target(TARGET_RPC_TIMEOUT))
}
