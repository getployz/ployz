//! The recovery code a Server's Sealing Key is backed up under: the Server wraps the key, and
//! a person sees the code once.

use ployz_core::{RecoveryCode, SealingKeyWrapped, WrapSealingKeyRequest, op};

use crate::cloud::enroll::RecoveryBackup;
use crate::cluster::{Call, Wait};
use crate::connect::Client;
use crate::failure::Failure as Error;

pub(crate) const RECOVERY_CODE_NOT_SAVED: &str =
    "Recovery code not saved. Run ployz recovery rotate now to make one.";
pub(crate) const KEEP_THE_CODE: &str = "Keep the recovery code safe; ployz recover needs it.";

/// Whether a person sees a fresh recovery code: on a terminal, or under `--json` only on request.
pub(crate) fn shows_code(requested: bool, json: bool, stdout_is_terminal: bool) -> bool {
    if json { requested } else { stdout_is_terminal }
}

pub(crate) async fn wrap_sealing_key(
    client: &mut Client,
    code: &RecoveryCode,
) -> Result<RecoveryBackup, Error> {
    let wrapped = wrap(client, code).await?;
    Ok(RecoveryBackup {
        public_key: wrapped.public_key,
        backup: wrapped.backup,
    })
}

async fn wrap(client: &mut Client, code: &RecoveryCode) -> Result<SealingKeyWrapped, Error> {
    Ok(client
        .call::<op::WrapSealingKey>(
            WrapSealingKeyRequest { code: code.clone() },
            Call::keyed().waiting(Wait::standard()),
        )
        .await?)
}
