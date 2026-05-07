use std::time::Duration;

use async_nats::HeaderMap;
use async_nats::header::{NATS_EXPECTED_STREAM, NATS_MESSAGE_ID};
use async_nats::jetstream;
use async_nats::jetstream::AckKind;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy, ReplayPolicy, pull};
use async_nats::jetstream::message::PublishMessage;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use ployz_types::error::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::subjects;
use crate::subjects::{CERT_JOBS_STREAM, NatsScope};

pub const NATS_SCHEDULE: &str = "Nats-Schedule";
pub const NATS_SCHEDULE_TARGET: &str = "Nats-Schedule-Target";
pub const CERT_RENEWAL_CONSUMER: &str = "ployzd_cert_renewal";
pub const CERT_RENEWAL_FETCH_TIMEOUT: Duration = Duration::from_secs(30);
pub const CERT_RENEWAL_MAX_ACK_PENDING: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertRenewalJob {
    pub hostname: String,
    pub renewal_subject: String,
    pub schedule_subject: String,
}

impl CertRenewalJob {
    #[must_use]
    pub fn new(hostname: impl Into<String>) -> Self {
        Self::new_in(&NatsScope::default(), hostname)
    }

    #[must_use]
    pub fn new_in(scope: &NatsScope, hostname: impl Into<String>) -> Self {
        let hostname = hostname.into().to_ascii_lowercase();
        Self {
            renewal_subject: subjects::cert_renewal_job_in(scope, &hostname),
            schedule_subject: subjects::cert_renewal_schedule_in(scope, &hostname),
            hostname,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertRenewalJobPayload {
    pub hostname: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobSchedule {
    Immediate,
    AtUnixSecs(u64),
    Cron(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertRenewalPublishSpec {
    pub subject: String,
    pub headers: HeaderMap,
    pub payload: Vec<u8>,
}

impl CertRenewalPublishSpec {
    pub fn build(hostname: impl Into<String>, schedule: JobSchedule) -> Result<Self> {
        Self::build_in(&NatsScope::default(), hostname, schedule)
    }

    pub fn build_in(
        scope: &NatsScope,
        hostname: impl Into<String>,
        schedule: JobSchedule,
    ) -> Result<Self> {
        let job = CertRenewalJob::new_in(scope, hostname);
        let mut headers = HeaderMap::new();
        headers.insert(NATS_EXPECTED_STREAM, CERT_JOBS_STREAM);
        let message_id = cert_renewal_message_id(&job.hostname, &schedule);
        headers.insert(NATS_MESSAGE_ID, message_id.as_str());
        let subject = match schedule_header_value(&schedule)? {
            Some(schedule) => {
                headers.insert(NATS_SCHEDULE, schedule);
                headers.insert(NATS_SCHEDULE_TARGET, job.renewal_subject.as_str());
                job.schedule_subject
            }
            None => job.renewal_subject,
        };
        let payload = serde_json::to_vec(&CertRenewalJobPayload {
            hostname: job.hostname,
        })
        .map_err(|error| Error::operation("nats_cert_job_encode", error.to_string()))?;
        Ok(Self {
            subject,
            headers,
            payload,
        })
    }

    #[must_use]
    pub fn publish_message(&self) -> PublishMessage {
        PublishMessage::build()
            .payload(self.payload.clone().into())
            .headers(self.headers.clone())
            .expected_stream(CERT_JOBS_STREAM)
    }
}

pub async fn publish_cert_renewal_job(
    js: &jetstream::Context,
    hostname: impl Into<String>,
    schedule: JobSchedule,
) -> Result<()> {
    publish_cert_renewal_job_in(js, &NatsScope::default(), hostname, schedule).await
}

pub async fn publish_cert_renewal_job_in(
    js: &jetstream::Context,
    scope: &NatsScope,
    hostname: impl Into<String>,
    schedule: JobSchedule,
) -> Result<()> {
    let spec = CertRenewalPublishSpec::build_in(scope, hostname, schedule)?;
    let ack = js
        .send_publish(spec.subject.clone(), spec.publish_message())
        .await
        .map_err(|error| Error::operation("nats_cert_job_publish", format!("{error:?}")))?;
    ack.await
        .map(|_| ())
        .map_err(|error| Error::operation("nats_cert_job_ack", format!("{error:?}")))
}

pub fn cert_renewal_consumer_config(policy: WorkQueuePolicy) -> Result<pull::Config> {
    cert_renewal_consumer_config_in(&NatsScope::default(), policy)
}

pub fn cert_renewal_consumer_config_in(
    scope: &NatsScope,
    policy: WorkQueuePolicy,
) -> Result<pull::Config> {
    let max_deliver = i64::try_from(policy.max_deliver)
        .map_err(|error| Error::operation("nats_cert_job_consumer_config", error.to_string()))?;
    Ok(pull::Config {
        durable_name: Some(CERT_RENEWAL_CONSUMER.to_string()),
        name: Some(CERT_RENEWAL_CONSUMER.to_string()),
        description: Some("ployz certificate renewal work queue".to_string()),
        deliver_policy: DeliverPolicy::All,
        ack_policy: AckPolicy::Explicit,
        ack_wait: policy.ack_wait,
        max_deliver,
        filter_subject: subjects::cert_renewal_filter_in(scope),
        replay_policy: ReplayPolicy::Instant,
        max_ack_pending: CERT_RENEWAL_MAX_ACK_PENDING,
        max_batch: CERT_RENEWAL_MAX_ACK_PENDING,
        max_expires: CERT_RENEWAL_FETCH_TIMEOUT,
        ..Default::default()
    })
}

pub struct NatsCertRenewalJobConsumer {
    consumer: jetstream::consumer::PullConsumer,
    scope: NatsScope,
    fetch_timeout: Duration,
}

impl NatsCertRenewalJobConsumer {
    pub async fn connect(js: &jetstream::Context, policy: WorkQueuePolicy) -> Result<Self> {
        Self::connect_in(js, &NatsScope::default(), policy).await
    }

    pub async fn connect_in(
        js: &jetstream::Context,
        scope: &NatsScope,
        policy: WorkQueuePolicy,
    ) -> Result<Self> {
        let stream = js
            .get_stream(CERT_JOBS_STREAM)
            .await
            .map_err(|error| Error::operation("nats_cert_job_stream", format!("{error:?}")))?;
        let consumer = stream
            .get_or_create_consumer(
                CERT_RENEWAL_CONSUMER,
                cert_renewal_consumer_config_in(scope, policy)?,
            )
            .await
            .map_err(|error| Error::operation("nats_cert_job_consumer", format!("{error:?}")))?;
        Ok(Self {
            consumer,
            scope: scope.clone(),
            fetch_timeout: CERT_RENEWAL_FETCH_TIMEOUT,
        })
    }

    pub async fn next(&self) -> Result<Option<CertRenewalJobDelivery>> {
        let mut messages = self
            .consumer
            .fetch()
            .max_messages(1)
            .expires(self.fetch_timeout)
            .messages()
            .await
            .map_err(|error| Error::operation("nats_cert_job_fetch", format!("{error:?}")))?;
        let Some(next) = messages.next().await else {
            return Ok(None);
        };
        let message =
            next.map_err(|error| Error::operation("nats_cert_job_message", format!("{error:?}")))?;
        match CertRenewalJobDelivery::from_message(&self.scope, message) {
            Ok(delivery) => Ok(Some(delivery)),
            Err(failure) => {
                let CertRenewalJobDecodeFailure { message, error } = *failure;
                if let Err(term_error) = message.ack_with(AckKind::Term).await {
                    return Err(Error::operation(
                        "nats_cert_job_term_invalid",
                        format!("{error}; term failed: {term_error:?}"),
                    ));
                }
                Err(error)
            }
        }
    }
}

#[derive(Debug)]
pub struct CertRenewalJobDelivery {
    message: jetstream::Message,
    pub subject: String,
    pub hostname: String,
}

impl CertRenewalJobDelivery {
    fn from_message(
        scope: &NatsScope,
        message: jetstream::Message,
    ) -> std::result::Result<Self, Box<CertRenewalJobDecodeFailure>> {
        match decode_cert_renewal_message_in(
            scope,
            &message.message.subject,
            &message.message.payload,
        ) {
            Ok((subject, hostname)) => Ok(Self {
                message,
                subject,
                hostname,
            }),
            Err(error) => Err(Box::new(CertRenewalJobDecodeFailure { message, error })),
        }
    }

    pub async fn ack(&self) -> Result<()> {
        self.message
            .ack()
            .await
            .map_err(|error| Error::operation("nats_cert_job_ack_delivery", format!("{error:?}")))
    }

    pub async fn nak(&self) -> Result<()> {
        self.nak_after(None).await
    }

    pub async fn nak_after(&self, delay: Option<Duration>) -> Result<()> {
        self.message
            .ack_with(AckKind::Nak(delay))
            .await
            .map_err(|error| Error::operation("nats_cert_job_nak_delivery", format!("{error:?}")))
    }

    pub async fn term(&self) -> Result<()> {
        self.message
            .ack_with(AckKind::Term)
            .await
            .map_err(|error| Error::operation("nats_cert_job_term_delivery", format!("{error:?}")))
    }
}

struct CertRenewalJobDecodeFailure {
    message: jetstream::Message,
    error: Error,
}

#[cfg(test)]
fn decode_cert_renewal_message(
    subject: &async_nats::Subject,
    payload: &[u8],
) -> Result<(String, String)> {
    decode_cert_renewal_message_in(&NatsScope::default(), subject, payload)
}

fn decode_cert_renewal_message_in(
    scope: &NatsScope,
    subject: &async_nats::Subject,
    payload: &[u8],
) -> Result<(String, String)> {
    let payload: CertRenewalJobPayload = serde_json::from_slice(payload)
        .map_err(|error| Error::operation("nats_cert_job_decode", error.to_string()))?;
    let hostname = payload.hostname.to_ascii_lowercase();
    let expected_subject = subjects::cert_renewal_job_in(scope, &hostname);
    let subject = subject.to_string();
    if subject != expected_subject {
        return Err(Error::operation(
            "nats_cert_job_subject",
            format!("subject '{subject}' did not match hostname '{hostname}'"),
        ));
    }
    Ok((subject, hostname))
}

fn cert_renewal_message_id(hostname: &str, schedule: &JobSchedule) -> String {
    let schedule_token = match schedule {
        JobSchedule::Immediate => String::from("immediate"),
        JobSchedule::AtUnixSecs(timestamp) => format!("at:{timestamp}"),
        JobSchedule::Cron(expression) => format!("cron:{}", subjects::kv_key_token(expression)),
    };
    format!("cert-renewal:{hostname}:{schedule_token}")
}

fn schedule_header_value(schedule: &JobSchedule) -> Result<Option<String>> {
    match schedule {
        JobSchedule::Immediate => Ok(None),
        JobSchedule::Cron(expression) => Ok(Some(expression.clone())),
        JobSchedule::AtUnixSecs(timestamp) => {
            let timestamp = i64::try_from(*timestamp)
                .map_err(|error| Error::operation("nats_cert_job_schedule", error.to_string()))?;
            let Some(at) = DateTime::<Utc>::from_timestamp(timestamp, 0) else {
                return Err(Error::operation(
                    "nats_cert_job_schedule",
                    format!("invalid unix timestamp {timestamp}"),
                ));
            };
            Ok(Some(format!(
                "@at {}",
                at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            )))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkQueuePolicy {
    pub ack_wait: Duration,
    pub max_deliver: usize,
    pub duplicate_window: Duration,
}

impl Default for WorkQueuePolicy {
    fn default() -> Self {
        Self {
            ack_wait: Duration::from_secs(10 * 60),
            max_deliver: 5,
            duplicate_window: Duration::from_secs(60 * 60),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_nats::header::IntoHeaderName;

    #[test]
    fn renewal_publish_dedupe_id_is_normalized_and_schedule_scoped() {
        let first = CertRenewalJob::new("Api.Example.Com");
        let second = CertRenewalJob::new("api.example.com");
        assert_eq!(first.renewal_subject, second.renewal_subject);
        assert_eq!(first.schedule_subject, second.schedule_subject);

        assert_eq!(
            cert_renewal_message_id(&first.hostname, &JobSchedule::AtUnixSecs(1_803_619_200)),
            cert_renewal_message_id(&second.hostname, &JobSchedule::AtUnixSecs(1_803_619_200))
        );
        assert_ne!(
            cert_renewal_message_id(&first.hostname, &JobSchedule::AtUnixSecs(1_803_619_200)),
            cert_renewal_message_id(&second.hostname, &JobSchedule::AtUnixSecs(1_803_622_800))
        );
        assert_ne!(
            cert_renewal_message_id(&first.hostname, &JobSchedule::Immediate),
            cert_renewal_message_id(&second.hostname, &JobSchedule::AtUnixSecs(1_803_619_200))
        );
    }

    #[test]
    fn renewal_publish_spec_sets_workqueue_headers_and_payload() {
        let spec = CertRenewalPublishSpec::build(
            "Api.Example.Com",
            JobSchedule::AtUnixSecs(1_803_619_200),
        )
        .expect("build renewal job");

        assert_eq!(
            spec.subject,
            "ployz.v1.local.auth-default.work.cert.schedule.api%2Eexample%2Ecom"
        );
        assert_eq!(
            header(&spec.headers, NATS_EXPECTED_STREAM),
            CERT_JOBS_STREAM
        );
        assert_eq!(
            header(&spec.headers, NATS_MESSAGE_ID),
            "cert-renewal:api.example.com:at:1803619200"
        );
        assert_eq!(
            header(&spec.headers, NATS_SCHEDULE),
            "@at 2027-02-26T05:20:00Z"
        );
        assert_eq!(
            header(&spec.headers, NATS_SCHEDULE_TARGET),
            "ployz.v1.local.auth-default.work.cert.renew.api%2Eexample%2Ecom"
        );
        let payload: CertRenewalJobPayload =
            serde_json::from_slice(&spec.payload).expect("decode payload");
        assert_eq!(payload.hostname, "api.example.com");
    }

    #[test]
    fn immediate_renewal_publish_spec_has_no_schedule_header() {
        let spec = CertRenewalPublishSpec::build("api.example.com", JobSchedule::Immediate)
            .expect("build renewal job");

        assert!(spec.headers.get(NATS_SCHEDULE).is_none());
        assert!(spec.headers.get(NATS_SCHEDULE_TARGET).is_none());
        assert_eq!(
            header(&spec.headers, NATS_MESSAGE_ID),
            "cert-renewal:api.example.com:immediate"
        );
        assert_eq!(
            spec.subject,
            "ployz.v1.local.auth-default.work.cert.renew.api%2Eexample%2Ecom"
        );
    }

    #[test]
    fn cron_renewal_publish_spec_uses_schedule_expression() {
        let spec =
            CertRenewalPublishSpec::build("api.example.com", JobSchedule::Cron("0 3 * * *".into()))
                .expect("build renewal job");

        assert_eq!(header(&spec.headers, NATS_SCHEDULE), "0 3 * * *");
        assert_eq!(
            header(&spec.headers, NATS_MESSAGE_ID),
            cert_renewal_message_id("api.example.com", &JobSchedule::Cron("0 3 * * *".into()))
        );
        assert_eq!(
            header(&spec.headers, NATS_SCHEDULE_TARGET),
            "ployz.v1.local.auth-default.work.cert.renew.api%2Eexample%2Ecom"
        );
        assert_eq!(
            spec.subject,
            "ployz.v1.local.auth-default.work.cert.schedule.api%2Eexample%2Ecom"
        );
    }

    #[test]
    fn renewal_consumer_config_is_durable_workqueue_pull_consumer() {
        let config =
            cert_renewal_consumer_config(WorkQueuePolicy::default()).expect("consumer config");

        assert_eq!(config.durable_name.as_deref(), Some(CERT_RENEWAL_CONSUMER));
        assert_eq!(config.name.as_deref(), Some(CERT_RENEWAL_CONSUMER));
        assert_eq!(config.ack_policy, AckPolicy::Explicit);
        assert_eq!(config.deliver_policy, DeliverPolicy::All);
        assert_eq!(
            config.filter_subject,
            "ployz.v1.local.auth-default.work.cert.renew.>"
        );
        assert_eq!(config.max_ack_pending, 1);
        assert_eq!(config.max_batch, 1);
    }

    #[test]
    fn renewal_consumer_config_uses_scope() {
        let scope = NatsScope::new(
            ployz_types::model::InstallationId("inst-acme".into()),
            ployz_types::model::AuthorityId("auth-sin".into()),
        );
        let config = cert_renewal_consumer_config_in(&scope, WorkQueuePolicy::default())
            .expect("consumer config");

        assert_eq!(
            config.filter_subject,
            "ployz.v1.inst-acme.auth-sin.work.cert.renew.>"
        );
    }

    #[test]
    fn renewal_job_decode_accepts_matching_payload_and_subject() {
        let subject: async_nats::Subject =
            "ployz.v1.local.auth-default.work.cert.renew.api%2Eexample%2Ecom"
                .to_string()
                .into();
        let payload = serde_json::to_vec(&CertRenewalJobPayload {
            hostname: "Api.Example.Com".to_string(),
        })
        .expect("payload");

        let (actual_subject, hostname) =
            decode_cert_renewal_message(&subject, &payload).expect("decode job");

        assert_eq!(actual_subject, subject.to_string());
        assert_eq!(hostname, "api.example.com");
    }

    #[test]
    fn renewal_job_decode_accepts_scoped_subject() {
        let scope = NatsScope::new(
            ployz_types::model::InstallationId("inst-acme".into()),
            ployz_types::model::AuthorityId("auth-sin".into()),
        );
        let subject: async_nats::Subject =
            "ployz.v1.inst-acme.auth-sin.work.cert.renew.api%2Eexample%2Ecom"
                .to_string()
                .into();
        let payload = serde_json::to_vec(&CertRenewalJobPayload {
            hostname: "Api.Example.Com".to_string(),
        })
        .expect("payload");

        let (actual_subject, hostname) =
            decode_cert_renewal_message_in(&scope, &subject, &payload).expect("decode job");

        assert_eq!(actual_subject, subject.to_string());
        assert_eq!(hostname, "api.example.com");
    }

    #[test]
    fn renewal_job_decode_rejects_subject_payload_mismatch() {
        let subject: async_nats::Subject =
            "ployz.v1.local.auth-default.work.cert.renew.other%2Eexample%2Ecom"
                .to_string()
                .into();
        let payload = serde_json::to_vec(&CertRenewalJobPayload {
            hostname: "api.example.com".to_string(),
        })
        .expect("payload");

        let error = decode_cert_renewal_message(&subject, &payload).expect_err("decode fails");

        assert!(error.to_string().contains(
            "subject 'ployz.v1.local.auth-default.work.cert.renew.other%2Eexample%2Ecom'"
        ));
    }

    fn header(headers: &HeaderMap, name: impl IntoHeaderName) -> String {
        headers
            .get(name)
            .map(|value| value.as_str().to_string())
            .unwrap_or_default()
    }
}
