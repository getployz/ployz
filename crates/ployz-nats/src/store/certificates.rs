use async_nats::jetstream::kv;
use futures_util::{StreamExt, TryStreamExt};
use ployz_store_api::{AcmeChallengeSubscription, CertificateStore, CertificateSubscription};
use ployz_types::error::{Error, Result};
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeEvent, AcmeChallengeReadinessRecord, AcmeChallengeRecord,
    CertificateEvent, CertificateRecord, MachineId,
};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::warn;

use crate::NatsStore;
use crate::buckets::{
    ACME_ACCOUNTS_BUCKET, ACME_CHALLENGE_READINESS_BUCKET, ACME_CHALLENGES_BUCKET,
    CERTIFICATES_BUCKET,
};
use crate::store::kv_json;
use crate::subjects;

impl CertificateStore for NatsStore {
    async fn get_acme_account(&self, issuer_url: &str) -> Result<Option<AcmeAccountRecord>> {
        let bucket = kv_json::get_bucket(
            self.jetstream(),
            ACME_ACCOUNTS_BUCKET,
            "nats_acme_accounts_bucket",
        )
        .await?;
        let Some(bytes) = bucket
            .get(acme_account_key(issuer_url))
            .await
            .map_err(|error| {
                ployz_types::Error::operation("nats_acme_account_get", format!("{error:?}"))
            })?
        else {
            return Ok(None);
        };
        Ok(Some(kv_json::decode_json(
            "nats_acme_account_decode",
            bytes.as_ref(),
        )?))
    }

    async fn upsert_acme_account(&self, record: &AcmeAccountRecord) -> Result<()> {
        let bucket = kv_json::get_bucket(
            self.jetstream(),
            ACME_ACCOUNTS_BUCKET,
            "nats_acme_accounts_bucket",
        )
        .await?;
        kv_json::put_json(
            &bucket,
            &acme_account_key(&record.issuer_url),
            record,
            "nats_acme_account_encode",
            "nats_acme_account_put",
        )
        .await
    }

    async fn list_certificates(&self) -> Result<Vec<CertificateRecord>> {
        let bucket = certificates_bucket(self).await?;
        kv_json::list_json(&bucket, "nats_certificate_decode", "nats_certificates_list").await
    }

    async fn get_certificate(&self, hostname: &str) -> Result<Option<CertificateRecord>> {
        let bucket = certificates_bucket(self).await?;
        let Some(bytes) = bucket
            .get(certificate_key(hostname))
            .await
            .map_err(|error| {
                ployz_types::Error::operation("nats_certificate_get", format!("{error:?}"))
            })?
        else {
            return Ok(None);
        };
        Ok(Some(kv_json::decode_json(
            "nats_certificate_decode",
            bytes.as_ref(),
        )?))
    }

    async fn upsert_certificate(&self, record: &CertificateRecord) -> Result<()> {
        let bucket = certificates_bucket(self).await?;
        kv_json::put_json(
            &bucket,
            &certificate_key(&record.hostname),
            record,
            "nats_certificate_encode",
            "nats_certificate_put",
        )
        .await
    }

    async fn list_acme_challenges(&self) -> Result<Vec<AcmeChallengeRecord>> {
        let bucket = challenges_bucket(self).await?;
        kv_json::list_json(
            &bucket,
            "nats_acme_challenge_decode",
            "nats_acme_challenges_list",
        )
        .await
    }

    async fn upsert_acme_challenge(&self, record: &AcmeChallengeRecord) -> Result<()> {
        let bucket = challenges_bucket(self).await?;
        kv_json::put_json(
            &bucket,
            &challenge_key(&record.hostname, &record.token),
            record,
            "nats_acme_challenge_encode",
            "nats_acme_challenge_put",
        )
        .await
    }

    async fn delete_acme_challenge(&self, hostname: &str, token: &str) -> Result<()> {
        let bucket = challenges_bucket(self).await?;
        kv_json::delete(
            &bucket,
            &challenge_key(hostname, token),
            "nats_acme_challenge_delete",
        )
        .await?;
        delete_acme_challenge_readiness(self, hostname, token).await
    }

