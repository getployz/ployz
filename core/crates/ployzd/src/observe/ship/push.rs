//! The Machine's signed requests to Cloud: the push target, the signature, and the salts fetch.

use std::{
    io,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ployz_core::{MachineId, separation};
use reqwest::{RequestBuilder, Url, header::CONTENT_TYPE};
use sha2::{Digest, Sha256};

use crate::{machine::store, management::ManagementSecret};

const TIMESTAMP_HEADER: &str = "Ployz-Timestamp";
const SIGNATURE_HEADER: &str = "Ployz-Signature";

pub struct Pusher {
    url: Url,
    machine_id: MachineId,
    secret: ManagementSecret,
}

impl Pusher {
    pub(in crate::observe) async fn analytics_salts(
        &self,
        client: &reqwest::Client,
    ) -> io::Result<Vec<u8>> {
        let url = format!(
            "{}/api/machines/{}/analytics-salts",
            self.url.origin().ascii_serialization(),
            self.machine_id
        );
        let response = authenticate(client.post(url), self, unix_seconds(), b"{}".to_vec())
            .send()
            .await
            .map_err(io::Error::other)?;
        let status = response.status();
        if !status.is_success() {
            return Err(io::Error::other(format!("Cloud answered {status}")));
        }
        Ok(response.bytes().await.map_err(io::Error::other)?.to_vec())
    }
}

/// Read from machine.json before every send, so Cloud setting or clearing
/// the target and a Machine reset take effect without a restart. None until
/// Cloud sets a target, so the outbox only buffers.
pub fn push_target(data_dir: &Path) -> Option<Pusher> {
    let identity = store::identity(data_dir)?;
    Some(Pusher {
        url: identity.push_target?,
        machine_id: identity.id,
        secret: identity.management_secret,
    })
}

/// Signs the exact body bytes with the Management Identity's key; Cloud
/// verifies against the identity it stored at enrollment.
fn authenticate(
    request: RequestBuilder,
    pusher: &Pusher,
    ts: i64,
    body: Vec<u8>,
) -> RequestBuilder {
    let signature = signature(&pusher.secret, &pusher.machine_id, ts, &body);
    request
        .header(CONTENT_TYPE, "application/json")
        .header(TIMESTAMP_HEADER, ts.to_string())
        .header(SIGNATURE_HEADER, format!("v1={signature}"))
        .body(body)
}

fn signature(secret: &ManagementSecret, machine_id: &MachineId, ts: i64, body: &[u8]) -> String {
    let mut message = separation::PUSH_SIGNATURE.to_vec();
    message.extend_from_slice(machine_id.as_str().as_bytes());
    message.push(0);
    message.extend_from_slice(&ts.to_be_bytes());
    message.extend_from_slice(&Sha256::digest(body));
    URL_SAFE_NO_PAD.encode(secret.sign(&message))
}

pub(in crate::observe) fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
#[path = "push_tests.rs"]
mod tests;
