//! BuildKit itself runs on the host network, so a Dockerfile `ADD` of a URL is
//! fetched from the host. The daemon fences BuildKit's sockets by cgroup, so
//! those fetches reach the internet and the mesh image servers, never the host.

/// What the daemon fences for one build slot's BuildKit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fence {
    /// Build slot; each slot keeps its own firewall chain.
    pub slot: usize,
    /// The BuildKit container's cgroup. Its processes are fenced.
    pub cgroup: CgroupPath,
    /// Loopback ports of this Build's image context proxies.
    pub image_ports: Vec<u16>,
}

/// Installs or replaces a slot's fence. Every BuildKit solve waits on it, and
/// a failure fails the Build.
pub type FenceInstaller = fn(&Fence) -> Result<(), String>;

/// An absolute cgroup v2 path, safe to write into a firewall rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CgroupPath(String);

impl CgroupPath {
    /// Accept an absolute path made only of characters cgroup names use.
    #[must_use]
    pub fn new(path: &str) -> Option<Self> {
        let safe = path.len() > 1
            && path.starts_with('/')
            && !path.ends_with('/')
            && !path.contains("//")
            && path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-@:\\".contains(&b));
        safe.then(|| Self(path.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The container's own cgroup out of `/proc/<pid>/cgroup`. BuildKit moves
/// itself into a child cgroup, so the path is cut at the component naming the
/// container, which every BuildKit process stays under.
pub(crate) fn container_cgroup(proc_cgroup: &str, container_id: &str) -> Option<CgroupPath> {
    let path = proc_cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?;
    let mut end = None;
    let mut offset = 0;
    for component in path.split('/') {
        offset += component.len();
        if !container_id.is_empty() && component.contains(container_id) {
            end = Some(offset);
            break;
        }
        offset += 1;
    }
    CgroupPath::new(&path[..end?])
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "96c1d062d34fe5d72ba15c93099d43a6f11eaa27c0a45d11680a2148e8ed9280";

    #[test]
    fn the_fence_covers_the_whole_container_not_only_buildkits_child_cgroup() {
        let systemd = format!("0::/system.slice/docker-{ID}.scope/init\n");
        assert_eq!(
            container_cgroup(&systemd, ID).unwrap().as_str(),
            format!("/system.slice/docker-{ID}.scope")
        );
        let cgroupfs = format!("0::/docker/{ID}\n");
        assert_eq!(
            container_cgroup(&cgroupfs, ID).unwrap().as_str(),
            format!("/docker/{ID}")
        );
    }

    #[test]
    fn a_process_outside_the_container_is_never_fenced_in_its_place() {
        assert_eq!(container_cgroup("0::/user.slice/session.scope\n", ID), None);
        assert_eq!(container_cgroup(&format!("0::/{ID}\n"), ""), None);
        assert_eq!(container_cgroup("1:name=systemd:/x\n", ID), None);
        assert_eq!(CgroupPath::new("/"), None);
        assert_eq!(CgroupPath::new("/a\" -j ACCEPT"), None);
        assert_eq!(CgroupPath::new("relative/path"), None);
    }
}
