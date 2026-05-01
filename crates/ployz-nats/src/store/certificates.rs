use async_nats::jetstream::kv;
use futures_util::StreamExt;
use ployz_store_api::{AcmeChallengeSubscription, CertificateStore, CertificateSubscription};
use ployz_types::error::Result;
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeEvent, AcmeChallengeRecord, CertificateEvent, CertificateRecord,
};
use tokio::sync::mpsc;
use tracing::warn;

use crate::NatsStore;
use crate::buckets::{ACME_ACCOUNTS_BUCKET, ACME_CHALLENGES_BUCKET, CERTIFICATES_BUCKET};
use crate::store::kv_json;

impl CertificateStore for NatsStore {
    async fn get_acme_account(&self, issuer_url: &str) -> Result<Option<AcmeAccountRecord>> {
        let bucket = kv_json::get_bucket(
            self.jetstream(),
            ACME_ACCOUNTS_BUCKET,
            "nats_acme_accounts_bucket",
        )
        .await?;
        let Some(bytes) = bucket.get(issuer_url).await.map_err(|error| {
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
            &record.issuer_url,
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
        .await
    }

    async fn subscribe_certificates(&self) -> Result<CertificateSubscription> {
        let snapshot = self.list_certificates().await?;
        let bucket = certificates_bucket(self).await?;
        let mut watch = bucket.watch_all().await.map_err(|error| {
            ployz_types::Error::operation("nats_certificates_watch", format!("{error:?}"))
        })?;
        let (tx, rx) = mpsc::channel(128);
        tokio::spawn(async move {
            while let Some(next) = watch.next().await {
                let entry = match next {
                    Ok(entry) => entry,
                    Err(error) => {
                        warn!(?error, "NATS certificate watcher failed");
                        break;
                    }
                };
                let event = match entry.operation {
                    kv::Operation::Put => {
                        match kv_json::decode_json::<CertificateRecord>(
                            "nats_certificate_decode",
                            entry.value.as_ref(),
                        ) {
                            Ok(record) => CertificateEvent::Updated(record),
                            Err(error) => {
                                warn!(?error, key = %entry.key, "NATS certificate event decode failed");
                                continue;
                            }
                        }
                    }
                    kv::Operation::Delete | kv::Operation::Purge => continue,
                };
                if tx.send(event).await.is_err() {
                    break;
                }
            }
        });
        Ok((snapshot, rx))
    }

    async fn subscribe_acme_challenges(&self) -> Result<AcmeChallengeSubscription> {
        let snapshot = self.list_acme_challenges().await?;
        let bucket = challenges_bucket(self).await?;
        let mut watch = bucket.watch_all().await.map_err(|error| {
            ployz_types::Error::operation("nats_acme_challenges_watch", format!("{error:?}"))
        })?;
        let (tx, rx) = mpsc::channel(128);
        tokio::spawn(async move {
            while let Some(next) = watch.next().await {
                let entry = match next {
                    Ok(entry) => entry,
                    Err(error) => {
                        warn!(?error, "NATS ACME challenge watcher failed");
                        break;
                    }
                };
                let event = match entry.operation {
                    kv::Operation::Put => match kv_json::decode_json::<AcmeChallengeRecord>(
                        "nats_acme_challenge_decode",
                        entry.value.as_ref(),
                    ) {
                        Ok(record) => AcmeChallengeEvent::Updated(record),
                        Err(error) => {
                            warn!(?error, key = %entry.key, "NATS ACME challenge event decode failed");
                            continue;
                        }
                    },
                    kv::Operation::Delete | kv::Operation::Purge => {
                        let Some((hostname, token)) = split_challenge_key(&entry.key) else {
                            continue;
                        };
                        AcmeChallengeEvent::Removed(AcmeChallengeRecord {
                            hostname,
                            token,
                            key_authorization: String::new(),
                            expires_at: 0,
                            created_at: 0,
                        })
                    }
                };
                if tx.send(event).await.is_err() {
                    break;
                }
            }
        });
        Ok((snapshot, rx))
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

fn certificate_key(hostname: &str) -> String {
    hostname.trim_end_matches('.').to_ascii_lowercase()
}

fn challenge_key(hostname: &str, token: &str) -> String {
    format!("{}.{}", certificate_key(hostname), token)
}

fn split_challenge_key(key: &str) -> Option<(String, String)> {
    let (hostname, token) = key.rsplit_once('.')?;
    Some((hostname.to_string(), token.to_string()))
}
