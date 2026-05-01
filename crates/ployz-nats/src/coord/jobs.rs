use std::time::Duration;

use crate::subjects;

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

    #[test]
    fn renewal_publish_has_stable_dedupe_id() {
        let first = CertRenewalJob::new("Api.Example.Com");
        let second = CertRenewalJob::new("api.example.com");
        assert_eq!(first.message_id, second.message_id);
        assert_eq!(first.subject, second.subject);
    }
}
