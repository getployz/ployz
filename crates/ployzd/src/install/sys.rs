use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) fn home_dir_for_current_user() -> Result<PathBuf, String> {
    if let Some(home) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(home));
    }
    Err("HOME is not set".into())
}

pub(super) fn sudo_user_home_dir() -> Result<Option<PathBuf>, String> {
    let Some(user) = std::env::var_os("SUDO_USER") else {
        return Ok(None);
    };
    #[cfg(unix)]
    {
        use std::ffi::{CStr, CString};

        let username = user
            .into_string()
            .map_err(|_| "SUDO_USER was not valid UTF-8".to_string())?;
        let name = CString::new(username.clone())
            .map_err(|_| format!("invalid SUDO_USER '{username}'"))?;

        let configured_size = {
            let size = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
            if size > 0 { size as usize } else { 16_384 }
        };
        let mut buffer = vec![0_u8; configured_size];
        let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result = std::ptr::null_mut();

        let status = unsafe {
            libc::getpwnam_r(
                name.as_ptr(),
                &mut passwd,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status != 0 {
            return Err(format!(
                "failed to resolve home directory for '{username}': errno {status}"
            ));
        }
        if result.is_null() {
            return Err(format!("failed to resolve home directory for '{username}'"));
        }
        let home_ptr = passwd.pw_dir;
        if home_ptr.is_null() {
            return Err(format!("home directory missing for '{username}'"));
        }
        let home = unsafe { CStr::from_ptr(home_ptr) };
        Ok(Some(PathBuf::from(home.to_string_lossy().into_owned())))
    }
    #[cfg(not(unix))]
    {
        let _ = user;
        Ok(None)
    }
}

pub(super) fn run_command<const N: usize>(program: &str, args: [&str; N]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("start {program}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    Err(format!("{program} {} failed: {detail}", args.join(" "),))
}

pub(super) fn set_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path)
            .map_err(|error| format!("stat '{}': {error}", path.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .map_err(|error| format!("chmod '{}': {error}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

pub(super) fn nix_like_uid() -> Result<u32, String> {
    #[cfg(unix)]
    {
        Ok(unsafe { libc::geteuid() })
    }
    #[cfg(not(unix))]
    {
        Err("launchd user services require a unix-like system".into())
    }
}
