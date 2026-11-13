//! What the observe tests build their Docker and Log Store files from.

use bollard::models::{ContainerConfig, ContainerInspectResponse};

use super::frame::{Piece, Stream, tests::frame};

pub(super) const T0: i64 = 1_760_000_000_000_000_000;

pub(super) fn lines(first: i64, count: i64) -> Vec<u8> {
    (first..first + count)
        .flat_map(|ts| frame(ts, Stream::Stdout, b"line", Piece::Whole))
        .collect()
}

pub(super) fn torn(ts: i64) -> Vec<u8> {
    frame(ts, Stream::Stdout, &[b'x'; 300], Piece::Whole)
        .get(..120)
        .unwrap()
        .to_vec()
}

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
