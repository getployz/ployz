use std::collections::HashMap;
use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::spec::{ContainerSpec, MountSource, Namespace, VolumeDeclaration};

use super::{ShellRunner, ZfsDriver};

pub async fn resolve_volumes<R: ShellRunner>(
    driver: &ZfsDriver<R>,
    namespace: &Namespace,
    container: &ContainerSpec,
    declarations: &HashMap<String, VolumeDeclaration>,
) -> Result<HashMap<String, PathBuf>> {
    let mut resolved = HashMap::new();
    for mount in &container.mounts {
        let MountSource::Volume(name) = &mount.source else {
            continue;
        };
        if resolved.contains_key(name) {
            continue;
        }
        let declaration = declarations.get(name).ok_or_else(|| {
            Error::operation(
                "resolve_volumes",
                format!("volume '{name}' was not declared in manifest"),
            )
        })?;
        let dataset = format!(
            "{}/{}/{}",
            driver.root_dataset(),
            namespace.as_str(),
            declaration.name
        );
        let mountpoint = driver
            .root_mountpoint()
            .join(&namespace.as_str())
            .join(&declaration.name);
        let info = driver
            .ensure(&super::DatasetSpec {
                dataset,
                mountpoint,
                quota: declaration.quota.to_string(),
                mode: declaration.mode.to_string(),
                owner: declaration.owner.to_string(),
            })
            .await?;
        resolved.insert(name.clone(), info.mountpoint);
    }
    Ok(resolved)
}
