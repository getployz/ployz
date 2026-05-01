use std::time::Duration;

use async_nats::HeaderMap;
use async_nats::header::{NATS_EXPECTED_STREAM, NATS_MESSAGE_ID};
use async_nats::jetstream;
use async_nats::jetstream::message::PublishMessage;
use chrono::{DateTime, Utc};
use ployz_types::error::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::subjects;
use crate::subjects::CERT_JOBS_STREAM;

pub const NATS_SCHEDULE: &str = "Nats-Schedule";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertRenewalJob {
    pub hostname: String,
    pub subject: String,
    pub message_id: String,
}

impl CertRenewalJob {
    #[must_use]
    pub fn new(hostname: impl Into<String>) -> Self {
        let hostname = hostname.into().to_ascii_lowercase();
        Self {
            subject: subjects::cert_renewal_job(&hostname),
            message_id: format!("cert-renewal:{hostname}"),
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
        let job = CertRenewalJob::new(hostname);
        let mut headers = HeaderMap::new();
        headers.insert(NATS_EXPECTED_STREAM, CERT_JOBS_STREAM);
        headers.insert(NATS_MESSAGE_ID, job.message_id.as_str());
        if let Some(schedule) = schedule_header_value(schedule)? {
            headers.insert(NATS_SCHEDULE, schedule);
        }
        let payload = serde_json::to_vec(&CertRenewalJobPayload {
            hostname: job.hostname,
        })
        .map_err(|error| Error::operation("nats_cert_job_encode", error.to_string()))?;
        Ok(Self {
            subject: job.subject,
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
    let spec = CertRenewalPublishSpec::build(hostname, schedule)?;
    let ack = js
        .send_publish(spec.subject.clone(), spec.publish_message())
        .await
        .map_err(|error| Error::operation("nats_cert_job_publish", format!("{error:?}")))?;
    ack.await
        .map(|_| ())
        .map_err(|error| Error::operation("nats_cert_job_ack", format!("{error:?}")))
}

fn schedule_header_value(schedule: JobSchedule) -> Result<Option<String>> {
    match schedule {
        JobSchedule::Immediate => Ok(None),
        JobSchedule::Cron(expression) => Ok(Some(format!("cron {expression}"))),
        JobSchedule::AtUnixSecs(timestamp) => {
            let timestamp = i64::try_from(timestamp)
                .map_err(|error| Error::operation("nats_cert_job_schedule", error.to_string()))?;
            let Some(at) = DateTime::<Utc>::from_timestamp(timestamp, 0) else {
                return Err(Error::operation(
                    "nats_cert_job_schedule",
                    format!("invalid unix timestamp {timestamp}"),
                ));
            };
            Ok(Some(at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)))
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
    fn renewal_publish_has_stable_dedupe_id() {
        let first = CertRenewalJob::new("Api.Example.Com");
        let second = CertRenewalJob::new("api.example.com");
        assert_eq!(first.message_id, second.message_id);
        assert_eq!(first.subject, second.subject);
    }

    #[test]
    fn renewal_publish_spec_sets_workqueue_headers_and_payload() {
        let spec = CertRenewalPublishSpec::build(
            "Api.Example.Com",
            JobSchedule::AtUnixSecs(1_803_619_200),
        )
        .expect("build renewal job");

        assert_eq!(spec.subject, "cert.jobs.renew.api%2Eexample%2Ecom");
        assert_eq!(
            header(&spec.headers, NATS_EXPECTED_STREAM),
            CERT_JOBS_STREAM
        );
        assert_eq!(
            header(&spec.headers, NATS_MESSAGE_ID),
            "cert-renewal:api.example.com"
        );
        assert_eq!(header(&spec.headers, NATS_SCHEDULE), "2027-02-26T05:20:00Z");
        let payload: CertRenewalJobPayload =
            serde_json::from_slice(&spec.payload).expect("decode payload");
        assert_eq!(payload.hostname, "api.example.com");
    }

    #[test]
    fn immediate_renewal_publish_spec_has_no_schedule_header() {
        let spec = CertRenewalPublishSpec::build("api.example.com", JobSchedule::Immediate)
            .expect("build renewal job");

        assert!(spec.headers.get(NATS_SCHEDULE).is_none());
    }

    #[test]
    fn cron_renewal_publish_spec_prefixes_schedule_expression() {
        let spec =
            CertRenewalPublishSpec::build("api.example.com", JobSchedule::Cron("0 3 * * *".into()))
                .expect("build renewal job");

        assert_eq!(header(&spec.headers, NATS_SCHEDULE), "cron 0 3 * * *");
    }

    fn header(headers: &HeaderMap, name: impl IntoHeaderName) -> String {
        headers
            .get(name)
            .map(|value| value.as_str().to_string())
            .unwrap_or_default()
    }
}
