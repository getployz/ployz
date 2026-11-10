use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub(crate) struct ConfigDir<'a>(&'a Path);

impl<'a> ConfigDir<'a> {
    pub(crate) fn new(config: &'a Path) -> Self {
        Self(config)
    }

    pub(crate) fn mcp_token(self) -> PathBuf {
        self.0.with_file_name("mcp.json")
    }

    /// The Cloud sign-in.
    pub(crate) fn cloud(self) -> PathBuf {
        self.0.with_file_name("cloud.json")
    }

    pub(crate) fn device_key(self) -> PathBuf {
        self.0.with_file_name("device.key")
    }

    /// The sealing key of the in-process Config Store.
    pub(crate) fn store_key(self) -> PathBuf {
        self.0.with_file_name("store.key")
    }

    /// The links from directories to Projects and Environments.
    pub(crate) fn links(self) -> PathBuf {
        self.0.with_file_name("links.json")
    }

    /// The cache of published Cloud facts.
    pub(crate) fn published(self) -> PathBuf {
        self.0.with_file_name("published")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_file_sits_beside_the_config_under_its_name() {
        let config = Path::new("/home/ada/.ployz/config.toml");
        let dir = ConfigDir::new(config);
        let files = [
            (dir.mcp_token(), "mcp.json"),
            (dir.cloud(), "cloud.json"),
            (dir.device_key(), "device.key"),
            (dir.store_key(), "store.key"),
            (dir.links(), "links.json"),
            (dir.published(), "published"),
        ];
        for (path, name) in files {
            assert_eq!(path, Path::new("/home/ada/.ployz").join(name));
        }
    }
}