    async fn subscribe_certificates(&self) -> Result<CertificateSubscription> {
        let bucket = certificates_bucket(self).await?;
        let snapshot_boundary =
            kv_json::latest_sequence(&bucket, "nats_certificates_snapshot_boundary").await?;
        let snapshot = self.list_certificates().await?;
        let mut watch = bucket
            .watch_all_from_revision(kv_json::next_sequence(snapshot_boundary))
            .await
            .map_err(|error| {
                ployz_types::Error::operation("nats_certificates_watch", format!("{error:?}"))
            })?;
        let (tx, rx) = mpsc::channel(128);
        let last_seen_snapshot = snapshot.clone();
        tokio::spawn(async move {
            let mut last_seen = last_seen_snapshot
                .iter()
                .map(|record| (certificate_key(&record.hostname), record.clone()))
                .collect::<HashMap<_, _>>();
            loop {
                let next = tokio::select! {
                    _ = tx.closed() => break,
                    next = watch.next() => next,
                };
                let Some(next) = next else {
                    break;
                };
                let entry = match next {
                    Ok(entry) => entry,
                    Err(error) => {
                        let error =
                            Error::operation("nats_certificates_watch", format!("{error:?}"));
                        warn!(?error, "NATS certificate watcher failed");
                        let _ = tx.send(Err(error)).await;
                        break;
                    }
                };
                let Some(event) = (match certificate_event_from_kv_entry(
                    &mut last_seen,
                    entry.key.as_str(),
                    entry.value.as_ref(),
                    entry.operation,
                ) {
                    Ok(event) => event,
                    Err(error) => {
                        warn!(?error, key = %entry.key, "NATS certificate event decode failed");
                        let _ = tx.send(Err(error)).await;
                        break;
                    }
                }) else {
                    continue;
                };
                if tx.send(Ok(event)).await.is_err() {
                    break;
                }
            }
        });
        Ok((snapshot, rx))
    }

    async fn subscribe_acme_challenges(&self) -> Result<AcmeChallengeSubscription> {
        let bucket = challenges_bucket(self).await?;
        let snapshot_boundary =
            kv_json::latest_sequence(&bucket, "nats_acme_challenges_snapshot_boundary").await?;
        let snapshot = self.list_acme_challenges().await?;
        let mut watch = bucket
            .watch_all_from_revision(kv_json::next_sequence(snapshot_boundary))
            .await
            .map_err(|error| {
                ployz_types::Error::operation("nats_acme_challenges_watch", format!("{error:?}"))
            })?;
        let (tx, rx) = mpsc::channel(128);
        let last_seen_snapshot = snapshot.clone();
        tokio::spawn(async move {
            let mut last_seen = last_seen_snapshot
                .iter()
                .map(|record| {
                    (
                        challenge_key(&record.hostname, &record.token),
                        record.clone(),
                    )
                })
                .collect::<HashMap<_, _>>();
            loop {
                let next = tokio::select! {
                    _ = tx.closed() => break,
                    next = watch.next() => next,
                };
                let Some(next) = next else {
                    break;
                };
                let entry = match next {
                    Ok(entry) => entry,
                    Err(error) => {
                        let error =
                            Error::operation("nats_acme_challenges_watch", format!("{error:?}"));
                        warn!(?error, "NATS ACME challenge watcher failed");
                        let _ = tx.send(Err(error)).await;
                        break;
                    }
                };
                let Some(event) = (match challenge_event_from_kv_entry(
                    &mut last_seen,
                    entry.key.as_str(),
                    entry.value.as_ref(),
                    entry.operation,
                ) {
                    Ok(event) => event,
                    Err(error) => {
                        warn!(?error, key = %entry.key, "NATS ACME challenge event decode failed");
                        let _ = tx.send(Err(error)).await;
                        break;
                    }
                }) else {
                    continue;
                };
                if tx.send(Ok(event)).await.is_err() {
                    break;
                }
            }
        });
        Ok((snapshot, rx))
    }

    async fn upsert_acme_challenge_readiness(
        &self,
        record: &AcmeChallengeReadinessRecord,
    ) -> Result<()> {
        let bucket = readiness_bucket(self).await?;
        kv_json::put_json(
            &bucket,
            &readiness_key(&record.hostname, &record.token, &record.machine_id),
            record,
            "nats_acme_readiness_encode",
            "nats_acme_readiness_put",
        )
        .await
    }

    async fn list_acme_challenge_readiness(
        &self,
        hostname: &str,
        token: &str,
    ) -> Result<Vec<AcmeChallengeReadinessRecord>> {
        let bucket = readiness_bucket(self).await?;
        let records = kv_json::list_json::<AcmeChallengeReadinessRecord>(
            &bucket,
            "nats_acme_readiness_decode",
            "nats_acme_readiness_list",
        )
        .await?;
        let normalized_hostname = certificate_key(hostname);
        Ok(records
            .into_iter()
            .filter(|record| {
                certificate_key(&record.hostname) == normalized_hostname && record.token == token
            })
            .collect())
    }
}

async fn certificates_bucket(store: &NatsStore) -> Result<kv::Store> {
    kv_json::get_bucket(
        store.jetstream(),
        CERTIFICATES_BUCKET,
        "nats_certificates_bucket",
    )
    .await
}

