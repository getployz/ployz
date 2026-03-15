use serde::Deserialize;
use std::fs;
use std::path::Path;

const EMBEDDED_MANIFEST: &str = include_str!("../assets/built_in_images.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltInImage {
    Networking,
    Corrosion,
    Dns,
    Gateway,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BuiltInImages {
    pub networking: String,
    pub corrosion: String,
    pub dns: String,
    pub gateway: String,
}

#[derive(Debug, Deserialize)]
struct PartialBuiltInImages {
    networking: Option<String>,
    corrosion: Option<String>,
    dns: Option<String>,
    gateway: Option<String>,
}

impl BuiltInImages {
    pub fn load(manifest_path: Option<&Path>) -> Result<Self, String> {
        let mut images = parse_embedded_manifest()?;
        if let Some(path) = manifest_path {
            let raw = fs::read_to_string(path).map_err(|error| {
                format!(
                    "read built-in images manifest '{}': {error}",
                    path.display()
                )
            })?;
            let partial = toml::from_str::<PartialBuiltInImages>(&raw).map_err(|error| {
                format!(
                    "parse built-in images manifest '{}': {error}",
                    path.display()
                )
            })?;
            images.apply(partial);
        }
        Ok(images)
    }

    #[must_use]
    pub fn resolve(&self, image: BuiltInImage) -> &str {
        match image {
            BuiltInImage::Networking => &self.networking,
            BuiltInImage::Corrosion => &self.corrosion,
            BuiltInImage::Dns => &self.dns,
            BuiltInImage::Gateway => &self.gateway,
        }
    }

    fn apply(&mut self, partial: PartialBuiltInImages) {
        let PartialBuiltInImages {
            networking,
            corrosion,
            dns,
            gateway,
        } = partial;

        if let Some(networking) = networking {
            self.networking = networking;
        }
        if let Some(corrosion) = corrosion {
            self.corrosion = corrosion;
        }
        if let Some(dns) = dns {
            self.dns = dns;
        }
        if let Some(gateway) = gateway {
            self.gateway = gateway;
        }
    }
}

fn parse_embedded_manifest() -> Result<BuiltInImages, String> {
    toml::from_str(EMBEDDED_MANIFEST)
        .map_err(|error| format!("parse embedded built-in images manifest: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{BuiltInImage, BuiltInImages};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_manifest_path() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("ployz-builtins-{timestamp}.toml"))
    }

    #[test]
    fn embedded_manifest_resolves_every_image() {
        let images = BuiltInImages::load(None).expect("embedded manifest should parse");

        assert!(images.resolve(BuiltInImage::Networking).contains("sha256:"));
        assert!(images.resolve(BuiltInImage::Corrosion).contains("sha256:"));
        assert!(images.resolve(BuiltInImage::Dns).contains("sha256:"));
        assert!(images.resolve(BuiltInImage::Gateway).contains("sha256:"));
    }

    #[test]
    fn partial_override_manifest_merges_into_embedded_defaults() {
        let path = temp_manifest_path();
        fs::write(
            &path,
            "networking = \"ployz-dev/ployz-networking:test\"\ndns = \"ployz-dev/ployz-dns:test\"\n",
        )
        .expect("write temp manifest");

        let images = BuiltInImages::load(Some(&path)).expect("override manifest should parse");
        fs::remove_file(&path).ok();

        assert_eq!(
            images.resolve(BuiltInImage::Networking),
            "ployz-dev/ployz-networking:test"
        );
        assert_eq!(
            images.resolve(BuiltInImage::Dns),
            "ployz-dev/ployz-dns:test"
        );
        assert!(images.resolve(BuiltInImage::Corrosion).contains("sha256:"));
        assert!(images.resolve(BuiltInImage::Gateway).contains("sha256:"));
    }

    #[test]
    fn invalid_override_manifest_fails_clearly() {
        let path = temp_manifest_path();
        fs::write(&path, "networking = [").expect("write invalid temp manifest");

        let error = BuiltInImages::load(Some(&path)).expect_err("invalid manifest should fail");
        fs::remove_file(&path).ok();

        assert!(error.contains("parse built-in images manifest"));
    }
}
