//! The CLI writes the capture and the Build Machine reads it, so the names are a
//! client-to-daemon contract that releases keep.

use std::path::{Path, PathBuf};

pub const SOURCE: &str = "source";
/// The directory of material that is no source: registry configuration, recipes and Sealed values.
pub const PRIVATE: &str = "private";
pub const COMPOSE: &str = "compose.yaml";
/// The Build's Sealed values, still sealed, as a file under [`PRIVATE`].
pub const SEALED_RECORD: &str = "sealed.json";
const SEALED_ARGS: &str = "sealed-args.json";
const RAILPACK_PLAN: &str = "railpack.json";
const RAILPACK_SCRATCH: &str = "railpack";
const DOCKER: &str = "docker";
const DOCKER_CONFIG: &str = "config.json";

/// The directories a capture starts with, parents first.
#[must_use]
pub fn directories(root: &Path) -> [PathBuf; 3] {
    [source(root), private(root), docker(root)]
}

#[must_use]
pub fn source(root: &Path) -> PathBuf {
    root.join(SOURCE)
}

#[must_use]
pub fn private(root: &Path) -> PathBuf {
    root.join(PRIVATE)
}

#[must_use]
pub fn compose(root: &Path) -> PathBuf {
    root.join(COMPOSE)
}

#[must_use]
pub fn private_file(root: &Path, name: &str) -> PathBuf {
    private(root).join(name)
}

/// The directory the Docker client reads its configuration from.
#[must_use]
pub fn docker(root: &Path) -> PathBuf {
    private(root).join(DOCKER)
}

#[must_use]
pub fn docker_config(root: &Path) -> PathBuf {
    docker(root).join(DOCKER_CONFIG)
}

/// The Railpack recipes the capture carries.
#[must_use]
pub fn railpack_plan(root: &Path) -> PathBuf {
    private_file(root, RAILPACK_PLAN)
}

#[must_use]
pub fn sealed_record(root: &Path) -> PathBuf {
    private_file(root, SEALED_RECORD)
}

/// The names of the Sealed values each target declares as Build arguments.
#[must_use]
pub fn sealed_args(root: &Path) -> PathBuf {
    private_file(root, SEALED_ARGS)
}

/// The scratch directory a Railpack build writes its plan and images to. It is not
/// [`railpack_plan`], which the capture carries.
#[must_use]
pub fn railpack_scratch(root: &Path) -> PathBuf {
    private_file(root, RAILPACK_SCRATCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_keeps_the_names_releases_exchange() {
        let root = Path::new("/capture");
        let relative = |path: PathBuf| {
            path.strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(
            directories(root).map(relative),
            ["source", "private", "private/docker"]
        );
        assert_eq!(relative(compose(root)), "compose.yaml");
        assert_eq!(relative(docker_config(root)), "private/docker/config.json");
        assert_eq!(relative(railpack_plan(root)), "private/railpack.json");
        assert_eq!(relative(railpack_scratch(root)), "private/railpack");
        assert_eq!(relative(sealed_record(root)), "private/sealed.json");
        assert_eq!(relative(sealed_args(root)), "private/sealed-args.json");
    }
}
