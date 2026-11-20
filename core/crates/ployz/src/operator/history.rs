//! `ployz logs` for a Service that no longer runs: the lines Ployz Cloud kept, read a page
//! at a time from the newest. A Service that still has a container reads from Docker.

use ployz_core::ContainerId;
use serde::{Deserialize, Deserializer};

/// The most lines one `ployz logs` reads from Cloud when it asks for all of them.
pub const READ_ALL: usize = 5_000;
/// What Cloud's `limit` allows one page.
pub const PAGE: usize = 1_000;

/// Which stream a stored line came from.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum StoredStream {
    Stdout,
    Stderr,
}

/// One line Cloud kept.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredLine {
    #[serde(deserialize_with = "nanoseconds")]
    pub timestamp: i64,
    pub machine_name: String,
    pub container_id: String,
    pub service: String,
    pub stream: StoredStream,
    pub message: String,
}

impl StoredLine {
    #[must_use]
    pub fn container(&self) -> Option<ContainerId> {
        ContainerId::parse(self.container_id.as_str()).ok()
    }
}

/// One page of stored lines, newest first, and where the older page starts.
#[derive(Debug, Deserialize)]
pub struct StoredPage {
    pub lines: Vec<StoredLine>,
    pub cursor: Option<String>,
}

fn nanoseconds<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    let text = String::deserialize(deserializer)?;
    text.parse().map_err(serde::de::Error::custom)
}

/// The newest `want` lines from `since` on, oldest first, whichever order `pages` came in.
#[must_use]
pub fn window(
    mut lines: Vec<StoredLine>,
    since: Option<i64>,
    until: Option<i64>,
    want: usize,
) -> Vec<StoredLine> {
    lines.retain(|line| {
        since.is_none_or(|since| line.timestamp >= since)
            && until.is_none_or(|until| line.timestamp < until)
    });
    lines.sort_by_key(|line| std::cmp::Reverse(line.timestamp));
    lines.truncate(want);
    lines.reverse();
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::tests::{
        MACHINE_ONE, api_on_machine_one, machine_observation, running, service_selector,
    };
    use crate::operator::{LogsOptions, ServiceArg, observe_service_logs, open_service_logs};
    use futures_util::StreamExt;
    use ployz_core::{
        ContainerList, LogBody, LogEntry, LogMetadata, LogOrigin, MachineList, Namespace,
        RpcRequestBody, RpcResponse, ServiceId, ServiceName, op,
    };
    use tokio_util::sync::CancellationToken;

    /// The Docker path of `ployz logs` for a Service that still has a container: the Machine
    /// serves lines it has already masked, the CLI prints them as they come, and no history
    /// route is asked, so nothing reads them unmasked from anywhere else.
    #[tokio::test]
    async fn an_existing_containers_lines_come_back_masked_through_docker() {
        let first = machine_observation(MACHINE_ONE, "machine-1");
        let machine_id = first.machine.id;
        let machines = vec![first];
        let container = api_on_machine_one(running());
        let served = LogMetadata {
            origin: LogOrigin::Service {
                service_id: ServiceId::parse("1".repeat(32)).unwrap(),
                service_name: ServiceName::parse("api").unwrap(),
                container_id: container.container_id,
                hook: None,
            },
            machine_id,
            machine_name: machines[0].machine.name.clone(),
        };
        let (mut client, server) = crate::connect::test_support::rpc_routes(
            move |rpc: tonic::Request<ployz_core::OpaquePayload>| {
                let machines = machines.clone();
                let containers = vec![container.clone()];
                async move {
                    #[expect(
                        clippy::wildcard_enum_match_arm,
                        reason = "any other call is a retired route and must fail the test"
                    )]
                    let response = match rpc.into_inner().decode_request().unwrap().body {
                        RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                            enrollment: None,
                            machines,
                        }),
                        RpcRequestBody::ListContainers(_) => {
                            RpcResponse::from(ContainerList { containers })
                        }
                        other => panic!("`ployz logs` asked a Machine for {other:?}"),
                    };
                    Ok(tonic::Response::new(response.encode().unwrap()))
                }
            },
            &[op::TailLogs::PATH],
            move |_| {
                let served = served.clone();
                async move {
                    let frames = ["starting", "token=[secret API_TOKEN]"].map(|line| {
                        Ok(LogEntry {
                            metadata: served.clone(),
                            timestamp_unix_nanos: 1,
                            body: LogBody::Stdout(line.as_bytes().to_vec()),
                        }
                        .encode()
                        .unwrap())
                    });
                    Ok(tonic::Response::new(futures_util::stream::iter(frames)))
                }
            },
        )
        .await;

        let scope = observe_service_logs(&mut client, &[]).await.unwrap();
        let args = [ServiceArg {
            service: service_selector("app/api"),
            containers: Vec::new(),
        }];
        let options = LogsOptions {
            follow: false,
            tail: 10,
            since_nanos: None,
            until_unix_seconds: None,
        };
        let namespace = Namespace::parse("app").unwrap();
        let mut inputs = open_service_logs(
            &mut client,
            &scope,
            &args,
            Some(&namespace),
            options,
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
        let mut printed = Vec::new();
        while let Some(entry) = inputs[0].stream.next().await {
            let LogBody::Stdout(bytes) = entry.unwrap().body else {
                panic!("a masked line is stdout");
            };
            printed.push(String::from_utf8(bytes).unwrap());
        }
        server.abort();

        assert_eq!(printed, ["starting", "token=[secret API_TOKEN]"]);
    }

    fn line(timestamp: i64) -> StoredLine {
        StoredLine {
            timestamp,
            machine_name: "one".into(),
            container_id: "1".repeat(64),
            service: "api".into(),
            stream: StoredStream::Stdout,
            message: format!("line {timestamp}"),
        }
    }

    #[test]
    fn a_page_reads_as_cloud_sends_it() {
        let page: StoredPage = serde_json::from_value(serde_json::json!({
            "lines": [{
                "timestamp": "1700000000123456789",
                "machineId": "a".repeat(32),
                "machineName": "one",
                "containerId": "1".repeat(64),
                "service": "api",
                "stream": "stderr",
                "message": "boom",
            }],
            "cursor": "1700000000123456789.42",
        }))
        .unwrap();
        assert_eq!(page.lines[0].timestamp, 1_700_000_000_123_456_789);
        assert_eq!(page.lines[0].stream, StoredStream::Stderr);
        assert!(page.lines[0].container().is_some());
        assert_eq!(page.cursor.as_deref(), Some("1700000000123456789.42"));
    }

    #[test]
    fn the_newest_lines_of_a_window_print_oldest_first() {
        let lines = [5, 1, 4, 2, 3, 9].map(line).to_vec();
        let shown = |since, until, want| {
            window(lines.clone(), since, until, want)
                .iter()
                .map(|line| line.timestamp)
                .collect::<Vec<_>>()
        };
        assert_eq!(shown(None, None, 3), [4, 5, 9]);
        assert_eq!(shown(Some(2), Some(9), 10), [2, 3, 4, 5]);
    }
}
