use async_trait::async_trait;
use ployz_store_api::InviteStore;
use ployz_types::error::{Error, Result};
use ployz_types::model::{InviteRecord, MachineId};

use crate::NatsStore;
use crate::store::kv_json;

#[async_trait]
impl InviteStore for NatsStore {
    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        let bucket = invites_bucket(self).await?;
        let payload = serde_json::to_vec(invite)
            .map_err(|error| Error::operation("nats_invite_encode", error.to_string()))?;
        bucket
            .create(&invite.invite_id, payload.into())
            .await
            .map(|_| ())
            .map_err(|error| {
                Error::operation(
                    "invite_exists",
                    format!(
                        "invite '{}' already exists or could not be created: {error:?}",
                        invite.invite_id
                    ),
                )
            })
    }

    async fn get_invite(&self, invite_id: &str) -> Result<Option<InviteRecord>> {
        let bucket = invites_bucket(self).await?;
        let Some(bytes) = bucket
            .get(invite_id)
            .await
            .map_err(|error| Error::operation("nats_invite_get", format!("{error:?}")))?
        else {
            return Ok(None);
        };
        Ok(Some(decode_invite(invite_id, bytes.as_ref())?))
    }

    async fn list_invites(&self) -> Result<Vec<InviteRecord>> {
        let bucket = invites_bucket(self).await?;
        list_invites(&bucket).await
    }

    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> Result<InviteRecord> {
        let bucket = invites_bucket(self).await?;
        let Some(entry) = bucket
            .entry(invite_id.to_string())
            .await
            .map_err(|error| Error::operation("nats_invite_get", format!("{error:?}")))?
        else {
            return Err(Error::operation(
                "invite_not_found",
                format!("invite '{invite_id}' not found"),
            ));
        };
        let invite = decode_invite(invite_id, entry.value.as_ref())?;
        validate_redeemable(invite_id, &invite, machine_id, now_unix_secs)?;
        if invite.consumed_by.as_ref() == Some(machine_id) {
            return Ok(invite);
        }

        let mut next_invite = invite;
        next_invite.consumed_by = Some(machine_id.clone());
        next_invite.consumed_at = Some(now_unix_secs);
        update_invite(&bucket, invite_id, entry.revision, &next_invite).await?;
        Ok(next_invite)
    }

    async fn revoke_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<InviteRecord> {
        let bucket = invites_bucket(self).await?;
        let Some(entry) = bucket
            .entry(invite_id.to_string())
            .await
            .map_err(|error| Error::operation("nats_invite_get", format!("{error:?}")))?
        else {
            return Err(Error::operation(
                "invite_not_found",
                format!("invite '{invite_id}' not found"),
            ));
        };
        let invite = decode_invite(invite_id, entry.value.as_ref())?;
        if invite.consumed_by.is_some() {
            return Err(Error::operation(
                "invite_consumed",
                format!("invite '{invite_id}' is already consumed"),
            ));
        }
        let mut next_invite = invite;
        next_invite.revoked_at = Some(now_unix_secs);
        update_invite(&bucket, invite_id, entry.revision, &next_invite).await?;
        Ok(next_invite)
    }
}

async fn invites_bucket(store: &NatsStore) -> Result<async_nats::jetstream::kv::Store> {
    kv_json::get_bucket(
        store.jetstream(),
        store.assets().invites_bucket.as_str(),
        "nats_invites_bucket",
    )
    .await
}

fn validate_redeemable(
    invite_id: &str,
    invite: &InviteRecord,
    machine_id: &MachineId,
    now_unix_secs: u64,
) -> Result<()> {
    if invite.revoked_at.is_some() {
        return Err(Error::operation(
            "invite_revoked",
            format!("invite '{invite_id}' is revoked"),
        ));
    }
    if now_unix_secs > invite.expires_at {
        return Err(Error::operation(
            "invite_expired",
            format!("invite '{invite_id}' is expired"),
        ));
    }
    if let Some(consumed_by) = &invite.consumed_by
        && consumed_by != machine_id
    {
        return Err(Error::operation(
            "invite_consumed",
            format!("invite '{invite_id}' is already consumed"),
        ));
    }
    Ok(())
}

