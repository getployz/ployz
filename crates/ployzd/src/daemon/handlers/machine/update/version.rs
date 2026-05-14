pub(super) fn normalize_requested_version(version: &str) -> String {
    let trimmed = version.trim();
    if trimmed == "latest" {
        return trimmed.to_string();
    }
    trimmed.strip_prefix('v').unwrap_or(trimmed).to_string()
}

pub(super) fn requested_version_matches_current(version: &str) -> bool {
    version != "latest" && normalize_requested_version(version) == env!("CARGO_PKG_VERSION")
}

pub(super) fn installer_version_argument(canonical: &str) -> String {
    if canonical == "latest" {
        canonical.to_string()
    } else {
        format!("v{canonical}")
    }
}
