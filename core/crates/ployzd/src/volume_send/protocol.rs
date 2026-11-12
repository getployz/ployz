//! The bound route of the send port, as the writer serves it and the puller asks it.
//!
//! The send port is spoken between daemons of different releases during a rolling
//! upgrade, so the path and the body below stay as they are. The stream route's request
//! is [`ployz_core::SendStream`].

use ployz_core::{DockerVolumeName, ManagementAddress};

/// The path on the writer Machine's send port that answers a Volume's bound in bytes.
pub const BOUND_PATH_PREFIX: &str = "/volume-bound/";

/// The Volume a request path to the bound route names, or `None` for any other path.
#[must_use]
pub fn bound_volume(path: &str) -> Option<&str> {
    path.strip_prefix(BOUND_PATH_PREFIX)
}

/// The URL on which `writer` answers the bound of `name`.
#[must_use]
pub fn bound_url(writer: ManagementAddress, port: u16, name: &DockerVolumeName) -> String {
    format!("http://[{}]:{port}{BOUND_PATH_PREFIX}{name}", writer.0)
}

/// The bound a writer's answer states, in bytes.
///
/// # Errors
///
/// Returns when the body is not a byte count.
pub fn parse_bound(body: &str) -> Result<u64, std::num::ParseIntError> {
    body.trim().parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bound_route_keeps_the_path_an_older_daemon_serves() {
        let name = DockerVolumeName::parse("data").unwrap();
        let writer = ManagementAddress("fd00::7".parse().unwrap());
        assert_eq!(
            bound_url(writer, 7007, &name),
            "http://[fd00::7]:7007/volume-bound/data"
        );
        assert_eq!(bound_volume("/volume-bound/data"), Some("data"));
        assert_eq!(bound_volume("/volume-send/data"), None);
    }

    #[test]
    fn a_bound_is_a_byte_count_with_or_without_a_trailing_newline() {
        assert_eq!(parse_bound("1073741824"), Ok(1_073_741_824));
        assert_eq!(parse_bound("1073741824\n"), Ok(1_073_741_824));
        assert!(parse_bound("1 GiB").is_err());
    }
}
