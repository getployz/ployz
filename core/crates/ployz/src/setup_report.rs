//! Best-effort setup report for `ployz server add --token`: the Server profile and step times.
//!
//! One POST to `<cloud>/api/enroll/<token>/report` once setup succeeds or fails. It never
//! changes what the user sees: one attempt, a short timeout, every error swallowed.
//! `DO_NOT_TRACK` turns it off. It never carries hostnames, IPs, the Server name, labels,
//! Machine ids or secrets.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use ployz_core::{MachineToken, StorageChoice};
use serde::Serialize;

const TIMEOUT: Duration = Duration::from_secs(2);
const ERROR_LIMIT: usize = 1_000;
/// Cloud rejects the whole report when a profile string is longer.
const PROFILE_STRING_LIMIT: usize = 256;

/// The steps of `server add --token`, in order.
///
/// The names are an analytics contract: renaming one breaks every chart built on it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Step {
    /// Install or synchronize the daemon.
    Install,
    /// POST the identity until Cloud answers.
    Enroll,
    /// Prepare storage.
    Storage,
    /// Found or join, through the final callback.
    Join,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Profile {
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instance_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    os_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    os_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kernel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arch: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    virtualization: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cpu_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    disk_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    storage: Option<StorageChoice>,
    ployz_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    founder: Option<bool>,
}

#[derive(Serialize)]
struct StepTime {
    name: Step,
    seconds: f64,
}

#[derive(Serialize)]
#[serde(
    tag = "outcome",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum Outcome {
    Succeeded,
    Failed {
        failed_step: Step,
        failed_step_seconds: f64,
        error: String,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Body<'report> {
    #[serde(flatten)]
    outcome: Outcome,
    profile: &'report Profile,
    steps: &'report [StepTime],
    total_seconds: f64,
}

/// Times the steps of one `server add --token` run and collects the Server profile.
pub(crate) struct SetupReport {
    started: Instant,
    finished: Vec<StepTime>,
    /// The running step; `None` once the final callback succeeded.
    current: Option<(Step, Instant)>,
    profile: Profile,
}

impl SetupReport {
    /// Start timing at the `install` step.
    pub(crate) fn start() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            finished: Vec::new(),
            current: Some((Step::Install, now)),
            profile: Profile {
                ployz_version: env!("CARGO_PKG_VERSION"),
                ..Profile::default()
            },
        }
    }

    /// Finish the running step and start `step`; a no-op when `step` is already running.
    pub(crate) fn step(&mut self, step: Step) {
        if self.current.is_some_and(|(running, _)| running == step) {
            return;
        }
        self.finish_current();
        self.current = Some((step, Instant::now()));
    }

    /// The final callback succeeded: setup is done, whatever follows.
    pub(crate) fn succeeded(&mut self) {
        self.finish_current();
    }

    fn finish_current(&mut self) {
        if let Some((name, since)) = self.current.take() {
            self.finished.push(StepTime {
                name,
                seconds: since.elapsed().as_secs_f64(),
            });
        }
    }

    /// Read this host's profile; only when the CLI runs on the Server it enrolls.
    pub(crate) fn read_host(&mut self) {
        let dmi = |file: &str| read_trimmed(&Path::new("/sys/class/dmi/id").join(file));
        let (provider, instance_type) =
            provider(dmi("sys_vendor").as_deref(), dmi("product_name").as_deref());
        let os_release = read_trimmed(Path::new("/etc/os-release"))
            .or_else(|| read_trimmed(Path::new("/usr/lib/os-release")))
            .unwrap_or_default();
        let profile = &mut self.profile;
        profile.provider = Some(provider);
        profile.instance_type = instance_type;
        profile.os_id = os_release_value(&os_release, "ID");
        profile.os_version = os_release_value(&os_release, "VERSION_ID");
        profile.kernel = read_trimmed(Path::new("/proc/sys/kernel/osrelease"));
        profile.arch = Some(std::env::consts::ARCH);
        profile.cpu_count = std::thread::available_parallelism().ok().map(usize::from);
        // `systemd-detect-virt` prints `none` and exits 1 on bare metal; keep the answer.
        profile.virtualization = std::process::Command::new("systemd-detect-virt")
            .output()
            .ok()
            .and_then(|output| trimmed(String::from_utf8_lossy(&output.stdout).into_owned()));
    }

    /// Size as the daemon measured it, so it holds for a Server reached over SSH too.
    pub(crate) fn machine(&mut self, token: &MachineToken) {
        self.profile.memory_total_bytes = token.memory_total_bytes;
        self.profile.disk_total_bytes = token.disk_total_bytes;
    }

    /// What Cloud answered: the storage to prepare and whether this Server founds.
    pub(crate) fn enrolled(&mut self, storage: StorageChoice, founder: bool) {
        self.profile.storage = Some(storage);
        self.profile.founder = Some(founder);
    }

    /// POST the report for `result` to `url`, once, and ignore how that goes.
    pub(crate) async fn send<E: std::fmt::Display>(&self, url: &str, result: &Result<(), E>) {
        if do_not_track() {
            return;
        }
        let outcome = match (self.current, result) {
            (None, _) => Outcome::Succeeded,
            (Some((failed_step, since)), Err(error)) => Outcome::Failed {
                failed_step,
                failed_step_seconds: since.elapsed().as_secs_f64(),
                error: error.to_string().chars().take(ERROR_LIMIT).collect(),
            },
            // Not reached: success always passes the final callback.
            (Some(_), Ok(())) => return,
        };
        let body = Body {
            outcome,
            profile: &self.profile,
            steps: &self.finished,
            total_seconds: self.started.elapsed().as_secs_f64(),
        };
        let Ok(http) = reqwest::Client::builder().timeout(TIMEOUT).build() else {
            return;
        };
        let _ = http.post(url).json(&body).send().await;
    }
}

