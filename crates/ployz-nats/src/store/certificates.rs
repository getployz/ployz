use async_nats::jetstream::kv;
use futures_util::TryStreamExt;
use ployz_store_api::{AcmeChallengeSubscription, CertificateStore, CertificateSubscription};
use ployz_types::error::{Error, Result};
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeEvent, AcmeChallengeReadinessRecord, AcmeChallengeRecord,
    CertificateEvent, CertificateRecord, CertificateState, MachineId,
};
use std::collections::HashMap;

use crate::NatsStore;
use crate::buckets::{
    ACME_ACCOUNTS_BUCKET, ACME_CHALLENGE_READINESS_BUCKET, ACME_CHALLENGES_BUCKET,
    CERTIFICATES_BUCKET,
};
use crate::coord::jobs::{JobSchedule, publish_cert_renewal_job_in};
use crate::store::kv_json;
use crate::store::kv_watch;
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
        Ok(Some(decode_acme_account(
            &acme_account_key(issuer_url),
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
        schedule_certificate_renewal(self, record).await?;
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
        let snapshot_boundary =
            kv_json::latest_sequence(&bucket, "nats_certificates_snapshot_boundary").await?;
        let snapshot_entries = kv_json::list_json_entries::<CertificateRecord>(
            &bucket,
            "nats_certificate_decode",
            "nats_certificates_list",
        )
        .await?;
        let snapshot_revisions = snapshot_entries
            .iter()
            .map(|entry| (entry.key.clone(), entry.revision))
            .collect::<HashMap<_, _>>();
        let mut snapshot = snapshot_entries
            .into_iter()
            .map(|entry| validate_certificate_key(&entry.key, entry.value))
            .collect::<Result<Vec<_>>>()?;
        sort_certificates(&mut snapshot);
        kv_watch::subscribe_all_with_snapshot_revisions(
            &bucket,
            snapshot,
            snapshot_revisions,
            snapshot_boundary,
            |record: &CertificateRecord| certificate_key(&record.hostname),
            "nats_certificates_watch",
            "NATS certificate watcher failed",
            "NATS certificate event decode failed",
            certificate_event_from_kv_entry,
        )
        .await
    }

    async fn subscribe_acme_challenges(&self) -> Result<AcmeChallengeSubscription> {
        let bucket = challenges_bucket(self).await?;
        let snapshot_boundary =
            kv_json::latest_sequence(&bucket, "nats_acme_challenges_snapshot_boundary").await?;
        let snapshot_entries = kv_json::list_json_entries::<AcmeChallengeRecord>(
            &bucket,
            "nats_acme_challenge_decode",
            "nats_acme_challenges_list",
        )
        .await?;
        let (snapshot, snapshot_revisions) = acme_challenge_snapshot_parts(snapshot_entries);
        let snapshot = snapshot?;
        kv_watch::subscribe_all_with_snapshot_revisions(
            &bucket,
            snapshot,
            snapshot_revisions,
            snapshot_boundary,
            |record: &AcmeChallengeRecord| challenge_key(&record.hostname, &record.token),
            "nats_acme_challenges_watch",
            "NATS ACME challenge watcher failed",
            "NATS ACME challenge event decode failed",
            challenge_event_from_kv_entry,
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

async fn schedule_certificate_renewal(store: &NatsStore, record: &CertificateRecord) -> Result<()> {
    let Some(schedule) = certificate_renewal_job_schedule(record) else {
        return Ok(());
    };
    publish_cert_renewal_job_in(store.jetstream(), store.scope(), &record.hostname, schedule).await
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

async fn list_certificates(bucket: &kv::Store) -> Result<Vec<CertificateRecord>> {
    let mut records = kv_json::list_json_entries::<CertificateRecord>(
        bucket,
        "nats_certificate_decode",
        "nats_certificates_list",
    )
    .await?
    .into_iter()
    .map(|entry| validate_certificate_key(&entry.key, entry.value))
    .collect::<Result<Vec<_>>>()?;
    sort_certificates(&mut records);
    Ok(records)
}

fn decode_certificate(key: &str, bytes: &[u8]) -> Result<CertificateRecord> {
    let record: CertificateRecord = kv_json::decode_json("nats_certificate_decode", bytes)?;
    validate_certificate_key(key, record)
}

fn validate_certificate_key(key: &str, record: CertificateRecord) -> Result<CertificateRecord> {
    let expected_key = certificate_key(&record.hostname);
    if expected_key != key {
        return Err(Error::operation(
            "nats_certificate_decode",
            format!("certificate key {key} does not match payload key {expected_key}"),
        ));
    }
    Ok(record)
}

async fn list_challenges(bucket: &kv::Store) -> Result<Vec<AcmeChallengeRecord>> {
    let mut records = kv_json::list_json_entries::<AcmeChallengeRecord>(
        bucket,
        "nats_acme_challenge_decode",
        "nats_acme_challenges_list",
    )
    .await?
    .into_iter()
    .map(|entry| validate_challenge_key(&entry.key, entry.value))
    .collect::<Result<Vec<_>>>()?;
    sort_acme_challenges(&mut records);
    Ok(records)
}

fn validate_challenge_key(key: &str, record: AcmeChallengeRecord) -> Result<AcmeChallengeRecord> {
    let expected_key = challenge_key(&record.hostname, &record.token);
    if expected_key != key {
        return Err(Error::operation(
            "nats_acme_challenge_decode",
            format!("ACME challenge key {key} does not match payload key {expected_key}"),
        ));
    }
    Ok(record)
}

async fn list_readiness(bucket: &kv::Store) -> Result<Vec<AcmeChallengeReadinessRecord>> {
    let mut records = kv_json::list_json_entries::<AcmeChallengeReadinessRecord>(
        bucket,
        "nats_acme_readiness_decode",
        "nats_acme_readiness_list",
    )
    .await?
    .into_iter()
    .map(|entry| validate_readiness_key(&entry.key, entry.value))
    .collect::<Result<Vec<_>>>()?;
    sort_acme_challenge_readiness(&mut records);
    Ok(records)
}

fn validate_readiness_key(
    key: &str,
    record: AcmeChallengeReadinessRecord,
) -> Result<AcmeChallengeReadinessRecord> {
    let expected_key = readiness_key(&record.hostname, &record.token, &record.machine_id);
    if expected_key != key {
        return Err(Error::operation(
            "nats_acme_readiness_decode",
            format!("ACME readiness key {key} does not match payload key {expected_key}"),
        ));
    }
    Ok(record)
}

fn sort_certificates(records: &mut [CertificateRecord]) {
    records.sort_by(|left, right| {
        certificate_key(&left.hostname).cmp(&certificate_key(&right.hostname))
    });
}

fn sort_acme_challenges(records: &mut [AcmeChallengeRecord]) {
    records.sort_by(|left, right| {
        challenge_key(&left.hostname, &left.token)
            .cmp(&challenge_key(&right.hostname, &right.token))
    });
}

fn sort_acme_challenge_readiness(records: &mut [AcmeChallengeReadinessRecord]) {
    records.sort_by(|left, right| {
        readiness_key(&left.hostname, &left.token, &left.machine_id).cmp(&readiness_key(
            &right.hostname,
            &right.token,
            &right.machine_id,
        ))
    });
}

fn certificate_event_from_kv_entry(
    last_seen: &mut HashMap<String, CertificateRecord>,
    key: &str,
    bytes: &[u8],
    operation: kv::Operation,
) -> Result<Option<CertificateEvent>> {
    match operation {
        kv::Operation::Put => {
            let record = decode_certificate(key, bytes)?;
            let event = match last_seen.get(key) {
                Some(existing) if existing == &record => return Ok(None),
                Some(_) => CertificateEvent::Updated(record.clone()),
                None => CertificateEvent::Added(record.clone()),
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
            let record = validate_challenge_key(key, record)?;
            let event = match last_seen.get(key) {
                Some(existing) if existing == &record => return Ok(None),
                Some(_) => AcmeChallengeEvent::Updated(record.clone()),
                None => AcmeChallengeEvent::Added(record.clone()),
            };
            last_seen.insert(key.to_string(), record);
            Ok(Some(event))
        }
        kv::Operation::Delete | kv::Operation::Purge => {
            Ok(last_seen.remove(key).map(AcmeChallengeEvent::Removed))
        }
    }
}

fn acme_challenge_snapshot_parts(
    entries: Vec<kv_json::JsonEntry<AcmeChallengeRecord>>,
) -> (Result<Vec<AcmeChallengeRecord>>, HashMap<String, u64>) {
    let snapshot_revisions = entries
        .iter()
        .map(|entry| (entry.key.clone(), entry.revision))
        .collect::<HashMap<_, _>>();
    let mut snapshot = entries
        .into_iter()
        .map(|entry| validate_challenge_key(&entry.key, entry.value))
        .collect::<Result<Vec<_>>>();
    if let Ok(records) = snapshot.as_mut() {
        sort_acme_challenges(records);
    }
    (snapshot, snapshot_revisions)
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

fn decode_acme_account(key: &str, bytes: &[u8]) -> Result<AcmeAccountRecord> {
    let record: AcmeAccountRecord = kv_json::decode_json("nats_acme_account_decode", bytes)?;
    let expected_key = acme_account_key(&record.issuer_url);
    if expected_key != key {
        return Err(Error::operation(
            "nats_acme_account_decode",
            format!("ACME account key {key} does not match payload key {expected_key}"),
        ));
    }
    Ok(record)
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
    fn certificate_store_rows_sort_by_contract_identity() {
        let mut certificates = vec![certificate("z.example.com"), certificate("a.example.com")];
        sort_certificates(&mut certificates);
        assert_eq!(
            certificates
                .iter()
                .map(|record| record.hostname.as_str())
                .collect::<Vec<_>>(),
            ["a.example.com", "z.example.com"]
        );

        let mut challenges = vec![
            challenge("z.example.com", "token-b"),
            challenge("a.example.com", "token-a"),
        ];
        sort_acme_challenges(&mut challenges);
        assert_eq!(
            challenges
                .iter()
                .map(|record| (record.hostname.as_str(), record.token.as_str()))
                .collect::<Vec<_>>(),
            [("a.example.com", "token-a"), ("z.example.com", "token-b")]
        );

        let mut readiness = vec![
            readiness("example.com", "token", "machine-b"),
            readiness("example.com", "token", "machine-a"),
        ];
        sort_acme_challenge_readiness(&mut readiness);
        assert_eq!(
            readiness
                .iter()
                .map(|record| record.machine_id.0.as_str())
                .collect::<Vec<_>>(),
            ["machine-a", "machine-b"]
        );
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
    fn certificate_kv_put_ignores_identical_value() {
        let record = certificate("example.com");
        let bytes = serde_json::to_vec(&record).expect("encode certificate");
        let mut last_seen = HashMap::from([(String::from("example.com"), record.clone())]);

        let event = certificate_event_from_kv_entry(
            &mut last_seen,
            "example.com",
            &bytes,
            kv::Operation::Put,
        )
        .expect("put should decode");

        assert!(event.is_none());
        assert_eq!(last_seen.get("example.com"), Some(&record));
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

    #[test]
    fn acme_challenge_kv_put_ignores_identical_value() {
        let record = challenge("example.com", "token");
        let key = challenge_key(&record.hostname, &record.token);
        let bytes = serde_json::to_vec(&record).expect("encode challenge");
        let mut last_seen = HashMap::from([(key.clone(), record.clone())]);

        let event = challenge_event_from_kv_entry(&mut last_seen, &key, &bytes, kv::Operation::Put)
            .expect("put should decode");

        assert!(event.is_none());
        assert_eq!(last_seen.get(&key), Some(&record));
    }

    #[test]
    fn acme_challenge_snapshot_parts_keep_entry_revisions() {
        let record = challenge("example.com", "token");
        let key = challenge_key(&record.hostname, &record.token);
        let (snapshot, revisions) = acme_challenge_snapshot_parts(vec![kv_json::JsonEntry {
            key: key.clone(),
            revision: 42,
            value: record.clone(),
        }]);

        assert_eq!(snapshot.expect("valid snapshot"), vec![record]);
        assert_eq!(revisions.get(&key), Some(&42));
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

        assert!(error.to_string().contains("wrong-key"));
        assert!(
            error
                .to_string()
                .contains(&acme_account_key(&record.issuer_url))
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

    fn readiness(hostname: &str, token: &str, machine_id: &str) -> AcmeChallengeReadinessRecord {
        AcmeChallengeReadinessRecord {
            hostname: hostname.into(),
            token: token.into(),
            machine_id: MachineId(machine_id.into()),
            observed_at: 1,
        }
    }
}