async fn update_invite(
    bucket: &async_nats::jetstream::kv::Store,
    invite_id: &str,
    revision: u64,
    invite: &InviteRecord,
) -> Result<()> {
    let payload = serde_json::to_vec(invite)
        .map_err(|error| Error::operation("nats_invite_encode", error.to_string()))?;
    bucket
        .update(invite_id, payload.into(), revision)
        .await
        .map(|_| ())
        .map_err(|error| Error::operation("nats_invite_update", format!("{error:?}")))
}

async fn list_invites(bucket: &async_nats::jetstream::kv::Store) -> Result<Vec<InviteRecord>> {
    let entries = kv_json::list_json_entries::<InviteRecord>(
        bucket,
        "nats_invite_decode",
        "nats_invites_list",
    )
    .await?;
    invite_records_from_entries(entries)
}

fn invite_records_from_entries(
    entries: Vec<kv_json::JsonEntry<InviteRecord>>,
) -> Result<Vec<InviteRecord>> {
    entries
        .into_iter()
        .map(|entry| validate_invite_key(&entry.key, entry.value))
        .collect()
}

fn decode_invite(key: &str, bytes: &[u8]) -> Result<InviteRecord> {
    let invite: InviteRecord = kv_json::decode_json("nats_invite_decode", bytes)?;
    validate_invite_key(key, invite)
}

fn validate_invite_key(key: &str, invite: InviteRecord) -> Result<InviteRecord> {
    if invite.invite_id != key {
        return Err(Error::operation(
            "nats_invite_decode",
            format!(
                "invite key {key} does not match payload id {}",
                invite.invite_id
            ),
        ));
    }
    Ok(invite)
}

#[cfg(test)]
mod tests {
    use super::{decode_invite, invite_records_from_entries};
    use crate::store::kv_json;
    use ployz_types::model::{InviteRecord, MachineId, NetworkId};

    #[test]
    fn invite_kv_decode_failure_is_visible() {
        let error = decode_invite("invite-a", b"{").expect_err("invalid JSON should fail");

        assert!(error.to_string().contains("nats_invite_decode"));
    }

    #[test]
    fn invite_kv_key_mismatch_is_visible() {
        let invite = test_invite("payload-invite");
        let bytes = serde_json::to_vec(&invite).expect("encode invite");

        let error = decode_invite("key-invite", &bytes).expect_err("key mismatch should fail");

        assert!(error.to_string().contains("key-invite"));
        assert!(error.to_string().contains("payload-invite"));
    }

    #[test]
    fn invite_kv_decode_accepts_matching_key() {
        let invite = test_invite("invite-a");
        let bytes = serde_json::to_vec(&invite).expect("encode invite");

        let decoded = decode_invite("invite-a", &bytes).expect("matching invite key");

        assert_eq!(decoded, invite);
    }

    #[test]
    fn invite_records_keep_backend_list_order() {
        let entries = vec![
            kv_json::JsonEntry {
                key: "invite-a".into(),
                value: test_invite("invite-a"),
            },
            kv_json::JsonEntry {
                key: "invite-b".into(),
                value: test_invite("invite-b"),
            },
        ];

        let records = invite_records_from_entries(entries).expect("entries decode");

        assert_eq!(
            records
                .iter()
                .map(|invite| invite.invite_id.as_str())
                .collect::<Vec<_>>(),
            ["invite-a", "invite-b"]
        );
    }

    fn test_invite(id: &str) -> InviteRecord {
        InviteRecord {
            invite_id: id.into(),
            network_id: NetworkId("net-a".into()),
            issuer_machine_id: MachineId("issuer".into()),
            issuer_verify_key: "verify".into(),
            expires_at: 100,
            consumed_by: None,
            consumed_at: None,
            revoked_at: None,
            signature: "signature".into(),
        }
    }
}