fn do_not_track() -> bool {
    std::env::var_os("DO_NOT_TRACK").is_some_and(|value| !value.is_empty() && value != "0")
}

fn read_trimmed(path: &Path) -> Option<String> {
    trimmed(std::fs::read_to_string(path).ok()?)
}

/// A non-empty profile string, trimmed and capped to what Cloud accepts.
fn trimmed(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.chars().take(PROFILE_STRING_LIMIT).collect())
}

fn os_release_value(os_release: &str, key: &str) -> Option<String> {
    os_release.lines().find_map(|line| {
        let value = line.strip_prefix(key)?.strip_prefix('=')?;
        trimmed(value.trim_matches(['"', '\'']).to_owned())
    })
}

/// Provider and instance type from the DMI system vendor and product name.
///
/// A known vendor maps to a short name; an unknown one is reported as is; no DMI is
/// `unknown`. The product name is the instance type only where the provider puts it there.
fn provider(vendor: Option<&str>, product: Option<&str>) -> (String, Option<String>) {
    let Some(vendor) = vendor.and_then(|vendor| trimmed(vendor.to_owned())) else {
        return ("unknown".to_owned(), None);
    };
    let lower = vendor.to_ascii_lowercase();
    let (name, has_instance_type) = if lower.contains("hetzner") {
        ("hetzner", true)
    } else if lower.contains("amazon ec2") {
        ("aws", true)
    } else if lower.contains("digitalocean") {
        ("digitalocean", false)
    } else if lower.contains("google") {
        ("google", false)
    } else if lower.contains("microsoft") {
        ("azure", false)
    } else if lower.contains("vultr") {
        ("vultr", false)
    } else if lower.contains("linode") || lower.contains("akamai") {
        ("linode", false)
    } else if lower.contains("ovh") {
        ("ovh", false)
    } else if lower.contains("oracle") {
        ("oracle", false)
    } else {
        return (vendor, None);
    };
    let instance_type = product
        .filter(|_| has_instance_type)
        .and_then(|product| trimmed(product.to_owned()));
    (name.to_owned(), instance_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dmi_maps_to_provider_and_instance_type() {
        for (vendor, product, expected, instance_type) in [
            (Some("Hetzner"), Some("vServer"), "hetzner", Some("vServer")),
            (
                Some("Amazon EC2"),
                Some("t3.micro"),
                "aws",
                Some("t3.micro"),
            ),
            (Some("DigitalOcean"), Some("Droplet"), "digitalocean", None),
            (
                Some("Google"),
                Some("Google Compute Engine"),
                "google",
                None,
            ),
            (
                Some("Microsoft Corporation"),
                Some("Virtual Machine"),
                "azure",
                None,
            ),
            (Some("Vultr"), Some("VC2"), "vultr", None),
            (Some("Linode"), Some("Compute Instance"), "linode", None),
            (Some("Akamai"), None, "linode", None),
            (Some("OVH SAS"), Some("OpenStack Nova"), "ovh", None),
            (Some("Oracle Corporation"), None, "oracle", None),
            (Some("QEMU"), Some("Standard PC"), "QEMU", None),
            (Some("  "), Some("x"), "unknown", None),
            (None, None, "unknown", None),
            (Some("Amazon EC2"), Some(" "), "aws", None),
        ] {
            assert_eq!(
                provider(vendor, product),
                (expected.to_owned(), instance_type.map(str::to_owned)),
                "{vendor:?} {product:?}"
            );
        }
    }

    #[test]
    fn profile_strings_fit_what_cloud_accepts() {
        let long = "v".repeat(1_000);
        let (vendor, _) = provider(Some(&long), None);
        assert_eq!(vendor.chars().count(), PROFILE_STRING_LIMIT);
        let (_, instance_type) = provider(Some("Hetzner"), Some(&long));
        assert_eq!(instance_type.unwrap().chars().count(), PROFILE_STRING_LIMIT);
    }

    #[test]
    fn os_release_reads_quoted_and_bare_values() {
        let os_release = "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\nID_LIKE=debian\n";
        assert_eq!(
            os_release_value(os_release, "ID").as_deref(),
            Some("ubuntu")
        );
        assert_eq!(
            os_release_value(os_release, "VERSION_ID").as_deref(),
            Some("24.04")
        );
        assert_eq!(os_release_value(os_release, "BUILD_ID"), None);
    }
}
