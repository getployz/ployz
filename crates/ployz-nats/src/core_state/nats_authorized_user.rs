//! Durable authority records for the NATS authorized principal set.
//!
//! ADR-0001 classification: these `KV_CORE` records are explicitly named
//! durable authority. Their recovery evidence is the on-disk
//! `authorized-users.conf`, which survives JetStream loss — control adopts
//! file entries back into this set on start before any render.

use super::{AsyncNatsCoreStateStore, CoreStateStoreError};
use crate::kv::{bounded_bucket_key_scan_entries_with_prefix, with_io_timeout};
use ployz_core::nats_config::NatsAuthorizedUser;
use ployz_core::state::NATS_AUTHORIZED_USER_PREFIX;

fn authorized_user_key(user: &NatsAuthorizedUser) -> String {
    format!(
        "{NATS_AUTHORIZED_USER_PREFIX}.{}",
        user.principal.authority_key()
    )
}

impl AsyncNatsCoreStateStore {
    /// Upserts the authority record for one principal. Replacing a
    /// principal's key (rotation before delivery) is allowed; removal is
    /// not — revocation must go through an explicit machine-remove
    /// operation.
    pub async fn replace_nats_authorized_user(
        &self,
        user: &NatsAuthorizedUser,
    ) -> Result<(), CoreStateStoreError> {
        let key = authorized_user_key(user);
        let payload = serde_json::to_vec(user).map_err(CoreStateStoreError::Encode)?;
        with_io_timeout(
            "nats authorized user put",
            self.bucket.put(&key, payload.into()),
        )
        .await?
        .map_err(|error| CoreStateStoreError::Get {
            key,
            message: error.to_string(),
        })?;

        Ok(())
    }

    /// Create-only write used by adopt-on-start: a file entry unknown to KV
    /// becomes an authority record, but adoption never clobbers a newer KV
    /// record for the same principal.
    pub async fn adopt_nats_authorized_user_if_absent(
        &self,
        user: &NatsAuthorizedUser,
    ) -> Result<(), CoreStateStoreError> {
        let key = authorized_user_key(user);
        let payload = serde_json::to_vec(user).map_err(CoreStateStoreError::Encode)?;
        let created = with_io_timeout(
            "nats authorized user create",
            self.bucket.create(&key, payload.into()),
        )
        .await?;
        match created {
            Ok(_) => Ok(()),
            Err(error) => {
                // An existing record means the principal is already
                // authoritative in KV; adoption has nothing to do.
                let existing =
                    with_io_timeout("nats authorized user conflict read", self.bucket.get(&key))
                        .await?
                        .map_err(|read_error| CoreStateStoreError::Get {
                            key: key.clone(),
                            message: read_error.to_string(),
                        })?;
                if existing.is_some() {
                    Ok(())
                } else {
                    Err(CoreStateStoreError::CasConflict {
                        message: error.to_string(),
                    })
                }
            }
        }
    }

    /// The full authorized principal set, ordered by authority key.
    pub async fn nats_authorized_users(
        &self,
    ) -> Result<Vec<NatsAuthorizedUser>, CoreStateStoreError> {
        let entries = bounded_bucket_key_scan_entries_with_prefix(
            &self.bucket,
            &format!("{NATS_AUTHORIZED_USER_PREFIX}."),
        )
        .await
        .map_err(|error| CoreStateStoreError::ListKeys {
            message: error.message,
        })?;

        let mut users = Vec::with_capacity(entries.len());
        for entry in entries {
            users.push(
                serde_json::from_slice::<NatsAuthorizedUser>(&entry.value)
                    .map_err(CoreStateStoreError::Decode)?,
            );
        }
        Ok(users)
    }
}