async fn challenges_bucket(store: &NatsStore) -> Result<kv::Store> {
    kv_json::get_bucket(
        store.jetstream(),
        ACME_CHALLENGES_BUCKET,
        "nats_acme_challenges_bucket",
    )
    .await
}

async fn readiness_bucket(store: &NatsStore) -> Result<kv::Store> {
    kv_json::get_bucket(
        store.jetstream(),
        ACME_CHALLENGE_READINESS_BUCKET,
        "nats_acme_readiness_bucket",
    )
    .await
}

fn certificate_key(hostname: &str) -> String {
    hostname.trim_end_matches('.').to_ascii_lowercase()
}

fn challenge_key(hostname: &str, token: &str) -> String {
    format!(
        "{}.{}",
        subjects::kv_key_token(&certificate_key(hostname)),
        subjects::kv_key_token(token)
    )
}

fn readiness_key(hostname: &str, token: &str, machine_id: &MachineId) -> String {
    format!(
        "{}.{}",
        challenge_key(hostname, token),
        subjects::kv_key_token(&machine_id.0)
    )
}

fn readiness_key_prefix(hostname: &str, token: &str) -> String {
    format!("{}.", challenge_key(hostname, token))
}

fn certificate_event_from_kv_entry(
    last_seen: &mut HashMap<String, CertificateRecord>,
    key: &str,
    bytes: &[u8],
    operation: kv::Operation,
) -> Result<Option<CertificateEvent>> {
    match operation {
        kv::Operation::Put => {
            let record =
                kv_json::decode_json::<CertificateRecord>("nats_certificate_decode", bytes)?;
            let expected_key = certificate_key(&record.hostname);
            if expected_key != key {
                return Err(Error::operation(
                    "nats_certificate_decode",
                    format!("certificate key {key} does not match payload key {expected_key}"),
                ));
            }
            let event = if last_seen.contains_key(key) {
                CertificateEvent::Updated(record.clone())
            } else {
                CertificateEvent::Added(record.clone())
            };
            last_seen.insert(key.to_string(), record);
            Ok(Some(event))
        }
        kv::Operation::Delete | kv::Operation::Purge => {
            Ok(last_seen.remove(key).map(CertificateEvent::Removed))
        }
    }
}

fn challenge_event_from_kv_entry(
    last_seen: &mut HashMap<String, AcmeChallengeRecord>,
    key: &str,
    bytes: &[u8],
    operation: kv::Operation,
) -> Result<Option<AcmeChallengeEvent>> {
    match operation {
        kv::Operation::Put => {
            let record =
                kv_json::decode_json::<AcmeChallengeRecord>("nats_acme_challenge_decode", bytes)?;
            let expected_key = challenge_key(&record.hostname, &record.token);
            if expected_key != key {
                return Err(Error::operation(
                    "nats_acme_challenge_decode",
                    format!("ACME challenge key {key} does not match payload key {expected_key}"),
                ));
            }
            let event = if last_seen.contains_key(key) {
                AcmeChallengeEvent::Updated(record.clone())
            } else {
                AcmeChallengeEvent::Added(record.clone())
            };
            last_seen.insert(key.to_string(), record);
            Ok(Some(event))
        }
        kv::Operation::Delete | kv::Operation::Purge => {
            Ok(last_seen.remove(key).map(AcmeChallengeEvent::Removed))
        }
    }
}

async fn delete_acme_challenge_readiness(
    store: &NatsStore,
    hostname: &str,
    token: &str,
) -> Result<()> {
    let bucket = readiness_bucket(store).await?;
    let prefix = readiness_key_prefix(hostname, token);
    let keys = bucket
        .keys()
        .await
        .map_err(|error| {
            ployz_types::Error::operation("nats_acme_readiness_keys", format!("{error:?}"))
        })?
        .try_collect::<Vec<String>>()
        .await
        .map_err(|error| {
            ployz_types::Error::operation("nats_acme_readiness_keys", format!("{error:?}"))
        })?;
    for key in keys.into_iter().filter(|key| key.starts_with(&prefix)) {
        kv_json::delete(&bucket, &key, "nats_acme_readiness_delete").await?;
    }
    Ok(())
}

