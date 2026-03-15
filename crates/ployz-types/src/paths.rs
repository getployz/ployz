use std::path::{Path, PathBuf};

/// Path to a network's directory: `<data_dir>/networks/<name>/`
#[must_use]
pub fn network_dir(data_dir: &Path, name: &str) -> PathBuf {
    data_dir.join("networks").join(name)
}

/// Path to a network's config file: `<data_dir>/networks/<name>/network.json`
#[must_use]
pub fn network_config_path(data_dir: &Path, name: &str) -> PathBuf {
    network_dir(data_dir, name).join("network.json")
}

/// Read the active network name from `<data_dir>/active_network`.
#[must_use]
pub fn read_active_network(data_dir: &Path) -> Option<String> {
    std::fs::read_to_string(data_dir.join("active_network"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
