use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::DeserializeOwned;

pub const MAX_OPERATION_ID_LEN: usize = 128;

#[derive(Debug, Clone)]
pub struct FileOperationStore {
    root: PathBuf,
    dir_name: &'static str,
    label: &'static str,
}

impl FileOperationStore {
    #[must_use]
    pub fn new(root: PathBuf, dir_name: &'static str, label: &'static str) -> Self {
        Self {
            root,
            dir_name,
            label,
        }
    }

    pub fn save<T>(&self, id: &str, record: &T) -> Result<(), String>
    where
        T: Serialize,
    {
        validate_operation_id(self.label, id)?;
        let path = self.path_for(id);
        if let Some(parent) = path.parent() {
            let dir_label = self.dir_label();
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("create {dir_label} dir '{}': {err}", parent.display()))?;
        }
        let body = serde_json::to_vec_pretty(record)
            .map_err(|err| format!("encode {} '{}': {err}", self.label, id))?;
        std::fs::write(&path, body)
            .map_err(|err| format!("write {} '{}': {err}", self.label, path.display()))
    }

    pub fn load<T>(&self, id: &str) -> Result<Option<T>, String>
    where
        T: DeserializeOwned,
    {
        validate_operation_id(self.label, id)?;
        let path = self.path_for(id);
        match read_operation(&path, self.label) {
            Ok(record) => Ok(Some(record)),
            Err(ReadOperationError::NotFound) => Ok(None),
            Err(ReadOperationError::Other(message)) => Err(message),
        }
    }

    pub fn list<T, StartedAt, Id>(&self, started_at: StartedAt, id: Id) -> Result<Vec<T>, String>
    where
        T: DeserializeOwned,
        StartedAt: Fn(&T) -> u64,
        Id: Fn(&T) -> &str,
    {
        let dir = self.dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                let dir_label = self.dir_label();
                return Err(format!("read {dir_label} dir '{}': {err}", dir.display()));
            }
        };

        let dir_label = self.dir_label();
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| format!("read {dir_label} entry: {err}"))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            match read_operation(&path, self.label) {
                Ok(record) => records.push(record),
                Err(ReadOperationError::NotFound) => continue,
                Err(ReadOperationError::Other(message)) => return Err(message),
            }
        }
        records.sort_by(|left, right| {
            started_at(right)
                .cmp(&started_at(left))
                .then(id(left).cmp(id(right)))
        });
        Ok(records)
    }

    #[must_use]
    pub fn dir(&self) -> PathBuf {
        self.root.join(self.dir_name)
    }

    fn dir_label(&self) -> String {
        self.dir_name.replace('-', " ")
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.dir().join(format!("{id}.json"))
    }
}

pub fn unique_operation_id(kind: &str, operation: impl fmt::Display, now: u64) -> String {
    try_unique_operation_id(kind, operation, now).expect("time after epoch")
}

pub fn try_unique_operation_id(
    kind: &str,
    operation: impl fmt::Display,
    now: u64,
) -> Result<String, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("system clock is before UNIX epoch: {err}"))?
        .as_nanos();
    Ok(format!("{kind}-{operation}-{now}-{nanos}"))
}

pub fn validate_operation_id(label: &str, id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err(format!("{label} id cannot be empty"));
    }
    if id.len() > MAX_OPERATION_ID_LEN {
        return Err(format!(
            "{label} id exceeds {MAX_OPERATION_ID_LEN} characters"
        ));
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(format!(
            "{label} id '{id}' contains characters outside [A-Za-z0-9_-]"
        ));
    }
    Ok(())
}

enum ReadOperationError {
    NotFound,
    Other(String),
}

fn read_operation<T>(path: &Path, label: &str) -> Result<T, ReadOperationError>
where
    T: DeserializeOwned,
{
    let body = match std::fs::read(path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ReadOperationError::NotFound);
        }
        Err(error) => {
            return Err(ReadOperationError::Other(format!(
                "read {label} '{}': {error}",
                path.display()
            )));
        }
    };
    serde_json::from_slice(&body).map_err(|err| {
        ReadOperationError::Other(format!("decode {label} '{}': {err}", path.display()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TestOperation {
        id: String,
        started_at: u64,
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}-{sequence}", std::process::id()))
    }

    #[test]
    fn validate_operation_id_rejects_unsafe_paths() {
        assert!(validate_operation_id("test operation", "").is_err());
        assert!(validate_operation_id("test operation", "../etc/passwd").is_err());
        assert!(validate_operation_id("test operation", "/etc/passwd").is_err());
        assert!(validate_operation_id("test operation", "a/b").is_err());
        assert!(validate_operation_id("test operation", "with space").is_err());
        assert!(
            validate_operation_id("test operation", &"a".repeat(MAX_OPERATION_ID_LEN + 1)).is_err()
        );
    }

    #[test]
    fn generated_ids_use_kind_operation_time_and_nanos() {
        let id = unique_operation_id("image", "push", 42);

        assert!(id.starts_with("image-push-42-"));
        assert!(validate_operation_id("image operation", &id).is_ok());
    }

    #[test]
    fn save_load_and_list_operations() {
        let store = FileOperationStore::new(
            unique_temp_dir("ployz-file-operation-store-test"),
            "operations",
            "test operation",
        );
        let first = TestOperation {
            id: "first".to_string(),
            started_at: 1,
        };
        let second = TestOperation {
            id: "second".to_string(),
            started_at: 2,
        };

        store.save(&first.id, &first).expect("save first operation");
        store
            .save(&second.id, &second)
            .expect("save second operation");

        assert_eq!(
            store
                .load::<TestOperation>(&first.id)
                .expect("load operation"),
            Some(first.clone())
        );
        assert_eq!(
            store
                .list::<TestOperation, _, _>(|record| record.started_at, |record| &record.id)
                .expect("list operations"),
            vec![second, first]
        );
    }

    #[test]
    fn malformed_json_reports_operation_path_context() {
        let store = FileOperationStore::new(
            unique_temp_dir("ployz-file-operation-store-malformed"),
            "operations",
            "test operation",
        );
        std::fs::create_dir_all(store.dir()).expect("operation dir");
        let path = store.dir().join("broken.json");
        std::fs::write(&path, b"{not json").expect("write broken operation");

        let err = store
            .load::<TestOperation>("broken")
            .expect_err("malformed operation should fail visibly");

        assert!(err.contains("decode test operation"), "got: {err}");
        assert!(
            err.contains(path.to_str().expect("utf-8 path")),
            "got: {err}"
        );
    }
}
