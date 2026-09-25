//! Execution-host resource ceilings and administration of retained builder cache.

use crate::{Admission, BuildError, Docker, HostPolicy, Streams, builder::Builder};
use serde::Deserialize;
use std::{collections::BTreeMap, fs, path::Path};

/// Without a configured GC target, keep this share of the Docker root free.
const DEFAULT_MIN_FREE_PERCENT: u64 = 20;

/// Cache cap when none is configured. BuildKit evicts one record whenever a
/// free-space target alone is already met (its keep target equals the cache
/// size, which is not "under" it); any cap above the cache size avoids that.
/// 2^62 bytes survives Buildx's float byte parsing exactly.
const UNCAPPED_CACHE_BYTES: u64 = 1 << 62;

/// Parsed only from the execution host, never from captured source or requests.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Resources {
    cpu_cores: Option<f64>,
    memory_bytes: Option<u64>,
    cache_bytes: Option<u64>,
    min_free_bytes: Option<u64>,
}

impl Resources {
    pub(crate) fn load(path: &Path) -> Result<Self, BuildError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(BuildError::Prerequisite(format!(
                    "read {}: {error}",
                    path.display()
                )));
            }
        };
        Self::parse(&bytes)
    }

    fn parse(bytes: &[u8]) -> Result<Self, BuildError> {
        let policy: Self = serde_norway::from_slice(bytes).map_err(|error| {
            BuildError::Prerequisite(format!("invalid host build.yaml: {error}"))
        })?;
        if policy
            .cpu_cores
            .is_some_and(|cores| !cores.is_finite() || !(0.01..=1_000_000.0).contains(&cores))
        {
            return Err(BuildError::Prerequisite(
                "build cpu_cores must be finite and between 0.01 and 1000000".into(),
            ));
        }
        if policy
            .memory_bytes
            .is_some_and(|bytes| !(6 * 1024 * 1024..=i64::MAX as u64).contains(&bytes))
        {
            return Err(BuildError::Prerequisite(
                "build memory_bytes must be at least 6291456 and fit a signed 64-bit byte count"
                    .into(),
            ));
        }
        for bytes in [policy.cache_bytes, policy.min_free_bytes]
            .into_iter()
            .flatten()
        {
            if bytes == 0 || bytes > i64::MAX as u64 {
                return Err(BuildError::Prerequisite(
                    "build GC targets must be positive signed 64-bit byte counts".into(),
                ));
            }
        }
        Ok(policy)
    }

    pub(crate) fn check_support(&self, info: &serde_json::Value) -> Result<(), BuildError> {
        for (required, capabilities) in [
            (self.cpu_cores.is_some(), ["CpuCfsPeriod", "CpuCfsQuota"]),
            (self.memory_bytes.is_some(), ["MemoryLimit", "SwapLimit"]),
        ] {
            if required
                && capabilities
                    .iter()
                    .any(|name| info.get(name).and_then(serde_json::Value::as_bool) != Some(true))
            {
                return Err(BuildError::Prerequisite(format!(
                    "Docker did not report support for configured build limits: {}",
                    capabilities.join(", ")
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn worker_arguments(&self) -> Vec<String> {
        let mut arguments = Vec::new();
        if let Some(cores) = self.cpu_cores {
            arguments.extend([
                "--driver-opt".into(),
                "cpu-period=100000".into(),
                "--driver-opt".into(),
                format!("cpu-quota={:.0}", cores * 100_000.0),
            ]);
        }
        if let Some(bytes) = self.memory_bytes {
            arguments.extend([
                "--driver-opt".into(),
                format!("memory={bytes}"),
                "--driver-opt".into(),
                format!("memory-swap={bytes}"),
            ]);
        }
        arguments
    }

    pub(crate) fn preparation_arguments(&self) -> Vec<String> {
        let mut arguments = Vec::new();
        if let Some(cores) = self.cpu_cores {
            arguments.extend([
                "--cpu-period".into(),
                "100000".into(),
                "--cpu-quota".into(),
                format!("{:.0}", cores * 100_000.0),
            ]);
        }
        if let Some(bytes) = self.memory_bytes {
            arguments.extend([
                "--memory".into(),
                bytes.to_string(),
                "--memory-swap".into(),
                bytes.to_string(),
            ]);
        }
        arguments
    }

    pub(crate) fn collect_cache(
        &self,
        docker: &Docker<'_>,
        builder: &str,
    ) -> Result<(), BuildError> {
        let min_free_bytes = match (self.cache_bytes, self.min_free_bytes) {
            // An unreadable Docker root (remote endpoint, non-root user) skips this
            // prune; the worker's percentage policy still applies on its own GC.
            (None, None) => match docker_root_total_bytes(docker) {
                Some(total) => Some(total / 100 * DEFAULT_MIN_FREE_PERCENT),
                None => return Ok(()),
            },
            (_, min_free_bytes) => min_free_bytes,
        };
        let arguments = self.prune_arguments(builder, min_free_bytes);
        docker
            .run(
                "collect retained Ployz build cache",
                &arguments.iter().map(String::as_str).collect::<Vec<_>>(),
                Streams::Captured,
            )
            .map(|_| ())
    }

    /// Always capped: with only a free-space target that is already met,
    /// BuildKit still evicts one record (see `UNCAPPED_CACHE_BYTES`).
    fn prune_arguments(&self, builder: &str, min_free_bytes: Option<u64>) -> Vec<String> {
        let mut arguments = vec![
            "buildx".into(),
            "prune".into(),
            "--builder".into(),
            builder.into(),
            "--all".into(),
            "--force".into(),
            "--reserved-space".into(),
            "0".into(),
            "--max-used-space".into(),
            self.max_used_bytes().to_string(),
        ];
        if let Some(bytes) = min_free_bytes {
            arguments.extend(["--min-free-space".into(), bytes.to_string()]);
        }
        arguments
    }

    fn max_used_bytes(&self) -> u64 {
        self.cache_bytes.unwrap_or(UNCAPPED_CACHE_BYTES)
    }

    pub(crate) fn buildkit_config(&self) -> String {
        // BuildKit's default reservedSpace can exceed a small host budget.
        // Give its GC permission to reclaim down to zero; it owns all eviction.
        let mut config = "[worker.oci]\ngc = true\nreservedSpace = 0\n".to_owned();
        if self.cache_bytes.is_none() && self.min_free_bytes.is_none() {
            config.push_str(&format!("minFreeSpace = \"{DEFAULT_MIN_FREE_PERCENT}%\"\n"));
        }
        config.push_str(&format!("maxUsedSpace = {}\n", self.max_used_bytes()));
        if let Some(bytes) = self.min_free_bytes {
            config.push_str(&format!("minFreeSpace = {bytes}\n"));
        }
        config
    }
}

fn docker_root_total_bytes(docker: &Docker<'_>) -> Option<u64> {
    let root = docker
        .run(
            "inspect the Docker root",
            &["info", "--format", "{{.DockerRootDir}}"],
            Streams::Captured,
        )
        .ok()?;
    let stat = rustix::fs::statvfs(root.trim()).ok()?;
    Some(stat.f_blocks.saturating_mul(stat.f_frsize))
}

/// Clear only this host user's Ployz-owned BuildKit cache. Run on the execution
/// host as the same user as its Builds; no daemon is needed for local use.
/// Completed Docker images and unrelated Docker data are preserved.
///
/// # Errors
/// Refuses active or quarantined ownership, invalid host configuration, and
/// reports upstream launch/prune errors or unconfirmed cleanup.
pub fn clear_cache(policy: &HostPolicy) -> Result<(), BuildError> {
    // Every build slot keeps its own cache; stop at the first refusal.
    crate::builder::Lock::known_slots(&policy.state_directory)
        .try_for_each(|slot| clear_slot_cache(policy, slot))
}

fn clear_slot_cache(policy: &HostPolicy, slot: usize) -> Result<(), BuildError> {
    let admission = Admission::try_acquire_slot(policy, slot)?;
    let environment: BTreeMap<String, String> = std::env::vars().collect();
    // Private command output must not collide with a concurrent caller or a Build.
    let directory = policy
        .state_directory
        .join(format!("cache-clear-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory)
        .map_err(|error| BuildError::Prerequisite(format!("stage cache clearing: {error}")))?;
    let docker = Docker {
        program: &policy.docker,
        environment: &environment,
        working_dir: &directory,
        deadline: admission.deadline,
        cancellation: Some(&admission.cancellation),
        progress: None,
    };
    let result = (|| {
        docker.require_local()?;
        let builder = Builder::acquire(&docker, admission.lock, &admission.resources)?;
        let result = docker
            .run(
                "start the builder for cache clearing",
                &["buildx", "inspect", builder.name(), "--bootstrap"],
                Streams::Captured,
            )
            .and_then(|_| {
                docker
                    .run(
                        "clear Ployz build cache",
                        &[
                            "buildx",
                            "prune",
                            "--builder",
                            builder.name(),
                            "--all",
                            "--force",
                        ],
                        Streams::Captured,
                    )
                    .map(|_| ())
            });
        builder.finish(result)
    })();
    if !result.as_ref().is_err_and(BuildError::is_unknown) {
        let _ = fs::remove_dir_all(directory);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_configuration_rejects_invalid_limits_and_defaults_gc_to_a_fifth_free() {
        let defaults = Resources::parse(b"{}").unwrap();
        assert!(defaults.worker_arguments().is_empty());
        assert!(defaults.preparation_arguments().is_empty());
        assert_eq!(
            defaults.buildkit_config(),
            "[worker.oci]\ngc = true\nreservedSpace = 0\nminFreeSpace = \"20%\"\nmaxUsedSpace = 4611686018427387904\n"
        );
        assert_eq!(
            Resources::parse(b"cache_bytes: 1073741824")
                .unwrap()
                .buildkit_config(),
            "[worker.oci]\ngc = true\nreservedSpace = 0\nmaxUsedSpace = 1073741824\n"
        );
        for invalid in [
            "cpu_cores: 0",
            "cpu_cores: .nan",
            "cpu_cores: .inf",
            "cpu_cores: -1",
            "memory_bytes: 0",
            "memory_bytes: -1",
            "cache_bytes: 0",
            "min_free_bytes: 18446744073709551615",
            "concurrency: 2",
        ] {
            assert!(Resources::parse(invalid.as_bytes()).is_err(), "{invalid}");
        }
        assert!(Resources::parse(b"cpu_cores: 0.5\nmemory_bytes: 536870912\ncache_bytes: 1073741824\nmin_free_bytes: 2147483648").is_ok());
    }

    #[test]
    fn cache_collection_always_caps_used_space_so_a_met_free_target_evicts_nothing() {
        let max_used = |yaml: &[u8]| {
            let arguments = Resources::parse(yaml)
                .unwrap()
                .prune_arguments("ployz-0", Some(1));
            arguments
                .iter()
                .skip_while(|argument| *argument != "--max-used-space")
                .nth(1)
                .expect("prune always passes --max-used-space")
                .clone()
        };
        assert_eq!(max_used(b"{}"), "4611686018427387904");
        assert_eq!(max_used(b"min_free_bytes: 1"), "4611686018427387904");
        assert_eq!(max_used(b"cache_bytes: 1073741824"), "1073741824");
    }
}
