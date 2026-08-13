use std::{fs, path::PathBuf};

pub struct TestDir(pub PathBuf);

impl TestDir {
    pub fn new(prefix: &str) -> Self {
        Self(std::env::temp_dir().join(format!("{prefix}-{}", ployz_core::MachineId::random())))
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
