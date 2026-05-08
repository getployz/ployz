use crate::NatsStore;
use crate::coord::jobs::{JobSchedule, publish_cert_renewal_job_in};
use crate::store::kv_json;
use crate::store::kv_watch;
use crate::subjects;
use async_nats::jetstream::kv;
use async_trait::async_trait;
use ployz_store_api::{AcmeChallengeSubscription, CertificateStore, CertificateSubscription};
use ployz_types::error::{Error, Result, StoreRecordKind};
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeEvent, AcmeChallengeReadinessRecord, AcmeChallengeRecord,
    CertificateEvent, CertificateRecord, CertificateState, MachineId,
};

#[async_trait]
impl CertificateStore for NatsStore {
    async fn get_acme_account(&self, issuer_url: &str) -> Result<Option<AcmeAccountRecord>> {
        let bucket = kv_json::get_bucket(
            self.jetstream(),
            self.assets().acme_accounts_bucket.as_str(),
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
        Ok(Some(decode_acme_account(
            &acme_account_key(issuer_url),
            bytes.as_ref(),
        )?))
    }

    async fn upsert_acme_account(&self, record: &AcmeAccountRecord) -> Result<()> {
        let bucket = kv_json::get_bucket(
            self.jetstream(),
            self.assets().acme_accounts_bucket.as_str(),
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
        list_certificates(&bucket).await
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
        Ok(Some(decode_certificate(
            &certificate_key(hostname),
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
        .await?;
        if let Some(schedule) = certificate_renewal_job_schedule(record) {
            publish_cert_renewal_job_in(self.jetstream(), self.scope(), &record.hostname, schedule)
                .await?;
        }
        Ok(())
    }

    async fn list_acme_challenges(&self) -> Result<Vec<AcmeChallengeRecord>> {
        let bucket = challenges_bucket(self).await?;
        list_challenges(&bucket).await
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
        let observed_revision =
            kv_json::latest_sequence(&bucket, "nats_certificates_observed_revision").await?;
        let snapshot_entries = kv_json::list_json_entries::<CertificateRecord>(
            &bucket,
            "nats_certificate_decode",
            "nats_certificates_list",
        )
        .await?;
        let snapshot = snapshot_entries
            .into_iter()
            .map(|entry| validate_certificate_key(&entry.key, entry.value))
            .collect::<Result<Vec<_>>>()?;
        kv_watch::subscribe_all(
            &bucket,
            snapshot,
            observed_revision,
            |record: &CertificateRecord| certificate_key(&record.hostname),
            decode_certificate,
            CertificateEvent::Upsert,
            |record| CertificateEvent::Removed {
                hostname: record.hostname,
            },
            "nats_certificates_watch",
            "NATS certificate watcher failed",
            "NATS certificate event decode failed",
        )
        .await
    }

    async fn subscribe_acme_challenges(&self) -> Result<AcmeChallengeSubscription> {
        let bucket = challenges_bucket(self).await?;
        let observed_revision =
            kv_json::latest_sequence(&bucket, "nats_acme_challenges_observed_revision").await?;
        let snapshot_entries = kv_json::list_json_entries::<AcmeChallengeRecord>(
            &bucket,
            "nats_acme_challenge_decode",
            "nats_acme_challenges_list",
        )
        .await?;
        let snapshot = acme_challenge_snapshot(snapshot_entries)?;
        kv_watch::subscribe_all(
            &bucket,
            snapshot,
            observed_revision,
            |record: &AcmeChallengeRecord| challenge_key(&record.hostname, &record.token),
            decode_challenge,
            AcmeChallengeEvent::Upsert,
            |record| AcmeChallengeEvent::Removed {
                hostname: record.hostname,
                token: record.token,
            },
            "nats_acme_challenges_watch",
            "NATS ACME challenge watcher failed",
            "NATS ACME challenge event decode failed",
        )
        .await
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
        let records = list_readiness(&bucket).await?;
        let normalized_hostname = certificate_key(hostname);
        Ok(records
            .into_iter()
            .filter(|record| {
                certificate_key(&record.hostname) == normalized_hostname && record.token == token
            })
            .collect())
    }
}

fn certificate_renewal_job_schedule(record: &CertificateRecord) -> Option<JobSchedule> {
    if record.state != CertificateState::Active {
        return None;
    }
    record.next_renewal_at.map(JobSchedule::AtUnixSecs)
}

async fn certificates_bucket(store: &NatsStore) -> Result<kv::Store> {
    kv_json::get_bucket(
        store.jetstream(),
        store.assets().certificates_bucket.as_str(),
        "nats_certificates_bucket",
    )
    .await
}

async fn challenges_bucket(store: &NatsStore) -> Result<kv::Store> {
    kv_json::get_bucket(
        store.jetstream(),
        store.assets().acme_challenges_bucket.as_str(),
        "nats_acme_challenges_bucket",
    )
    .await
}

async fn readiness_bucket(store: &NatsStore) -> Result<kv::Store> {
    kv_json::get_bucket(
        store.jetstream(),
        store.assets().acme_challenge_readiness_bucket.as_str(),
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

async fn list_certificates(bucket: &kv::Store) -> Result<Vec<CertificateRecord>> {
    kv_json::list_json_entries::<CertificateRecord>(
        bucket,
        "nats_certificate_decode",
        "nats_certificates_list",
    )
    .await?
    .into_iter()
    .map(|entry| validate_certificate_key(&entry.key, entry.value))
    .collect()
}

fn decode_certificate(key: &str, bytes: &[u8]) -> Result<CertificateRecord> {
    let record: CertificateRecord = kv_json::decode_json("nats_certificate_decode", bytes)?;
    validate_certificate_key(key, record)
}

fn validate_certificate_key(key: &str, record: CertificateRecord) -> Result<CertificateRecord> {
    let expected_key = certificate_key(&record.hostname);
    if expected_key != key {
        return Err(Error::store_key_mismatch(
            StoreRecordKind::Certificate,
            key,
            expected_key,
        ));
    }
    Ok(record)
}

async fn list_challenges(bucket: &kv::Store) -> Result<Vec<AcmeChallengeRecord>> {
    kv_json::list_json_entries::<AcmeChallengeRecord>(
        bucket,
        "nats_acme_challenge_decode",
        "nats_acme_challenges_list",
    )
    .await?
    .into_iter()
    .map(|entry| validate_challenge_key(&entry.key, entry.value))
    .collect()
}

fn validate_challenge_key(key: &str, record: AcmeChallengeRecord) -> Result<AcmeChallengeRecord> {
    let expected_key = challenge_key(&record.hostname, &record.token);
    if expected_key != key {
        return Err(Error::store_key_mismatch(
            StoreRecordKind::AcmeChallenge,
            key,
            expected_key,
        ));
    }
    Ok(record)
}

async fn list_readiness(bucket: &kv::Store) -> Result<Vec<AcmeChallengeReadinessRecord>> {
    kv_json::list_json_entries::<AcmeChallengeReadinessRecord>(
        bucket,
        "nats_acme_readiness_decode",
        "nats_acme_readiness_list",
    )
    .await?
    .into_iter()
    .map(|entry| validate_readiness_key(&entry.key, entry.value))
    .collect()
}

fn validate_readiness_key(
    key: &str,
    record: AcmeChallengeReadinessRecord,
) -> Result<AcmeChallengeReadinessRecord> {
    let expected_key = readiness_key(&record.hostname, &record.token, &record.machine_id);
    if expected_key != key {
        return Err(Error::store_key_mismatch(
            StoreRecordKind::AcmeChallengeReadiness,
            key,
            expected_key,
        ));
    }
    Ok(record)
}

fn decode_challenge(key: &str, bytes: &[u8]) -> Result<AcmeChallengeRecord> {
    let record = kv_json::decode_json::<AcmeChallengeRecord>("nats_acme_challenge_decode", bytes)?;
    validate_challenge_key(key, record)
}

fn acme_challenge_snapshot(
    entries: Vec<kv_json::JsonEntry<AcmeChallengeRecord>>,
) -> Result<Vec<AcmeChallengeRecord>> {
    entries
        .into_iter()
        .map(|entry| validate_challenge_key(&entry.key, entry.value))
        .collect()
}

async fn delete_acme_challenge_readiness(
    store: &NatsStore,
    hostname: &str,
    token: &str,
) -> Result<()> {
    let bucket = readiness_bucket(store).await?;
    let prefix = readiness_key_prefix(hostname, token);
    for key in kv_json::list_keys_with_prefix(&bucket, &prefix, "nats_acme_readiness_keys").await? {
        kv_json::delete(&bucket, &key, "nats_acme_readiness_delete").await?;
    }
    Ok(())
}

fn acme_account_key(issuer_url: &str) -> String {
    subjects::kv_key_token(issuer_url)
}

fn decode_acme_account(key: &str, bytes: &[u8]) -> Result<AcmeAccountRecord> {
    let record: AcmeAccountRecord = kv_json::decode_json("nats_acme_account_decode", bytes)?;
    let expected_key = acme_account_key(&record.issuer_url);
    if expected_key != key {
        return Err(Error::store_key_mismatch(
            StoreRecordKind::AcmeAccount,
            key,
            expected_key,
        ));
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::error::{Error, StoreRecordKind};
    use ployz_types::model::CertificateState;

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
    fn active_certificate_schedules_next_renewal_job() {
        let mut record = certificate("example.com");
        record.state = CertificateState::Active;
        record.next_renewal_at = Some(1_803_619_200);

        let schedule = certificate_renewal_job_schedule(&record);

        assert_eq!(schedule, Some(JobSchedule::AtUnixSecs(1_803_619_200)));
    }

    #[test]
    fn non_active_certificate_does_not_schedule_renewal_job() {
        let mut record = certificate("example.com");
        record.state = CertificateState::RenewalDue;
        record.next_renewal_at = Some(1_803_619_200);

        assert_eq!(certificate_renewal_job_schedule(&record), None);
    }

    #[test]
    fn acme_challenge_snapshot_validates_keys() {
        let record = challenge("example.com", "token");
        let key = challenge_key(&record.hostname, &record.token);
        let snapshot = acme_challenge_snapshot(vec![kv_json::JsonEntry {
            key: key.clone(),
            value: record.clone(),
        }]);

        assert_eq!(snapshot.expect("valid snapshot"), vec![record]);
    }

    #[test]
    fn acme_account_kv_key_mismatch_is_visible() {
        let record = AcmeAccountRecord {
            account_id: "account-1".into(),
            issuer_url: "https://issuer.example/acme".into(),
            contact_email: None,
            account_credentials_json: "{}".into(),
            created_at: 1,
            updated_at: 1,
        };
        let bytes = serde_json::to_vec(&record).expect("encode account");

        let error = decode_acme_account("wrong-key", &bytes).expect_err("key mismatch should fail");

        assert_eq!(
            error,
            Error::store_key_mismatch(
                StoreRecordKind::AcmeAccount,
                "wrong-key",
                acme_account_key(&record.issuer_url)
            )
        );
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
