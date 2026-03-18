use ployz_config::{HostPathsContext, Os, RuntimeTarget, ServiceMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostPlatform {
    pub os: Os,
    pub is_root: bool,
}

impl HostPlatform {
    #[must_use]
    pub fn detect() -> Self {
        Self {
            os: detect_os(),
            is_root: current_user_is_root(),
        }
    }

    #[must_use]
    pub fn paths_context(self) -> HostPathsContext {
        HostPathsContext {
            os: self.os,
            is_root: self.is_root,
        }
    }
}

pub fn validate_runtime(
    runtime_target: RuntimeTarget,
    service_mode: ServiceMode,
    platform: HostPlatform,
) -> Result<(), String> {
    match (runtime_target, service_mode, platform.os) {
        (RuntimeTarget::Docker, ServiceMode::System, _) => {
            Err("system service mode requires host runtime".into())
        }
        (RuntimeTarget::Host, ServiceMode::System, Os::Linux) => Ok(()),
        (RuntimeTarget::Host, ServiceMode::System, Os::Darwin | Os::Other) => {
            Err("system service mode requires Linux host runtime".into())
        }
        (RuntimeTarget::Host, ServiceMode::User, Os::Other) => {
            Err("host runtime is not supported on this platform".into())
        }
        (RuntimeTarget::Host, ServiceMode::User, Os::Linux | Os::Darwin) => Ok(()),
        (RuntimeTarget::Docker, ServiceMode::User, _) => Ok(()),
    }
}

fn detect_os() -> Os {
    if cfg!(target_os = "linux") {
        Os::Linux
    } else if cfg!(target_os = "macos") {
        Os::Darwin
    } else {
        Os::Other
    }
}

fn current_user_is_root() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: `geteuid` has no Rust-side preconditions.
        unsafe { libc::geteuid() == 0 }
    }

    #[cfg(not(unix))]
    {
        false
    }
}
