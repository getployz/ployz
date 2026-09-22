//! Bounded result selection over Docker's native growing tails.
use std::collections::BTreeMap;

use futures_util::StreamExt;
use ployz_core::LogsOptions;
use tonic::Status;

use super::ContainerRuntime;
use crate::logs::RawLogEntry;

pub(super) async fn read_history(
    runtime: &ContainerRuntime,
    container: &str,
    before: i64,
    limit: i32,
) -> Result<Vec<RawLogEntry>, Status> {
    let mut tail = limit.saturating_mul(2);
    loop {
        let options = LogsOptions {
            follow: false,
            tail,
            since_unix_seconds: None,
            until_unix_seconds: None,
        };
        let mut source = runtime.raw_logs(container, &options)?;
        let mut selected = HistoryWindow::new(
            before,
            usize::try_from(limit).expect("validated positive limit"),
        );
        let mut count = 0;
        let mut first_timestamp = None;
        while let Some(entry) = source.next().await {
            let entry = entry.map_err(|error| Status::unavailable(error.to_string()))?;
            first_timestamp.get_or_insert(entry.timestamp_unix_nanos);
            count += 1;
            selected.push(entry)?;
        }
        // Include every record at the page boundary. Re-read if Docker may have
        // cut that timestamp group at the beginning of its tail.
        let complete = count < tail
            || (selected.older_count()
                >= usize::try_from(limit).expect("validated positive limit")
                && first_timestamp != selected.oldest());
        if complete {
            return Ok(selected.into_entries());
        }
        tail = tail.checked_mul(2).ok_or_else(|| {
            Status::resource_exhausted("Docker history is too large for one read")
        })?;
    }
}

struct HistoryWindow {
    before: i64,
    limit: usize,
    count: usize,
    bytes: usize,
    rows: BTreeMap<i64, Vec<RawLogEntry>>,
}
impl HistoryWindow {
    fn new(before: i64, limit: usize) -> Self {
        Self {
            before,
            limit,
            count: 0,
            bytes: 0,
            rows: BTreeMap::new(),
        }
    }
    #[expect(
        clippy::result_large_err,
        reason = "preserve the RPC tonic status without another error adapter"
    )]
    fn push(&mut self, row: RawLogEntry) -> Result<(), Status> {
        if row.timestamp_unix_nanos > self.before {
            return Ok(());
        }
        self.bytes += row.message.len();
        self.count += usize::from(row.timestamp_unix_nanos < self.before);
        self.rows
            .entry(row.timestamp_unix_nanos)
            .or_default()
            .push(row);
        while let Some((&timestamp, entries)) = self.rows.first_key_value() {
            if timestamp == self.before || self.count.saturating_sub(entries.len()) < self.limit {
                break;
            }
            self.bytes -= entries
                .iter()
                .map(|entry| entry.message.len())
                .sum::<usize>();
            self.count -= entries.len();
            self.rows.pop_first();
        }
        if self.bytes > 16 * 1024 * 1024 {
            return Err(Status::resource_exhausted(
                "History timestamp group exceeds 16 MiB",
            ));
        }
        Ok(())
    }
    fn older_count(&self) -> usize {
        self.count
    }
    fn oldest(&self) -> Option<i64> {
        self.rows.first_key_value().map(|(timestamp, _)| *timestamp)
    }
    fn into_entries(self) -> Vec<RawLogEntry> {
        self.rows.into_values().flatten().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::LogLineStream;
    fn row(time: i64, message: &str) -> RawLogEntry {
        RawLogEntry {
            timestamp_unix_nanos: time,
            stream: LogLineStream::Stdout,
            message: message.as_bytes().to_vec(),
        }
    }
    #[test]
    fn older_page_keeps_boundary_duplicates_and_orders_clock_regressions() {
        let mut page = HistoryWindow::new(10, 2);
        for entry in [
            row(1, "old"),
            row(10, "boundary"),
            row(9, "same"),
            row(11, "new"),
            row(8, "older"),
            row(9, "same"),
        ] {
            page.push(entry).unwrap();
        }
        let rows = page.into_entries();
        assert_eq!(
            rows.iter()
                .map(|row| row.timestamp_unix_nanos)
                .collect::<Vec<_>>(),
            [9, 9, 10]
        );
        assert_eq!(rows.first().unwrap().message, rows.get(1).unwrap().message);
    }
}
