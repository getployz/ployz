//! What the observe tests build their Docker inspect answers from.

use bollard::models::{ContainerConfig, ContainerInspectResponse};

pub(super) const T0: i64 = 1_760_000_000_000_000_000;

pub(super) fn labelled(labels: &[(&str, &str)]) -> ContainerInspectResponse {
    ContainerInspectResponse {
        name: Some("/replica".to_owned()),
        config: Some(ContainerConfig {
            labels: Some(
                labels
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect(),
            ),
            ..ContainerConfig::default()
        }),
        ..ContainerInspectResponse::default()
    }
}