fn acme_account_key(issuer_url: &str) -> String {
    subjects::kv_key_token(issuer_url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::CertificateState;

    #[test]
    fn certificate_kv_decode_failure_is_subscription_failure() {
        let mut last_seen = HashMap::new();

        let result = certificate_event_from_kv_entry(
            &mut last_seen,
            "example.com",
            b"{",
            kv::Operation::Put,
        );

        assert!(result.is_err());
        assert!(last_seen.is_empty());
    }

    #[test]
    fn certificate_kv_key_mismatch_is_subscription_failure() {
        let record = certificate("example.com");
        let bytes = serde_json::to_vec(&record).expect("encode certificate");
        let mut last_seen = HashMap::new();

        let result = certificate_event_from_kv_entry(
            &mut last_seen,
            "other.example.com",
            &bytes,
            kv::Operation::Put,
        );

        assert!(result.is_err());
        assert!(last_seen.is_empty());
    }

    #[test]
    fn certificate_kv_delete_for_unknown_key_is_noop() {
        let mut last_seen = HashMap::new();

        let event = certificate_event_from_kv_entry(
            &mut last_seen,
            "example.com",
            &[],
            kv::Operation::Delete,
        )
        .expect("delete should not fail");

        assert!(event.is_none());
    }

    #[test]
    fn acme_challenge_kv_decode_failure_is_subscription_failure() {
        let mut last_seen = HashMap::new();

        let result = challenge_event_from_kv_entry(
            &mut last_seen,
            "example.com.token",
            b"{",
            kv::Operation::Put,
        );

        assert!(result.is_err());
        assert!(last_seen.is_empty());
    }

    #[test]
    fn acme_challenge_kv_key_mismatch_is_subscription_failure() {
        let record = challenge("example.com", "token");
        let bytes = serde_json::to_vec(&record).expect("encode challenge");
        let mut last_seen = HashMap::new();

        let result = challenge_event_from_kv_entry(
            &mut last_seen,
            "example.com.other-token",
            &bytes,
            kv::Operation::Put,
        );

        assert!(result.is_err());
        assert!(last_seen.is_empty());
    }

    #[test]
    fn acme_challenge_kv_delete_for_unknown_key_is_noop() {
        let mut last_seen = HashMap::new();

        let event = challenge_event_from_kv_entry(
            &mut last_seen,
            "example.com.token",
            &[],
            kv::Operation::Delete,
        )
        .expect("delete should not fail");

        assert!(event.is_none());
    }

    #[test]
    fn acme_challenge_readiness_key_is_collision_safe() {
        let machine = MachineId("machine.1".into());

        let dotted = readiness_key("foo.bar.example", "token", &machine);
        let underscored = readiness_key("foo_bar.example", "token", &machine);

        assert_ne!(dotted, underscored);
        assert!(!dotted.contains("foo.bar"));
    }

    #[test]
    fn acme_challenge_key_includes_token() {
        let old = readiness_key("example.com", "old-token", &MachineId("machine-a".into()));
        let new = readiness_key("example.com", "new-token", &MachineId("machine-a".into()));

        assert_ne!(old, new);
    }

    #[test]
    fn readiness_key_prefix_matches_only_challenge_entries() {
        let prefix = readiness_key_prefix("example.com", "token");
        let readiness = readiness_key("example.com", "token", &MachineId("machine-a".into()));
        let other = readiness_key("example.com", "token-extra", &MachineId("machine-a".into()));

        assert!(readiness.starts_with(&prefix));
        assert!(!other.starts_with(&prefix));
    }

    #[test]
    fn certificate_kv_put_updates_last_seen() {
        let record = certificate("example.com");
        let bytes = serde_json::to_vec(&record).expect("encode certificate");
        let mut last_seen = HashMap::new();

        let event = certificate_event_from_kv_entry(
            &mut last_seen,
            "example.com",
            &bytes,
            kv::Operation::Put,
        )
        .expect("put should decode");

        assert!(
            matches!(event, Some(CertificateEvent::Added(event_record)) if event_record == record)
        );
        assert_eq!(last_seen.get("example.com"), Some(&record));
    }

    #[test]
    fn acme_challenge_kv_put_updates_last_seen() {
        let record = challenge("example.com", "token");
        let key = challenge_key(&record.hostname, &record.token);
        let bytes = serde_json::to_vec(&record).expect("encode challenge");
        let mut last_seen = HashMap::new();

        let event = challenge_event_from_kv_entry(&mut last_seen, &key, &bytes, kv::Operation::Put)
            .expect("put should decode");

        assert!(
            matches!(event, Some(AcmeChallengeEvent::Added(event_record)) if event_record == record)
        );
        assert_eq!(last_seen.get(&key), Some(&record));
    }

    fn certificate(hostname: &str) -> CertificateRecord {
        CertificateRecord {
            hostname: hostname.into(),
            issuer_url: "https://issuer.example/acme".into(),
            account_id: "account-1".into(),
            state: CertificateState::Pending,
            active_version_id: None,
            versions: Vec::new(),
            order_url: None,
            last_error: None,
            requested_at: 1,
            updated_at: 1,
            next_renewal_at: None,
        }
    }

    fn challenge(hostname: &str, token: &str) -> AcmeChallengeRecord {
        AcmeChallengeRecord {
            hostname: hostname.into(),
            token: token.into(),
            key_authorization: "authorization".into(),
            expires_at: 10,
            created_at: 1,
        }
    }
}
