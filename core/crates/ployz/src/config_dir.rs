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

    /// The Cluster Sealing public keys this computer pinned, by Organization.
    pub(crate) fn sealing_keys(self) -> PathBuf {
        self.0.with_file_name("sealing-keys.json")
    }

    /// The Servers this computer pinned for activation, by Machine.
    pub(crate) fn known_machines(self) -> PathBuf {
        self.0.with_file_name("known-machines.json")
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
            (dir.sealing_keys(), "sealing-keys.json"),
            (dir.known_machines(), "known-machines.json"),
            (dir.links(), "links.json"),
            (dir.published(), "published"),
        ];
        for (path, name) in files {
            assert_eq!(path, Path::new("/home/ada/.ployz").join(name));
        }
    }
}
