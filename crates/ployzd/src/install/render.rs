use std::fs;
use std::path::Path;

pub(super) fn write_client_config(path: &Path, data_dir: &Path, socket_path: &str) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err(format!("invalid config path '{}'", path.display()));
    };
    fs::create_dir_all(parent)
        .map_err(|error| format!("create config dir '{}': {error}", parent.display()))?;
    let body = format!(
        "data_dir = {}\nsocket = {}\n",
        toml_string(&data_dir.display().to_string()),
        toml_string(socket_path),
    );
    fs::write(path, body).map_err(|error| format!("write config '{}': {error}", path.display()))
}

pub(super) fn systemd_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

pub(super) fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn toml_string(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    )
}
