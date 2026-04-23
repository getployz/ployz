use std::sync::Arc;

use crate::error::Error as PortError;

use super::{Mesh, MeshError, Result};

impl Mesh {
    pub(super) async fn attach_ebpf_dataplane(&mut self) -> Result<()> {
        let Some(cn) = self.container_network.as_ref() else {
            return Err(MeshError::Port(PortError::operation(
                "attach_ebpf",
                "container_network not configured".to_string(),
            )));
        };
        let bridge_ifname = cn.resolve_bridge_ifname().await?;

        #[cfg(all(target_os = "linux", feature = "ebpf-native"))]
        {
            let wg_ifname = self.network.ebpf_attachment_ifname(&bridge_ifname);
            let wg_ifindex = resolve_ifindex(&wg_ifname)?;
            let dp = crate::network::ebpf::EbpfDataplane::attach_native(&bridge_ifname)?;
            self.wg_ifindex = wg_ifindex;
            self.dataplane = Some(Arc::new(dp));
        }

        #[cfg(not(all(target_os = "linux", feature = "ebpf-native")))]
        {
            let dp = crate::network::ebpf::EbpfDataplane::attach_container(
                "ployz-networking",
                &bridge_ifname,
                &bridge_ifname,
            )
            .await?;
            self.wg_ifindex = 0;
            self.dataplane = Some(Arc::new(dp));
        }

        Ok(())
    }
}

#[cfg(all(target_os = "linux", feature = "ebpf-native"))]
fn resolve_ifindex(ifname: &str) -> std::result::Result<u32, PortError> {
    let c_name = std::ffi::CString::new(ifname)
        .map_err(|e| PortError::operation("if_nametoindex", e.to_string()))?;
    let idx = unsafe { libc::if_nametoindex(c_name.as_ptr()) };
    if idx == 0 {
        return Err(PortError::operation(
            "if_nametoindex",
            format!("interface {ifname} not found"),
        ));
    }
    Ok(idx)
}
